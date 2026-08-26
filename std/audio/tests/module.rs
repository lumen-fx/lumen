//! The bundled-module shape, end to end: build the `lumen-audio` cdylib and
//! the dylib-linked host runner with `-C prefer-dynamic` (the way `lumenc
//! package` builds a Rust app), then drive generated app directories through
//! the host as a subprocess and assert on the loader record, the signals, and
//! the script events.
//!
//! A normal `cargo test` binary links the engine statically and must never
//! load modules itself, so everything here runs through the subprocess.
//! Builds go to the shared `target/module-fixture` subdirectory with an
//! explicit `--target`, which keeps `prefer-dynamic` off build scripts and
//! proc macros.

#![cfg(not(windows))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use lumen_audio::synth;

struct Fixtures {
    /// The dylib-linked host runner.
    host: PathBuf,
    /// Directories the host and its libraries load from.
    lib_dirs: Vec<PathBuf>,
    /// Scratch root for generated app dirs.
    scratch: PathBuf,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate sits two levels under the workspace root")
        .to_path_buf()
}

fn host_triple() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc).arg("-vV").output().expect("rustc runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("rustc -vV reports a host")
        .to_string()
}

fn target_libdir(triple: &str) -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc)
        .args(["--print", "target-libdir", "--target", triple])
        .output()
        .expect("rustc runs");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// Build the module and the host once, prefer-dynamic, and remember where
/// everything landed. The premise of `bundled = true` is that the module
/// library sits beside the running engine, which here means beside the host
/// executable; both build into one directory.
fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let root = workspace_root();
        let triple = host_triple();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"))
            .join("module-fixture");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let rustflags = {
            let existing = std::env::var("RUSTFLAGS").unwrap_or_default();
            format!("{existing} -C prefer-dynamic").trim().to_string()
        };
        let out = Command::new(cargo)
            .current_dir(&root)
            .args([
                "build",
                "-p",
                "lumen-module-fixture-host",
                "-p",
                "lumen-audio",
                "--message-format=json-render-diagnostics",
            ])
            .arg("--target")
            .arg(&triple)
            .arg("--target-dir")
            .arg(&target_dir)
            .env("RUSTFLAGS", &rustflags)
            .output()
            .expect("cargo runs");
        assert!(
            out.status.success(),
            "fixture build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let mut audio_module = None;
        let mut host = None;
        let mut lib_dirs: Vec<PathBuf> = Vec::new();
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if msg["reason"] != "compiler-artifact" {
                continue;
            }
            let name = msg["target"]["name"].as_str().unwrap_or_default();
            if let Some(exe) = msg["executable"].as_str()
                && matches!(
                    name,
                    "lumen-module-fixture-host" | "lumen_module_fixture_host"
                )
            {
                host = Some(PathBuf::from(exe));
            }
            for f in msg["filenames"].as_array().into_iter().flatten() {
                let path = PathBuf::from(f.as_str().unwrap_or_default());
                let is_lib = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("so" | "dylib")
                );
                if !is_lib {
                    continue;
                }
                // The lib target is named `lumen_audio`, the spelling the
                // bundled probe derives from `lumen-audio`.
                if matches!(name, "lumen_audio" | "lumen-audio") {
                    audio_module = Some(path.clone());
                }
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let audio_module = audio_module.expect("bundled audio module cdylib built");
        let host = host.expect("fixture host binary built");
        assert_eq!(
            audio_module.parent(),
            host.parent(),
            "the audio module must build into the host's directory for the bundled probe"
        );

        let scratch = target_dir.join("audio-apps");
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        Fixtures {
            host,
            lib_dirs,
            scratch,
        }
    })
}

/// Write an app dir: markup with a rhai script, the script itself, a
/// generated wav under `wav_name`, and the given `[dependencies]` block.
fn write_app(f: &Fixtures, case: &str, dependencies: &str, script: &str, wav_secs: f32) -> PathBuf {
    let dir = f.scratch.join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    synth::write_wav(&dir.join("tone.wav"), &synth::sine(440.0, wav_secs)).expect("wav");
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root><label>hello</label><script src=\"main.rhai\" /></root>\n",
    )
    .expect("markup");
    std::fs::write(dir.join("src/main.rhai"), script).expect("script");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[dependencies]\n{dependencies}"),
    )
    .expect("config");
    dir
}

/// Run the host over an app dir and return (stdout, stderr).
fn run_host(f: &Fixtures, app_dir: &Path, ticks: u32, signals: &str) -> (String, String) {
    let joined = std::env::join_paths(&f.lib_dirs).expect("lib dirs join");
    let out = Command::new(&f.host)
        .arg(app_dir)
        .arg(ticks.to_string())
        .env("LD_LIBRARY_PATH", &joined)
        .env("DYLD_LIBRARY_PATH", &joined)
        .env("DYLD_FALLBACK_LIBRARY_PATH", &joined)
        .env("LUMEN_PLUGIN_CACHE", f.scratch.join("no-such-cache"))
        .env("LUMEN_FIXTURE_SIGNALS", signals)
        .output()
        .expect("host runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "host exited non-zero.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("HOST done"), "{stdout}");
    (stdout, stderr)
}

