//! A portable plugin's events, end to end: the fixture cdylib is declared
//! under `[dependencies]`, its pushes cross the core event bus, and the
//! script pipeline delivers them - the fallback handler, a per-key
//! registration that wins over it, and a command batch that mutates the
//! app's state with no handler at all.

#![cfg(all(feature = "modules", not(windows)))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lumen_core::app::App as EcsApp;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// Nav, the DOM snapshot, and the property and event buses are
/// process-global, so the headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build a headless app running `source` under rhai, with the portable
/// fixture plugin declared in the app's `lumen.toml` with `config`.
fn app_with_plugin(tag: &str, config: &str, source: &str) -> EcsApp {
    let dir =
        std::env::temp_dir().join(format!("lumen_plugin_events_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp app dir");
    let plugin: PathBuf = lumen_plugin::testing::fixture_copy(&format!("events-{tag}"));
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[dependencies]\nlumen-plugin-fixture = {{ path = \"{}\", config = {{ {config} }} }}\n",
            plugin.display()
        ),
    )
    .expect("lumen.toml");
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
            engine: "rhai".to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (app, _window) = build_headless_app(opts).expect("build headless app");
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

/// Tick until `name` holds a value or the timeout passes; events pushed from
/// a plugin's own thread race the tick loop, so the test drives it the way a
/// wake-driven backend would.
fn tick_until_signal(app: &mut EcsApp, name: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        app.tick();
        if let Some(value) = signal(app, name) {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_threaded_event_reaches_the_fallback_handler() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // The fixture spawns a thread at init that pushes one Call event with
    // key "thread0", fallback "on_any"; the app defines no `on_fixture`, so
    // the fallback fires with the key as its first argument.
    let mut app = app_with_plugin(
        "thread",
        "thread_events = 1",
        r#"fn on_any(key, n) { signal("plugin_event", "").set(key + ":" + n); }"#,
    );
    assert_eq!(
        tick_until_signal(&mut app, "plugin_event", Duration::from_secs(10)).as_deref(),
        Some("thread0:0"),
        "the plugin's thread event never reached the script"
    );
}

#[test]
fn a_per_key_registration_wins_over_the_fallback() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // `fixture_event(key)` pushes on_fixture/on_any with `key`; the script
    // registers a per-key handler for one key, so that key routes to it and
    // any other key still lands in the fallback.
    let mut app = app_with_plugin(
        "per-key",
        "",
        r#"
fn on_start() {
    on("on_fixture", "special", "on_special");
    fixture_event("special");
    fixture_event("plain");
}
fn on_special(key, arg) { signal("special_route", "").set("per-key:" + key); }
fn on_any(key, arg) { signal("fallback_route", "").set("fallback:" + key); }
"#,
    );
    assert_eq!(
        tick_until_signal(&mut app, "special_route", Duration::from_secs(10)).as_deref(),
        Some("per-key:special")
    );
    assert_eq!(
        tick_until_signal(&mut app, "fallback_route", Duration::from_secs(10)).as_deref(),
        Some("fallback:plain")
    );
}

#[test]
fn a_command_batch_mutates_the_app_without_a_handler() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // `fixture_push_signal` pushes a PluginEvent::Commands batch; the
    // collect system puts it straight on the command bus and the applier
    // commits it - no handler is defined anywhere in the script.
    let mut app = app_with_plugin(
        "commands",
        "",
        r#"fn on_start() { fixture_push_signal("from_plugin", "yes"); }"#,
    );
    assert_eq!(
        tick_until_signal(&mut app, "from_plugin", Duration::from_secs(10)).as_deref(),
        Some("yes")
    );
}
