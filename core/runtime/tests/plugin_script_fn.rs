//! A plugin registers a script function, and the app's script calls it.
//!
//! The round trip goes plugin -> `ScriptFnRegistry` -> host -> script, on a
//! headless app built the way `lumenc run` builds one. What each case proves is
//! that the plugin phase happens early enough: candela binds its `host`
//! declarations while the program compiles, so a registration that arrived any
//! later would have nothing to bind to.

use std::sync::{Arc, Mutex};

use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptTy, ScriptValue};

// The candela host, compiled in: the artifact names the module that runs its
// program, and a test binary has no shared engine to open it from.
use lumen_candela_dev as _;

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
        // whole path, without the script spelling a signal builtin.
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
                    Ok(ScriptValue::Str(greeting))
                }),
        );
    }
}

/// A plugin that puts its function in a namespace of its own, the way a device
/// or service integration would.
struct GpioPlugin {
    /// candela source the plugin ships with the namespace, or nothing.
    wrapper: Option<&'static str>,
}

impl Plugin for GpioPlugin {
    fn build(self, app: &mut EcsApp) {
        app.add_script_fn(
            ScriptFn::new("level")
                .ns(ScriptNs::Named("gpio".to_string()))
                .param("pin", ScriptTy::Int)
                .build(|cx| {
                    let doubled = cx.int_arg(0) * 2;
                    cx.emit(ScriptCommand::SetSignal {
                        name: "reading".to_string(),
                        value: doubled.to_string(),
                    });
                    Ok(ScriptValue::I64(doubled))
                }),
        );
        if let Some(wrapper) = self.wrapper {
            app.add_script_prelude("candela", "gpio", wrapper);
        }
    }
}

/// A plugin that registers a name the runtime already provides, in the
/// runtime's own `lumen` namespace, to prove the later registration is the one
/// the script reaches.
struct ShadowPlugin;

impl Plugin for ShadowPlugin {
    fn build(self, app: &mut EcsApp) {
        app.add_script_fn(
            ScriptFn::commands("page_current", 0, |cx| {
                cx.emit(ScriptCommand::SetSignal {
                    name: "shadowed".to_string(),
                    value: "yes".to_string(),
                });
            })
            .with_ns(ScriptNs::Builtin),
        );
    }
}

