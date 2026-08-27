//! The bundled-module shape, end to end: build the `lumen-archive` cdylib and
//! the dylib-linked host runner with `-C prefer-dynamic` (the way `lumenc
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

use lumen_archive::testkit;

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
                "lumen-archive",
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

        let mut archive_module = None;
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
                // The lib target is named `lumen_archive`, the spelling the
                // bundled probe derives from `lumen-archive`.
                if matches!(name, "lumen_archive" | "lumen-archive") {
                    archive_module = Some(path.clone());
                }
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let archive_module = archive_module.expect("bundled archive module cdylib built");
        let host = host.expect("fixture host binary built");
        assert_eq!(
            archive_module.parent(),
            host.parent(),
            "the archive module must build into the host's directory for the bundled probe"
        );

        let scratch = target_dir.join("archive-apps");
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        Fixtures {
            host,
            lib_dirs,
            scratch,
        }
    })
}

/// Write an app dir: markup with a rhai script, the script itself, an archive
/// to unpack, and the given `[dependencies]` block.
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
        format!("[app]\nid = \"lumen-archive-module-{case}\"\n\n[dependencies]\n{dependencies}"),
    )
    .expect("config");
    testkit::normal_zip(&dir.join("bundle.zip")).expect("archive fixture");
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
/// function exists in the `archive` namespace, the archive lands beside the
/// app, and the done event reaches the script with the count.
#[test]
fn the_bundled_module_unpacks_an_archive() {
    let f = fixtures();
    let dir = write_app(
        f,
        "archive-bundled",
        "lumen-archive = { bundled = true }\n",
        r#"
fn on_start() {
    signal("taken", "").set(archive::extract("bundle.zip", "out", "bundle"));
}
fn on_archive_done(tag, dest, count) {
    signal("done", "").set(tag);
    signal("count", "").set(count);
}
fn on_archive_error(tag, message) { signal("failed", "").set(message); }
"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 60, "taken,done,count,failed");

    assert!(
        stdout.contains("HOST loaded name=lumen-archive kind=EngineModule build_id=lumen-engine "),
        "{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    for expected in [
        "HOST signal taken=true",
        "HOST signal done=bundle",
        "HOST signal count=3",
        "HOST signal failed=<unset>",
    ] {
        assert!(stdout.contains(expected), "missing `{expected}`:\n{stdout}");
    }
    for (member, body) in testkit::MEMBERS {
        assert_eq!(
            std::fs::read_to_string(dir.join("out").join(member)).ok(),
            Some(body.to_string()),
            "a relative destination named a directory beside the app: {member}"
        );
    }
}

/// Without the module the function does not exist: the script's call fails
/// with the host's ordinary unknown-namespace error, the app survives its
/// run, and nothing is unpacked.
#[test]
fn without_the_module_the_function_does_not_exist() {
    let f = fixtures();
    let dir = write_app(
        f,
        "archive-absent",
        "",
        r#"fn on_start() { signal("taken", "").set(archive::extract("bundle.zip", "out", "bundle")); }"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 20, "taken");

    assert!(stderr.contains("Module not found: archive"), "{stderr}");
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
    assert!(stdout.contains("HOST signal taken=<unset>"), "{stdout}");
    assert!(!dir.join("out").exists(), "nothing was unpacked");
}
