// `lumenc run --headless` and `app_kind` come from the linked runtime, so the
// whole file compiles out of a thin (`--no-default-features`) build the same
// way the other runtime-backed suites do.
//
// Linux only. The stderr policy below is an allow-list of the lines a clean
// run prints in this environment, and that set is what makes the check sharp
// enough to catch a missing asset. Calibrating one list against three
// environments means loosening it until it stops catching anything. The app
// sources under test are identical on every platform; the layer beneath them
// is what the three-OS matrix covers.
#![cfg(all(feature = "dev-run", target_os = "linux"))]

//! Every app the repo ships runs, and so does every app `lumenc new`
//! scaffolds.
//!
//! The rest of the suite drives the pipeline through fixtures it writes
//! itself, which leaves the shipped apps and the scaffold templates
//! unexercised end to end: their markup compiles in `check.rs`, but nothing
//! spawns them, mounts them, and ticks them with their script attached.
//!
//! A run that ends in success is not enough on its own. When a script fails
//! to compile the runtime prints a banner and carries on with every handler
//! disabled, so the process still exits zero with a window's worth of dead
//! app behind it. The same goes for an asset an app names but does not
//! ship. Both land on stderr, so a run counts as clean only when its stderr
//! carries nothing but the lines listed in [`ALLOWED_STDERR`].

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lumenc::app_kind::{AppKind, detect};

/// Ticks each app runs for. `on_ready` dispatches after the first mount, so
/// this has to be past it; a handful more covers the timers and derivations
/// that only settle a frame or two later.
const TICKS: &str = "6";

