//! The bundled-module shape, end to end: build the `lumen-canvas` cdylib and
//! the dylib-linked host runner with `-C prefer-dynamic` (the way `lumenc
//! package` builds a Rust app), then drive generated app directories through
//! the host as a subprocess.
//!
//! This is the only place the whole chain runs as it ships: an app declares
//! the module, the loader opens a real shared library, the module's install
//! registers a markup tag before the app's markup is parsed, and a script
//! calls functions that exist only inside that library.
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

/// Where the nested cargo builds: `<target>/module-fixture` normally, and
/// `<coverage target>/debug/module-fixture` under `cargo llvm-cov`.
fn nested_target_dir(root: &Path) -> PathBuf {
    if std::env::var_os("CARGO_LLVM_COV").is_some()
        && let Some(profile_pattern) = std::env::var_os("LLVM_PROFILE_FILE")
        && let Some(coverage_root) = Path::new(&profile_pattern).parent()
    {
        return coverage_root.join("debug").join("module-fixture");
    }
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"))
        .join("module-fixture")
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
        let target_dir = nested_target_dir(&root);
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
                "lumen-canvas",
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

        let mut canvas_module = None;
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
                // The lib target is named `lumen_canvas`, the spelling the
                // bundled probe derives from `lumen-canvas`.
                if matches!(name, "lumen_canvas" | "lumen-canvas") {
                    canvas_module = Some(path.clone());
                }
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let canvas_module = canvas_module.expect("bundled canvas module cdylib built");
        let host = host.expect("fixture host binary built");
        assert_eq!(
            canvas_module.parent(),
            host.parent(),
            "the canvas module must build into the host's directory for the bundled probe"
        );

        let scratch = target_dir.join("canvas-apps");
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        Fixtures {
            host,
            lib_dirs,
            scratch,
        }
    })
}

/// Write an app dir: markup with a rhai script, the script itself, and the
/// given `[dependencies]` block.
fn write_app(f: &Fixtures, case: &str, dependencies: &str, markup: &str, script: &str) -> PathBuf {
    let dir = f.scratch.join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::write(dir.join("src/main.lmn"), markup).expect("markup");
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

/// The value of a printed signal line.
fn signal(stdout: &str, name: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&format!("HOST signal {name}=")))
        .unwrap_or_else(|| panic!("the {name} signal is printed:\n{stdout}"))
        .trim()
        .to_string()
}

/// The whole dynamic path: the module loads through the bundled probe, the
/// tag it brings is accepted, the element it answers for is adopted, and its
/// script functions report the drawing space the markup declared.
#[test]
fn the_bundled_module_supplies_the_canvas_element_and_its_functions() {
    let f = fixtures();
    let dir = write_app(
        f,
        "canvas-bundled",
        "lumen-canvas = { bundled = true, tags = [\"canvas\"] }\n",
        "<root><canvas id=\"chart\" width=\"200\" height=\"120\" />\
         <script src=\"main.rhai\" /></root>\n",
        // The drawing space is the element's, so it is known once the
        // element is mounted: `on_ready`, not `on_start`.
        "fn on_ready() {\n\
         signals.w.set(canvas::width(\"chart\"));\n\
         signals.h.set(canvas::height(\"chart\"));\n\
         canvas::fill_rect(\"chart\", 0.0, 0.0, 10.0, 10.0);\n\
         }\n",
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "w,h");

    assert!(
        stdout.contains("HOST loaded name=lumen-canvas kind=EngineModule build_id=lumen-engine "),
        "{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    assert_eq!(signal(&stdout, "w"), "200");
    assert_eq!(signal(&stdout, "h"), "120");
    // The element matched, so nothing was reported as unanswered.
    assert!(!stderr.contains("no <canvas> element"), "{stderr}");
}

/// The module installs before the app's markup is parsed, so the tag it
/// registers is accepted even when the app never declared it.
///
/// `lumenc build` has no module to ask and does need the declaration, which
/// is why the documentation says to write it either way. This is what the run
/// path does on its own.
#[test]
fn the_run_path_accepts_the_tag_the_module_registers() {
    let f = fixtures();
    let dir = write_app(
        f,
        "canvas-no-tags-key",
        "lumen-canvas = { bundled = true }\n",
        "<root><canvas id=\"chart\" width=\"64\" height=\"64\" />\
         <script src=\"main.rhai\" /></root>\n",
        "fn on_ready() { signals.w.set(canvas::width(\"chart\")); }\n",
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "w");

    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    assert_eq!(
        signal(&stdout, "w"),
        "64",
        "the markup parsed and the element was adopted:\n{stdout}\n{stderr}"
    );
}

/// The frame hook, over a real app: a callback that asks again keeps running,
/// and one that stops asking parks.
///
/// The loop starts from `on_ready`, which is what the documentation tells an
/// author to do and is a handler the frame drain has to be ordered after: a
/// request the drain runs past does not arrive late, it lets an idle app park
/// with the request unread and the animation never starts.
#[test]
fn a_frame_loop_advances_and_then_parks() {
    let f = fixtures();
    let dir = write_app(
        f,
        "canvas-frames",
        "lumen-canvas = { bundled = true, tags = [\"canvas\"] }\n",
        "<root><canvas id=\"chart\" width=\"64\" height=\"64\" />\
         <script src=\"main.rhai\" /></root>\n",
        "fn on_ready() {\n\
         signals.frames.set(0);\n\
         request_frame();\n\
         }\n\
         fn on_frame(dt) {\n\
         let n = signals.frames.get() + 1;\n\
         signals.frames.set(n);\n\
         canvas::fill_rect(\"chart\", n * 1.0, 0.0, 1.0, 1.0);\n\
         if n < 5 { request_frame(); }\n\
         }\n",
    );
    // Far more ticks than frames asked for: the count stops where the script
    // stopped asking, which is what makes an idle animation free.
    let (stdout, stderr) = run_host(f, &dir, 60, "frames");

    assert_eq!(
        signal(&stdout, "frames"),
        "5",
        "the loop ran five frames and then parked:\n{stdout}\n{stderr}"
    );
}

/// Without the module the tag does not exist, so the app refuses to build and
/// the message names the key that would have declared it.
#[test]
fn without_the_module_the_element_is_an_unknown_tag() {
    let f = fixtures();
    let dir = write_app(
        f,
        "canvas-absent",
        "",
        "<root><canvas id=\"chart\" /><script src=\"main.rhai\" /></root>\n",
        "fn on_start() {}\n",
    );
    let joined = std::env::join_paths(&f.lib_dirs).expect("lib dirs join");
    let out = Command::new(&f.host)
        .arg(&dir)
        .arg("5")
        .env("LD_LIBRARY_PATH", &joined)
        .env("DYLD_LIBRARY_PATH", &joined)
        .env("DYLD_FALLBACK_LIBRARY_PATH", &joined)
        .output()
        .expect("host runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the app must not build");
    assert!(stderr.contains("unknown tag 'canvas'"), "{stderr}");
    // The message names the key that would have declared the tag. The host
    // prints it through a `Debug` formatter, so the quotes arrive escaped.
    assert!(stderr.contains("tags = ["), "{stderr}");
}
