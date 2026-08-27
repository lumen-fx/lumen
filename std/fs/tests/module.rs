//! The bundled-module shape, end to end: build the `lumen-fs` cdylib and the
//! dylib-linked host runner with `-C prefer-dynamic` (the way `lumenc
//! package` builds a Rust app), then drive generated app directories through
//! the host as a subprocess and assert on the loader record and the signals.
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
/// `<coverage target>/debug/module-fixture` under `cargo llvm-cov`. The
/// nested build is instrumented by the inherited rustc wrapper and the host
/// subprocess writes its counters through the inherited `LLVM_PROFILE_FILE`,
/// whose directory is the coverage target dir; the report step walks that
/// tree's `debug` profile directory for the objects that map the counters.
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
                "lumen-fs",
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

        let mut fs_module = None;
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
                // The lib target is named `lumen_fs`, the spelling the bundled
                // probe derives from `lumen-fs`.
                if matches!(name, "lumen_fs" | "lumen-fs") {
                    fs_module = Some(path.clone());
                }
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let fs_module = fs_module.expect("bundled fs module cdylib built");
        let host = host.expect("fixture host binary built");
        assert_eq!(
            fs_module.parent(),
            host.parent(),
            "the fs module must build into the host's directory for the bundled probe"
        );

        let scratch = target_dir.join("fs-apps");
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
fn write_app(f: &Fixtures, case: &str, dependencies: &str, script: &str) -> PathBuf {
    let dir = f.scratch.join(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("app dir");
    std::fs::write(
        dir.join("src/main.lmn"),
        "<root><label>hello</label><script src=\"main.rhai\" /></root>\n",
    )
    .expect("markup");
    std::fs::write(dir.join("src/main.rhai"), script).expect("script");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"lumen-fs-module-{case}\"\n\n[dependencies]\n{dependencies}"),
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

/// The whole dynamic path: the module loads through the bundled probe, its
/// functions exist in the `files` namespace, and every path the script wrote
/// resolved against the app directory.
#[test]
fn the_bundled_module_supplies_the_file_surface() {
    let f = fixtures();
    let dir = write_app(
        f,
        "fs-bundled",
        "lumen-fs = { bundled = true }\n",
        r#"
fn on_start() {
    signal("wrote", "").set(files::write("notes.txt", "saved"));
    signal("read", "").set(files::read("notes.txt"));
    signal("exists", "").set(files::exists("notes.txt"));
    signal("made", "").set(files::mkdir("sub"));
    signal("copied", "").set(files::copy("notes.txt", "sub/copy.txt"));
    signal("listed", "").set(files::list("sub")[0]);
    signal("bytes", "").set(files::read_bytes("notes.txt").len());
    signal("removed", "").set(files::remove("sub/copy.txt"));
}
"#,
    );
    let (stdout, stderr) = run_host(
        f,
        &dir,
        20,
        "wrote,read,exists,made,copied,listed,bytes,removed",
    );

    assert!(
        stdout.contains("HOST loaded name=lumen-fs kind=EngineModule build_id=lumen-engine "),
        "{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    for expected in [
        "HOST signal wrote=true",
        "HOST signal read=saved",
        "HOST signal exists=true",
        "HOST signal made=true",
        "HOST signal copied=true",
        "HOST signal listed=copy.txt",
        "HOST signal bytes=5",
        "HOST signal removed=true",
    ] {
        assert!(stdout.contains(expected), "missing `{expected}`:\n{stdout}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).ok(),
        Some("saved".to_string()),
        "a relative path named a file beside the app"
    );
    assert!(
        !stderr.contains("lumen-fs:"),
        "nothing was refused: {stderr}"
    );
}

/// A refusal is one stderr line from the module and the app carries on: the
/// script reads the false it got back and keeps running.
#[test]
fn a_refusal_reports_on_stderr_and_leaves_the_app_running() {
    let f = fixtures();
    let dir = write_app(
        f,
        "fs-refused",
        "lumen-fs = { bundled = true }\n",
        r#"
fn on_start() {
    files::mkdir("full");
    files::write("full/kept.txt", "kept");
    signal("removed", "").set(files::remove("full"));
    signal("still_there", "").set(files::exists("full/kept.txt"));
}
"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "removed,still_there");

    assert!(
        stderr.contains("lumen-fs: remove(") && stderr.contains("full"),
        "the refusal names the path: {stderr}"
    );
    assert!(stdout.contains("HOST signal removed=false"), "{stdout}");
    assert!(stdout.contains("HOST signal still_there=true"), "{stdout}");
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
}

/// The `read_bytes_cap` an app sets in the module's `config` table is what the
/// loaded module enforces.
#[test]
fn the_config_table_sets_the_byte_cap() {
    let f = fixtures();
    let dir = write_app(
        f,
        "fs-cap",
        "lumen-fs = { bundled = true, config = { read_bytes_cap = 1024 } }\n",
        r#"
fn on_start() {
    let big = "";
    while big.len < 1100 { big += "0123456789"; }
    files::write("big.txt", big);
    files::write("small.txt", "tiny");
    signal("big", "").set(files::read_bytes("big.txt").len());
    signal("small", "").set(files::read_bytes("small.txt").len());
}
"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "big,small");

    assert!(
        stdout.contains("HOST signal big=0"),
        "a file past the configured cap reads as no bytes:\n{stdout}"
    );
    assert!(stdout.contains("HOST signal small=4"), "{stdout}");
    assert!(
        stderr.contains("read_bytes_cap"),
        "the refusal names the setting to raise: {stderr}"
    );
}

/// Without the module the functions do not exist: the script's call fails
/// with the host's ordinary unknown-namespace error, the app survives its
/// run, and nothing is written.
#[test]
fn without_the_module_the_functions_do_not_exist() {
    let f = fixtures();
    let dir = write_app(
        f,
        "fs-absent",
        "",
        r#"fn on_start() { signal("read", "").set(files::read("notes.txt")); }"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "read");

    assert!(stderr.contains("Module not found: files"), "{stderr}");
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
    assert!(stdout.contains("HOST signal read=<unset>"), "{stdout}");
    assert!(!stderr.contains("lumen-fs:"), "{stderr}");
}