/// The value of a printed signal line, parsed as a float.
fn signal_value(stdout: &str, name: &str) -> f64 {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&format!("HOST signal {name}=")))
        .unwrap_or_else(|| panic!("the {name} signal is printed:\n{stdout}"))
        .trim()
        .parse()
        .unwrap_or(-1.0)
}

/// The whole dynamic path: the module loads through the bundled probe, its
/// script functions exist, the app-relative path resolves against the app
/// dir, the wav's duration decodes, and the position advances.
#[test]
fn the_bundled_module_supplies_the_audio_surface() {
    let f = fixtures();
    let dir = write_app(
        f,
        "audio-bundled",
        "lumen-audio = { bundled = true }\n",
        "fn on_start() { audio_volume(0.0); audio_play(\"tone.wav\"); }\n",
        5.0,
    );
    let (stdout, stderr) = run_host(f, &dir, 250, "audio_duration,audio_playing,audio_position");

    assert!(
        stdout.contains("HOST loaded name=lumen-audio kind=EngineModule build_id=lumen-engine "),
        "{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    // The module's decoder knows the track length; sound itself is not
    // asserted (CI has no output device, the backend runs its documented
    // silent path there).
    let duration = signal_value(&stdout, "audio_duration");
    assert!(
        (4.9..5.1).contains(&duration),
        "expected the wav's ~5s duration, got {duration}\n{stdout}"
    );
    assert!(
        stdout.contains("HOST signal audio_playing=true"),
        "{stdout}"
    );
    let position = signal_value(&stdout, "audio_position");
    assert!(
        position > 0.0,
        "the playhead advanced: {position}\n{stdout}"
    );
}

/// End of track rides the plugin-event bus into the script: the
/// `on_audio_end(path)` fallback fires, and a per-key registration wins.
#[test]
fn end_of_track_reaches_the_script_through_the_event_bus() {
    let f = fixtures();
    let dir = write_app(
        f,
        "audio-end",
        "lumen-audio = { bundled = true }\n",
        r#"
fn on_start() {
    audio_volume(0.0);
    on("audio_end", "tone.wav", "special_end");
    audio_play("tone.wav");
}
fn special_end(path) { signal("special", "").set(path); }
fn on_audio_end(path) { signal("fallback", "").set(path); }
"#,
        0.2,
    );
    let (stdout, stderr) = run_host(f, &dir, 400, "special,fallback");

    assert!(
        stdout.contains("HOST signal special=tone.wav"),
        "the per-key handler fired:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("HOST signal fallback=<unset>"),
        "the per-key registration wins over the fallback:\n{stdout}"
    );
}

/// A path nothing holds is a per-track stderr report from the module, the
/// transport stays idle, and the app keeps running.
#[test]
fn a_missing_track_reports_on_stderr() {
    let f = fixtures();
    let dir = write_app(
        f,
        "audio-missing",
        "lumen-audio = { bundled = true }\n",
        "fn on_start() { audio_play(\"no-such-track.wav\"); }\n",
        0.5,
    );
    let (stdout, stderr) = run_host(f, &dir, 150, "audio_playing,audio_duration");

    assert!(
        stderr.contains("lumen-audio: track failed to load: no-such-track.wav"),
        "{stderr}"
    );
    assert!(
        stdout.contains("HOST signal audio_playing=false"),
        "{stdout}"
    );
    assert_eq!(
        signal_value(&stdout, "audio_duration"),
        0.0,
        "no track, no duration:\n{stdout}"
    );
}

/// Without the module the functions do not exist: the script's call fails
/// with the host's ordinary unknown-function error, the app survives its
/// run, and the engine prints nothing audio-named.
#[test]
fn without_the_module_the_functions_do_not_exist() {
    let f = fixtures();
    let dir = write_app(
        f,
        "audio-absent",
        "",
        "fn on_start() { audio_play(\"tone.wav\"); }\n",
        0.5,
    );
    let (stdout, stderr) = run_host(f, &dir, 50, "audio_duration,audio_playing");

    // The host's own unknown-function error, nothing more.
    assert!(
        stderr.contains("Function not found: audio_play"),
        "{stderr}"
    );
    // The app survived the whole run (`HOST done` is asserted in run_host)
    // and no tick panicked.
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
    // The engine has no audio surface, so no signal exists and nothing
    // audio-named is printed by anything but the script's own call.
    assert!(
        stdout.contains("HOST signal audio_duration=<unset>"),
        "{stdout}"
    );
    assert!(
        stdout.contains("HOST signal audio_playing=<unset>"),
        "{stdout}"
    );
    assert!(!stderr.contains("lumen-audio:"), "{stderr}");
    assert!(!stderr.contains("no audio backend"), "{stderr}");
}
