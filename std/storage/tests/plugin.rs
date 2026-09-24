//! The compiled-in shape: [`StoragePlugin`] installed on a headless app, the
//! way `lumenc run` installs a declared module, driven from candela.
//!
//! What this proves: the local set outlives the app that wrote it and the
//! session set does not, which is the whole difference between the two.

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_storage::StoragePlugin;

/// The app directory, the DOM snapshot, and the property store are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-storage-plugin-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("lumen.toml");
    dir
}

/// Build a headless app in `dir` running `source`, with the plugin keeping
/// its file at `file`, and run one tick.
fn run(dir: &std::path::Path, file: &std::path::Path, source: &str) -> EcsApp {
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        script_source: source.to_string(),
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(dir)
        .with_artifact_bytes(bytes)
        .with_plugin(StoragePlugin::at(file));
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.tick();
    app
}

fn signal(app: &EcsApp, name: &str) -> Option<String> {
    match app
        .world
        .resource::<PropertyStore>()
        .get(&PropertyKey::global(name))
    {
        Some(PropertyValue::Str(s)) => Some(s.to_string()),
        other => other.map(|v| format!("{v:?}")),
    }
}

#[test]
fn the_local_set_outlives_the_run_and_the_session_set_does_not() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("restart");
    let file = dir.join("data").join("storage.json");

    let first = run(
        &dir,
        &file,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set_bool("set", storage::set_item("greeting", "hi"));
    storage::set_item("gone", "soon");
    storage::remove_item("gone");
    storage::session_set_item("visit", "first");
    lumen::signal_set("session", str(storage::session_get_item("visit")));
    lumen::signal_set("missing", str(storage::get_item("never")));
}

fn main() {}
"#,
    );
    assert_eq!(signal(&first, "set").as_deref(), Some("true"));
    assert_eq!(signal(&first, "session").as_deref(), Some("first"));
    assert_eq!(signal(&first, "missing").as_deref(), Some("null"));
    drop(first);
    let written = std::fs::read_to_string(&file).expect("the file was written");
    assert!(written.contains("\"greeting\": \"hi\""), "{written}");
    assert!(!written.contains("gone"), "{written}");

    let second = run(
        &dir,
        &file,
        r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_set("local", str(storage::get_item("greeting")));
    lumen::signal_set("keys", str(storage::keys()));
    lumen::signal_set("session", str(storage::session_get_item("visit")));
    storage::clear();
    lumen::signal_set("cleared", str(storage::keys()));
}

fn main() {}
"#,
    );
    assert_eq!(signal(&second, "local").as_deref(), Some("hi"));
    assert_eq!(signal(&second, "keys").as_deref(), Some("[\"greeting\"]"));
    assert_eq!(signal(&second, "session").as_deref(), Some("null"));
    assert_eq!(signal(&second, "cleared").as_deref(), Some("[]"));
    let _ = std::fs::remove_dir_all(&dir);
}