/// Every stderr line a clean headless run is allowed to print.
///
/// Most of these are the environment talking: a runner has no display, so
/// the clipboard and global-hotkey backends report
/// themselves absent and the runner
/// announces the mode it was put in. `[script]` is an app's own `print`.
/// `info` and its `hint:` continuation are lint findings, which are style
/// nudges rather than defects.
///
/// A backend reporting itself absent is the environment, not the app. A
/// backend reporting a failure while it is present is the app, and there is
/// no entry for that.
///
/// A line outside this set fails the run it appeared in. Anything the
/// runtime prints while an app is starting means the app got less than it
/// asked for.
const ALLOWED_STDERR: &[&str] = &[
    "lumenc: headless mode - no window;",
    "lumenc: no clipboard backend; clipboard builtins are inert",
    "lumen-os-hotkey: no X11 display",
    "[script] ",
    "info  ",
    "      hint: ",
    // This test binary compiles the engine in, so a runtime module an app
    // declares (the music app's `lumen-audio`) cannot load here: the loader
    // says so once and the app boots without it. The line is the build shape
    // talking, not the app; the dynamic e2e suites cover the module-loaded
    // run. The app's own boot path never touches a module function, so this
    // notice is the only thing the skip may print.
    "lumen-runtime: dependency 'lumen-audio' skipped: this build compiles the engine in",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/lumenc`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/lumenc sits two levels below the workspace root")
        .to_path_buf()
}

/// App directories git tracks directly under `parent`.
///
/// Reading the directory instead would pick up whatever a working tree has
/// lying around, so ask git. A tree with no git to ask fails here rather
/// than quietly testing nothing.
fn tracked_app_dirs(parent: &str) -> Vec<PathBuf> {
    let root = workspace_root();
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "--full-name", parent])
        .output()
        .unwrap_or_else(|e| panic!("ask git which apps are tracked under {parent}/: {e}"));
    assert!(
        out.status.success(),
        "git ls-files {parent}/ failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listing = String::from_utf8_lossy(&out.stdout);
    let mut dirs: Vec<PathBuf> = listing
        .lines()
        .filter_map(|p| p.split('/').nth(1))
        .map(|name| root.join(parent).join(name))
        .collect();
    dirs.sort();
    dirs.dedup();
    assert!(!dirs.is_empty(), "git tracks no app under {parent}/");
    dirs
}

/// Run `dir` headless for [`TICKS`] ticks with no display to reach for.
fn run_headless(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .arg("run")
        .arg(dir)
        .args(["--headless", "--ticks", TICKS])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", dir.display()))
}

/// The app exits clean and says nothing on stderr beyond [`ALLOWED_STDERR`].
fn run_clean(dir: &Path, label: &str) -> Result<(), String> {
    let out = run_headless(dir);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(format!(
            "{label} exits {}\n{}",
            out.status,
            stderr.trim_end()
        ));
    }

    let unexpected: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !ALLOWED_STDERR.iter().any(|ok| line.starts_with(ok)))
        .collect();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!("{label}\n{}", unexpected.join("\n")))
    }
}

/// Fail with every app that did not come up clean, so one run names them all.
fn report(failures: Vec<String>, what: &str) {
    assert!(
        failures.is_empty(),
        "{} {what} start with less than they asked for:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The demo apps a reader opens first, driven the way a reader drives them.
#[test]
fn every_example_app_runs_clean() {
    let mut failures = Vec::new();
    for dir in tracked_app_dirs("apps") {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        // A native-SDK app has no markup runner to drive; the one that
        // ships gets its own test below.
        if detect(&dir) != AppKind::Markup {
            continue;
        }
        if let Err(e) = run_clean(&dir, &format!("apps/{name}")) {
            failures.push(e);
        }
    }
    report(failures, "example apps");
}

/// The fixtures the rest of the suite loads in-process. A test that reads
/// the world back cannot see the stderr a subprocess prints, so a fixture
/// whose script died still satisfies its own assertions.
#[test]
fn every_fixture_app_runs_clean() {
    let mut failures = Vec::new();
    for dir in tracked_app_dirs("fixtures") {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if let Err(e) = run_clean(&dir, &format!("fixtures/{name}")) {
            failures.push(e);
        }
    }
    report(failures, "fixture apps");
}

/// Every template `lumenc new` offers, scaffolded through the CLI and run.
///
/// The names come from the gallery the CLI itself reads, so a template added
/// to it is covered the moment it lands. Scaffolding goes through `lumenc
/// new` rather than the files behind it: what a user gets is whatever the
/// command put on disk.
///
/// Those files are downloaded rather than kept in the repository
/// (`tools/fetch-templates.sh`), so a checkout that has not run the script has
/// nothing to scaffold and this says so. CI fetches before it tests.
#[test]
fn every_template_runs_clean() {
    if let Err(why) = lumenc::scaffold::payload_dir() {
        eprintln!("skipping: {why}");
        return;
    }
    let workdir = std::env::temp_dir().join(format!("lumenc_template_run_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("create the scaffold workdir");

    let mut failures = Vec::new();
    for template in lumenc::scaffold::TEMPLATES {
        let out = Command::new(env!("CARGO_BIN_EXE_lumenc"))
            .current_dir(&workdir)
            .args(["new", template.name, template.name])
            .output()
            .unwrap_or_else(|e| panic!("scaffold `{}`: {e}", template.name));
        assert!(
            out.status.success(),
            "`lumenc new {0} {0}` failed:\n{1}",
            template.name,
            String::from_utf8_lossy(&out.stderr)
        );

        if let Err(e) = run_clean(
            &workdir.join(template.name),
            &format!("template `{}`", template.name),
        ) {
            failures.push(e);
        }
    }

    let _ = std::fs::remove_dir_all(&workdir);
    report(failures, "scaffold templates");
}

/// A native-SDK app builds and runs through its own toolchain, so the
/// markup runner turns it away instead of half-running it. `apps/sysmon` is
/// a CMake project against `liblumen`.
#[test]
fn the_markup_runner_refuses_a_native_app() {
    let dir = workspace_root().join("apps").join("sysmon");
    assert_eq!(detect(&dir), AppKind::Cpp, "apps/sysmon is the C++ example");

    let out = run_headless(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "the markup runner has nothing to drive here: {stderr}"
    );
    assert!(
        stderr.contains("markup apps") && stderr.contains("native toolchain"),
        "the refusal says which toolchain owns the app: {stderr}"
    );
}
