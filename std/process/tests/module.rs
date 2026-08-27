//! The bundled-module shape, end to end: build the `lumen-process` cdylib and
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

/// The test program the generated apps run, as the absolute path a script
/// names it by. It is built by the ordinary (statically linked) test build,
/// and is a plain program either build can run.
const CHILD: &str = env!("CARGO_BIN_EXE_lumen-process-test-child");

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
                "lumen-process",
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

        let mut process_module = None;
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
                // The lib target is named `lumen_process`, the spelling the
                // bundled probe derives from `lumen-process`.
                if matches!(name, "lumen_process" | "lumen-process") {
                    process_module = Some(path.clone());
                }
                if let Some(dir) = path.parent()
                    && !lib_dirs.contains(&dir.to_path_buf())
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        lib_dirs.push(target_libdir(&triple));

        let process_module = process_module.expect("bundled process module cdylib built");
        let host = host.expect("fixture host binary built");
        assert_eq!(
            process_module.parent(),
            host.parent(),
            "the process module must build into the host's directory for the bundled probe"
        );

        let scratch = target_dir.join("process-apps");
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
        format!("[app]\nid = \"lumen-process-module-{case}\"\n\n[dependencies]\n{dependencies}"),
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
/// function exists in the `process` namespace, and a child runs from end to
/// end with its output and its exit reaching the script's handlers.
#[test]
fn the_bundled_module_runs_a_child_end_to_end() {
    let f = fixtures();
    let dir = write_app(
        f,
        "process-bundled",
        "lumen-process = { bundled = true }\n",
        &format!(
            r#"
fn on_start() {{
    signal("started", "").set(process::start("{CHILD}", ["6", "hello"], "job"));
}}
fn on_process_stdout(tag, line) {{
    let s = signal("out", "");
    s.set(s.get() + tag + "/" + line + ";");
}}
fn on_process_stderr(tag, line) {{ signal("err", "").set(tag + "/" + line); }}
fn on_process_exit(tag, code) {{ signal("exit", "").set(tag + "/" + code); }}
"#
        ),
    );
    let (stdout, stderr) = run_host(f, &dir, 200, "started,out,err,exit");

    assert!(
        stdout.contains("HOST loaded name=lumen-process kind=EngineModule build_id=lumen-engine "),
        "{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("HOST failed"), "{stdout}\n{stderr}");
    for expected in [
        "HOST signal started=true",
        "HOST signal out=job/6;job/hello;",
        "HOST signal err=job/child stderr",
        "HOST signal exit=job/6",
    ] {
        assert!(stdout.contains(expected), "missing `{expected}`:\n{stdout}");
    }
    assert!(
        !stderr.contains("lumen-process:"),
        "nothing was refused: {stderr}"
    );
}

/// A program that is not there is one stderr line from the module and a false
/// the script reads; the app carries on and no event ever arrives under the
/// tag.
#[test]
fn a_program_that_cannot_start_reports_and_leaves_the_app_running() {
    let f = fixtures();
    let dir = write_app(
        f,
        "process-missing",
        "lumen-process = { bundled = true }\n",
        r#"
fn on_start() {
    signal("started", "").set(process::start("no-such-program-8f2c", [], "gone"));
}
fn on_process_exit(tag, code) { signal("exit", "").set(code); }
"#,
    );
    let (stdout, stderr) = run_host(f, &dir, 40, "started,exit");

    assert!(
        stderr.contains("lumen-process: start(no-such-program-8f2c)"),
        "the refusal names the program: {stderr}"
    );
    assert!(stdout.contains("HOST signal started=false"), "{stdout}");
    assert!(stdout.contains("HOST signal exit=<unset>"), "{stdout}");
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
}

/// Without the module the function does not exist: the script's call fails
/// with the host's ordinary unknown-namespace error, the app survives its run,
/// and nothing was started.
#[test]
fn without_the_module_the_function_does_not_exist() {
    let f = fixtures();
    let dir = write_app(
        f,
        "process-absent",
        "",
        &format!(
            r#"fn on_start() {{ signal("started", "").set(process::start("{CHILD}", [], "job")); }}"#
        ),
    );
    let (stdout, stderr) = run_host(f, &dir, 40, "started");

    assert!(stderr.contains("Module not found: process"), "{stderr}");
    assert!(!stdout.contains("HOST tick-panic-caught"), "{stdout}");
    assert!(stdout.contains("HOST signal started=<unset>"), "{stdout}");
    assert!(!stderr.contains("lumen-process:"), "{stderr}");
}
