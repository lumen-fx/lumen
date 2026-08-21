//! A plugin's script function outlives the events that rebuild a host.
//!
//! `plugin_script_fn.rs` proves the first load binds what a plugin registered.
//! What these cases prove is that the binding survives what happens after it: a
//! hot reload, a reset, and a second language loading into an app whose
//! registration channel the first language already sealed. Each is a point
//! where a host throws away state; the Lua host rebuilds its whole engine on
//! reset, and the candela host recompiles the app's source from scratch on
//! reload.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lumen_core::app::{App as EcsApp, Plugin};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{
    ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptFnRegistry, ScriptHost, ScriptNs, ScriptTy,
    ScriptValue,
};

/// Nav, the DOM snapshot, and the property store are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The languages every case runs against, in the order the runtime's own
/// engine table lists them.
const LANGUAGES: [&str; 3] = ["rhai", "lua", "candela"];

/// What the registered function was called with, readable from the test.
type Calls = Arc<Mutex<Vec<String>>>;

/// A plugin whose function records its argument and rides the greeting back as
/// a signal.
struct GreeterPlugin {
    calls: Calls,
}

impl Plugin for GreeterPlugin {
    fn build(self, app: &mut EcsApp) {
        let calls = self.calls;
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

/// A plugin with a namespace of its own and candela sugar over it. `pins`
/// records the pin every call named, so the wrapper's effect is readable
/// without going through a signal.
struct GpioPlugin {
    pins: Calls,
    wrapper: &'static str,
}

impl Plugin for GpioPlugin {
    fn build(self, app: &mut EcsApp) {
        let pins = self.pins;
        app.add_script_fn(
            ScriptFn::new("level")
                .ns(ScriptNs::Named("gpio".to_string()))
                .param("pin", ScriptTy::Int)
                .build(move |cx| {
                    let pin = cx.int_arg(0);
                    pins.lock().unwrap().push(pin.to_string());
                    ScriptValue::I64(pin * 2)
                }),
        );
        app.add_script_prelude("candela", "gpio", self.wrapper);
    }
}

/// A temp directory name no other app in this process takes.
fn scratch_dir() -> std::path::PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumen_script_fn_lifecycle_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("temp app dir");
    dir
}

/// Build a headless app from one `(engine, source)` pair per script, with
/// `plugin` installed.
///
/// The artifact carries the language split the AOT compiler recorded, which is
/// the only way a compiled app gets more than one host: nothing rescans the
/// directory for `.lua` / `.rhai` files once the sources are compiled in.
fn app_with_scripts(scripts: &[(&str, &str)], plugin: impl Plugin + Send + 'static) -> EcsApp {
    let dir = scratch_dir();
    let bytes = artifact::serialize(&CompiledApp {
        ir: LayoutIR {
            root: Element {
                tag: "root".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        scripts: scripts
            .iter()
            .map(|(engine, source)| CompiledScript {
                engine: (*engine).to_string(),
                source: (*source).to_string(),
                bytecode: None,
            })
            .collect(),
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

/// One script in one language.
fn app_with(engine: &str, source: &str, plugin: impl Plugin + Send + 'static) -> EcsApp {
    app_with_scripts(&[(engine, source)], plugin)
}

/// Run `$body` against the host resource `$engine` names, borrowed out of the
/// world the way the runtime's own reload path borrows it. The body is written
/// once against [`ScriptHost`] and type-checked per host.
macro_rules! with_host {
    ($app:expr, $engine:expr, |$host:ident| $body:block) => {
        match $engine {
            "rhai" => {
                let mut $host = $app.world.resource_mut::<lumen_script_rhai::RhaiHost>();
                $body
            }
            "lua" => {
                let mut $host = $app.world.resource_mut::<lumen_script_lua::LuaHost>();
                $body
            }
            "candela" => {
                let mut $host = $app
                    .world
                    .resource_mut::<lumen_script_candela::CandelaHost>();
                $body
            }
            other => panic!("no host resource for `{other}`"),
        }
    };
}

/// One call of the plugin's `greet`, spelled the way `engine` spells it.
/// candela reaches an embedder's function through the `native` namespace the
/// host declares for it.
fn greeting_source(engine: &str, tag: &str) -> String {
    match engine {
        "rhai" => format!("fn on_start() {{ greet(\"{tag}\"); }}"),
        "lua" => format!("function on_start() greet(\"{tag}\") end"),
        "candela" => {
            format!("fn on_start() {{ let msg = native::greet(\"{tag}\"); }}\nfn main() {{}}\n")
        }
        other => panic!("no greeting source for `{other}`"),
    }
}

/// Hot reload swaps the program, not the engine, so a function the plugin
/// registered is still bound for the reloaded script in every language.
#[test]
fn a_plugin_function_survives_a_reload_in_every_language() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for engine in LANGUAGES {
        let calls: Calls = Arc::default();
        let mut app = app_with(
            engine,
            &greeting_source(engine, "first"),
            GreeterPlugin {
                calls: calls.clone(),
            },
        );

        let reloaded = greeting_source(engine, "second");
        let uri = format!("reload.{engine}");
        with_host!(app, engine, |host| {
            host.replace(&reloaded, &uri)
                .unwrap_or_else(|e| panic!("{engine}: the reloaded script compiles: {e}"));
            host.call("on_start", &[])
                .unwrap_or_else(|e| panic!("{engine}: on_start runs again: {e}"));
        });

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["first".to_owned(), "second".to_owned()],
            "{engine}: the reloaded script reached the same plugin function"
        );
    }
}

/// A reset drops the program and leaves the host without one, so the app
/// reloads afterwards. What survives the rebuild is the registration: the
/// reloaded program reaches the same plugin function. The Lua host builds a
/// fresh `Lua` here, which is the case a lost replay would show up in.
#[test]
fn a_plugin_function_survives_a_reset_in_every_language() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for engine in LANGUAGES {
        let calls: Calls = Arc::default();
        let mut app = app_with(
            engine,
            &greeting_source(engine, "before"),
            GreeterPlugin {
                calls: calls.clone(),
            },
        );

        let reloaded = greeting_source(engine, "after");
        let uri = format!("restart.{engine}");
        with_host!(app, engine, |host| {
            host.reset();
            // Spelled through the trait: two hosts also have an inherent
            // `load` of their own, and it takes no URI.
            ScriptHost::load(&mut *host, &reloaded, &uri)
                .unwrap_or_else(|e| panic!("{engine}: the script loads into the reset host: {e}"));
            host.call("on_start", &[])
                .unwrap_or_else(|e| panic!("{engine}: on_start runs after the reset: {e}"));
        });

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["before".to_owned(), "after".to_owned()],
            "{engine}: the reset host still carries the plugin's function"
        );
    }
}

/// The candela sugar a plugin ships is spliced in front of the reloaded source
/// too, not only the first one: the reloaded script keeps calling the method
/// form and keeps reaching the plugin's function underneath it.
#[test]
fn a_candela_plugin_wrapper_survives_a_reload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pins: Calls = Arc::default();
    let mut app = app_with(
        "candela",
        "fn on_start() { let v = pin(21).level(); }\nfn main() {}\n",
        GpioPlugin {
            pins: pins.clone(),
            wrapper: r#"
struct Pin { number: int }
fn pin(number) { return Pin { number: number }; }
impl Pin {
    fn level(self) { return gpio::level(self.number); }
}
"#,
        },
    );

    {
        let mut host = app
            .world
            .resource_mut::<lumen_script_candela::CandelaHost>();
        // A wrapper the reload dropped would fail here: `pin` would be an
        // unknown function and the source would not compile.
        host.replace(
            "fn on_start() { let v = pin(7).level(); }\nfn main() {}\n",
            "reload.cdl",
        )
        .expect("the reloaded script still resolves the plugin's method form");
        host.call("on_start", &[])
            .expect("on_start runs again after the reload");
    }

    assert_eq!(
        pins.lock().unwrap().as_slice(),
        ["21".to_owned(), "7".to_owned()],
        "both the first and the reloaded script reached `gpio::level` through the wrapper"
    );
}

/// One plugin function, several languages in one app: every host binds it, and
/// every language's call reaches the same Rust body.
///
/// The registry is sealed by the first host that loads, so this is also what
/// says the seal closes the channel to later registrations without closing it
/// to the hosts that have not drained it yet.
#[test]
fn every_language_in_one_app_reaches_the_same_plugin_function() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let scripts: Vec<(&str, String)> = LANGUAGES
        .iter()
        .map(|engine| (*engine, greeting_source(engine, engine)))
        .collect();
    let scripts: Vec<(&str, &str)> = scripts
        .iter()
        .map(|(engine, source)| (*engine, source.as_str()))
        .collect();
    let app = app_with_scripts(
        &scripts,
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    let mut reached = calls.lock().unwrap().clone();
    reached.sort();
    assert_eq!(
        reached,
        vec!["candela".to_owned(), "lua".to_owned(), "rhai".to_owned()],
        "one body, one call from each language"
    );

    let registry = app.world.resource::<ScriptFnRegistry>();
    assert!(
        registry.is_sealed(),
        "the hosts have bound what they are going to bind"
    );
    assert_eq!(
        registry.fns().iter().filter(|f| f.name == "greet").count(),
        1,
        "one description, drained once per host rather than pushed once per host"
    );
}
