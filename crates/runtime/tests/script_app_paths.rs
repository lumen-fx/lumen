//! Where an app's file paths land once the runtime has built it.
//!
//! Paths used to resolve against the working directory, so an app that saved
//! its state kept it wherever the launcher happened to be started from and
//! read nothing back on the next run from somewhere else. The runtime
//! publishes the app directory and the app id while it builds, and everything
//! that names a file resolves through those: a relative path names a file the
//! app ships, and the data directory names the per-app place saved state
//! belongs.
//!
//! The reader of those values is the `lumen-fs` module, whose own suite
//! drives them from a script. This one covers the publishing side, which is
//! the runtime's own and runs on every platform, module or no module.
//!
//! The test runs from the crate directory, never from the app directory, so
//! a resolution that fell back to the working directory would miss.

use lumen_core::app_paths;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use std::path::{Path, PathBuf};

/// An app publishes process-global registries, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const APP_ID: &str = "lumen-script-app-paths-test";

const SOURCE: &str = r#"
fn on_start() { signal("started", "").set("yes"); }
"#;

/// A temp app directory carrying `lumen.toml` with the id under test.
fn app_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_app_paths_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!("[app]\nid = \"{APP_ID}\"\n"),
    )
    .expect("write lumen.toml");
    dir
}

/// Build and tick a headless app in `dir` running the script above.
fn build_app_in(dir: &Path) {
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        script_source: SOURCE.to_string(),
        scripts: vec![CompiledScript {
            engine: "rhai".to_string(),
            source: SOURCE.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
}

#[test]
fn building_an_app_publishes_the_directory_relative_paths_resolve_against() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("relative");
    let cwd = std::env::current_dir().expect("cwd");
    assert_ne!(cwd, dir, "the test does not run from the app directory");

    build_app_in(&dir);

    assert_eq!(app_paths::app_dir(), dir, "the app directory is published");
    assert_eq!(
        app_paths::resolve("saved.txt"),
        dir.join("saved.txt"),
        "a relative path names a file beside the app, not beside the launcher"
    );
    let absolute = dir.join("elsewhere.txt");
    assert_eq!(
        app_paths::resolve(&absolute),
        absolute,
        "an absolute path is left alone"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_state_gets_a_directory_of_its_own_named_by_the_app_id() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("data");
    build_app_in(&dir);

    assert_eq!(app_paths::app_id(), APP_ID, "`[app] id` is published");
    let data = app_paths::data_dir();
    assert!(data.is_dir(), "{} was not created", data.display());
    assert!(
        data.ends_with(APP_ID) || data == dir,
        "`[app] id` names the directory: {}",
        data.display()
    );

    if data != dir {
        let _ = std::fs::remove_dir(&data);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
