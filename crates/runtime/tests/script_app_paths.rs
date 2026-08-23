//! Where a script's file paths land once the runtime has built the app.
//!
//! `read_file` and `write_file` used to resolve against the working
//! directory, so an app that saved its state kept it wherever the launcher
//! happened to be started from and read nothing back on the next run from
//! somewhere else. The runtime publishes the app directory and the app id
//! while it builds, and the path builtins resolve through those: a relative
//! path names a file the app ships, and `data_dir()` names the per-app place
//! saved state belongs.
//!
//! The test runs from the crate directory, never from the app directory, so
//! a resolution that fell back to the working directory would miss.

use lumen_core::app::App as EcsApp;
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptHost, ScriptValue};
use lumen_script_rhai::RhaiHost;
use std::path::{Path, PathBuf};

/// An app publishes process-global registries, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const APP_ID: &str = "lumen-script-app-paths-test";

const SOURCE: &str = r#"
fn probe_round_trip() {
    write_file("saved.txt", "from the app");
    read_file("saved.txt")
}

fn probe_data_dir() {
    data_dir()
}

fn probe_data_write() {
    write_file(data_dir() + "/state.txt", "kept")
}
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

/// Build and tick a headless app in `dir` running the rhai script above.
fn app_in(dir: &Path) -> EcsApp {
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
    app
}

/// The string a probe returned.
fn probe(app: &mut EcsApp, fn_name: &str) -> String {
    let mut host = app.world.resource_mut::<RhaiHost>();
    let outcome = host
        .call(fn_name, &[])
        .unwrap_or_else(|e| panic!("`{fn_name}` ran: {e:?}"));
    assert!(outcome.found, "the script defines `{fn_name}`");
    match outcome.ret {
        Some(ScriptValue::Str(s)) => s,
        Some(ScriptValue::Bool(b)) => b.to_string(),
        other => panic!("`{fn_name}` returned {other:?}"),
    }
}

#[test]
fn a_relative_path_names_a_file_in_the_app_directory() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("relative");
    let cwd = std::env::current_dir().expect("cwd");
    assert_ne!(cwd, dir, "the test does not run from the app directory");

    let mut app = app_in(&dir);
    assert_eq!(probe(&mut app, "probe_round_trip"), "from the app");
    assert_eq!(
        std::fs::read_to_string(dir.join("saved.txt")).ok(),
        Some("from the app".to_string()),
        "the write landed beside the app, not beside the launcher"
    );
    assert!(
        !cwd.join("saved.txt").exists(),
        "nothing was written next to the working directory"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_state_gets_a_directory_of_its_own_named_by_the_app_id() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = app_dir("data");
    let mut app = app_in(&dir);

    let data = PathBuf::from(probe(&mut app, "probe_data_dir"));
    assert!(data.is_dir(), "{} was not created", data.display());
    assert!(
        data.ends_with(APP_ID) || data == dir,
        "`[app] id` names the directory: {}",
        data.display()
    );

    assert_eq!(probe(&mut app, "probe_data_write"), "true");
    assert_eq!(
        std::fs::read_to_string(data.join("state.txt")).ok(),
        Some("kept".to_string()),
        "a script writes into the directory it was handed"
    );
    let _ = std::fs::remove_file(data.join("state.txt"));
    if data != dir {
        let _ = std::fs::remove_dir(&data);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