/// Build a headless app running the candela `source`, with `plugin` installed.
fn app_with(source: &str, plugin: impl Plugin + Send + 'static) -> EcsApp {
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
        scripts: vec![CompiledScript {
            engine: "candela".to_string(),
            module: "lumen-candela-dev".to_string(),
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

/// candela resolves a host call through a declared block, and the host writes
/// that block from what the plugin registered, so the app declares nothing.
#[test]
fn a_plugin_function_is_callable_from_a_script() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let app = app_with(
        r#"
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

/// An app that declares the namespace itself keeps working: the host leaves a
/// namespace the source already spells alone, which is what a `.cdl` written
/// before auto-declaration, or an artifact built from one, relies on.
#[test]
fn a_candela_app_may_declare_the_namespace_itself() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let app = app_with(
        r#"
host "native" {
    any greet(...);
}

fn on_start() {
    let msg = native::greet("by hand");
}

fn main() {}
"#,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    assert_eq!(calls.lock().unwrap().as_slice(), ["by hand".to_owned()]);
    assert_eq!(signal(&app, "greeting").as_deref(), Some("hello by hand"));
}

/// A plugin's own namespace is reachable from a script, which calls the
/// function through it.
#[test]
fn a_plugin_namespace_is_callable_from_a_script() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let app = app_with(
        "fn on_start() { let v = gpio::level(21); }\nfn main() {}\n",
        GpioPlugin { wrapper: None },
    );
    assert_eq!(
        signal(&app, "reading").as_deref(),
        Some("42"),
        "the plugin's namespaced function ran"
    );
}

/// A plugin can ship candela sugar over its namespace, so the script calls the
/// method form of what it registered.
#[test]
fn a_candela_plugin_wrapper_offers_the_method_form() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let app = app_with(
        "fn on_start() { let v = pin(21).level(); }\nfn main() {}\n",
        GpioPlugin {
            wrapper: Some(
                r#"
struct Pin { number: int }
fn pin(number) { return Pin { number: number }; }
impl Pin {
    fn level(self) { return gpio::level(self.number); }
}
"#,
            ),
        },
    );

    assert_eq!(signal(&app, "reading").as_deref(), Some("42"));
}

/// The host binds the runtime's own functions first, so a plugin that takes
/// one of their names in the same namespace wins.
///
/// The script declares nothing and imports no prelude, so the `lumen` block it
/// compiles against is the one the host writes from the plugin's registration.
#[test]
fn a_plugin_function_shadows_a_runtime_builtin() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let app = app_with(
        "fn on_start() { lumen::page_current(); }\nfn main() {}\n",
        ShadowPlugin,
    );
    assert_eq!(
        signal(&app, "shadowed").as_deref(),
        Some("yes"),
        "the plugin's `page_current` is the one the script reached"
    );
}

/// Hot reload swaps the program, not the engine, so a registered function is
/// still there for the reloaded script.
#[test]
fn a_plugin_function_survives_a_reload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let mut app = app_with(
        "fn on_start() { let msg = native::greet(\"first\"); }\nfn main() {}\n",
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    {
        // Through the language the host module registered, the way hot
        // reload reaches it.
        let candela = *app
            .world
            .resource::<lumen_script::ScriptLanguages>()
            .get("candela")
            .expect("the candela module registered its language");
        let reload = candela.reload.expect("the source host reloads");
        reload(
            &mut app.world,
            "fn on_start() { let msg = native::greet(\"second\"); }\nfn main() {}\n",
            "reload.cdl",
        )
        .expect("the host is installed")
        .expect("the reloaded script compiles");
        ((candela.access)().call)(&mut app.world, "on_start").expect("on_start runs again");
    }

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_owned(), "second".to_owned()]
    );
}

/// A plugin function that fails is a script error, not a dead app.
///
/// The script raises it the way it raises its own failures, and the tick loop
/// keeps going, so the window an author is looking at stays up. candela
/// catches by kind, so the script reports having caught rather than what it
/// caught.
#[test]
fn a_failing_plugin_function_leaves_the_app_running() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let caught: Calls = Arc::default();
    let mut app = app_with(
        "fn on_start() {\n\
         \x20   try { native::refuse(\"x\"); }\n\
         \x20   catch \"host_fn_error\" { native::noted(\"raised\"); }\n\
         }\n\
         fn main() {}\n",
        RefusingPlugin {
            caught: caught.clone(),
        },
    );
    // A few more ticks: a dead host would stop answering here.
    app.tick();
    app.tick();
    let caught = caught.lock().unwrap();
    assert_eq!(
        caught.as_slice(),
        ["raised".to_owned()],
        "the script caught the failure"
    );
}

/// A plugin whose one function always refuses, plus a second that records what
/// the script caught.
struct RefusingPlugin {
    caught: Calls,
}

impl Plugin for RefusingPlugin {
    fn build(self, app: &mut EcsApp) {
        let caught = self.caught;
        app.add_script_fn(
            ScriptFn::new("refuse")
                .param("what", ScriptTy::Str)
                .ret(ScriptTy::Str)
                .build(|cx| Err(format!("`{}` is not available", cx.str_arg(0)))),
        );
        app.add_script_fn(
            ScriptFn::new("noted")
                .param("message", ScriptTy::Str)
                .ret(ScriptTy::Unit)
                .build(move |cx| {
                    caught.lock().unwrap().push(cx.str_arg(0));
                    Ok(ScriptValue::Unit)
                }),
        );
    }
}
