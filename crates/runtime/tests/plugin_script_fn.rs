//! A plugin registers a script function, and the app's script calls it.
//!
//! The round trip goes plugin -> `ScriptFnRegistry` -> host -> script, once per
//! language, on a headless app built the way `lumenc run` builds one. What each
//! case proves is that the plugin phase happens early enough: candela binds its
//! `host` declarations while the program compiles, so a registration that
//! arrived any later would have nothing to bind to.

use std::sync::{Arc, Mutex};

use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptTy, ScriptValue};

/// Nav, the DOM snapshot, and the property store are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the registered function was called with, readable from the test after
/// the app has ticked.
type Calls = Arc<Mutex<Vec<String>>>;

/// A plugin whose whole job is to expose one function to the app's script.
struct GreeterPlugin {
    calls: Calls,
}

impl Plugin for GreeterPlugin {
    fn build(self, app: &mut EcsApp) {
        let calls = self.calls;
        // The greeting rides back as a signal so one assertion covers the
        // whole path in every language, without each script spelling its own
        // host's signal builtin.
        app.add_script_fn(
            ScriptFn::new("greet")
                .param("who", ScriptTy::Str)
                .build(move |cx| {
                    let who = cx.str_arg(0);
                    calls.lock().unwrap().push(who.clone());
                    let greeting = format!("hello {who}");
                    cx.emit(ScriptCommand::SetSignal {
                        name: "greeting".to_string(),
                        value: greeting.clone(),
                    });
                    ScriptValue::Str(greeting)
                }),
        );
    }
}

/// A plugin that registers a name the runtime already provides, to prove the
/// later registration is the one the script reaches.
struct ShadowPlugin;

impl Plugin for ShadowPlugin {
    fn build(self, app: &mut EcsApp) {
        app.add_script_fn(ScriptFn::commands("page_current", 0, |cx| {
            cx.emit(ScriptCommand::SetSignal {
                name: "shadowed".to_string(),
                value: "yes".to_string(),
            });
        }));
    }
}

/// Build a headless app from a script in `engine`, with `plugin` installed.
fn app_with(engine: &str, source: &str, plugin: impl Plugin + Send + 'static) -> EcsApp {
    let dir = std::env::temp_dir().join(format!(
        "lumen_plugin_script_fn_{}_{}",
        std::process::id(),
        {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).expect("temp app dir");
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
            engine: engine.to_string(),
            source: source.to_string(),
            bytecode: None,
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_plugin(plugin);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    // Two ticks: `on_start`'s commands are re-stashed into the host sink and
    // drained on the first, and the applier commits them during it.
    app.tick();
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
fn a_plugin_function_is_callable_from_rhai() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let app = app_with(
        "rhai",
        r#"fn on_start() { greet("rhai"); }"#,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    assert_eq!(calls.lock().unwrap().as_slice(), ["rhai".to_owned()]);
    assert_eq!(signal(&app, "greeting").as_deref(), Some("hello rhai"));
}

#[test]
fn a_plugin_function_is_callable_from_lua() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let app = app_with(
        "lua",
        r#"function on_start() greet("lua") end"#,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    assert_eq!(calls.lock().unwrap().as_slice(), ["lua".to_owned()]);
    assert_eq!(signal(&app, "greeting").as_deref(), Some("hello lua"));
}

/// candela resolves a host call through a declared block, so the script says
/// what it calls. Auto-declaring the registered set comes later.
#[test]
fn a_plugin_function_is_callable_from_candela() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let app = app_with(
        "candela",
        r#"
host "native" {
    any greet(...);
}

fn on_start() {
    let msg = native::greet("candela");
}

fn main() {}
"#,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    assert_eq!(calls.lock().unwrap().as_slice(), ["candela".to_owned()]);
    assert_eq!(signal(&app, "greeting").as_deref(), Some("hello candela"));
}

/// The runtime's own functions go into the registry first, so a plugin that
/// takes one of their names wins.
#[test]
fn a_plugin_function_shadows_a_runtime_builtin() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (engine, source) in [
        ("rhai", "fn on_start() { page_current(); }"),
        ("lua", "function on_start() page_current() end"),
    ] {
        let app = app_with(engine, source, ShadowPlugin);
        assert_eq!(
            signal(&app, "shadowed").as_deref(),
            Some("yes"),
            "{engine}: the plugin's `page_current` is the one the script reached"
        );
    }
}

/// Hot reload swaps the program, not the engine, so a registered function is
/// still there for the reloaded script.
#[test]
fn a_plugin_function_survives_a_reload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let mut app = app_with(
        "rhai",
        r#"fn on_start() { greet("first"); }"#,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    {
        use lumen_script::ScriptHost;
        let mut host = app.world.resource_mut::<lumen_script_rhai::RhaiHost>();
        host.replace(r#"fn on_start() { greet("second"); }"#, "reload.rhai")
            .expect("the reloaded script compiles");
        host.call("on_start", &[]).expect("on_start runs again");
    }

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_owned(), "second".to_owned()]
    );
}
