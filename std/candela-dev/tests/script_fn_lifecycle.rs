//! A plugin's script function outlives the events that rebuild a host.
//!
//! `plugin_script_fn.rs` proves the first load binds what a plugin registered.
//! What these cases prove is that the binding survives what happens after it: a
//! hot reload and a reset. Each is a point where the host throws away state;
//! the candela host recompiles the app's source from scratch on both.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lumen_core::app::{App as EcsApp, Plugin};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
// The candela host, compiled in: the module registry answers the program's
// implied host dependency from it.
use lumen_candela_dev as _;
use lumen_candela_dev::CandelaHost;
use lumen_script::{
    ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptHost, ScriptNs, ScriptTy, ScriptValue,
};

/// Nav, the DOM snapshot, and the property store are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
                    Ok(ScriptValue::Str(greeting))
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
                    Ok(ScriptValue::I64(pin * 2))
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

/// Build a headless app running `source` as its candela program, with
/// `plugin` installed.
fn app_with(source: &str, plugin: impl Plugin + Send + 'static) -> EcsApp {
    let dir = scratch_dir();
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

/// One call of the plugin's `greet`. candela reaches an embedder's function
/// through the `native` namespace the host declares for it.
fn greeting_source(tag: &str) -> String {
    format!("fn on_start() {{ let msg = native::greet(\"{tag}\"); }}\nfn main() {{}}\n")
}

/// Hot reload swaps the program, not the host, so a function the plugin
/// registered is still bound for the reloaded script.
#[test]
fn a_plugin_function_survives_a_reload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let mut app = app_with(
        &greeting_source("first"),
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    {
        let mut host = app.world.resource_mut::<CandelaHost>();
        host.replace(&greeting_source("second"), "reload.cdl")
            .expect("the reloaded script compiles");
        host.call("on_start", &[]).expect("on_start runs again");
    }

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_owned(), "second".to_owned()],
        "the reloaded script reached the same plugin function"
    );
}

/// A reset drops the program and leaves the host without one, so the app
/// reloads afterwards. What survives the rebuild is the registration: the
/// reloaded program reaches the same plugin function.
#[test]
fn a_plugin_function_survives_a_reset() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let mut app = app_with(
        &greeting_source("before"),
        GreeterPlugin {
            calls: calls.clone(),
        },
    );

    {
        let mut host = app.world.resource_mut::<CandelaHost>();
        host.reset();
        host.load(&greeting_source("after"), "restart.cdl")
            .expect("the script loads into the reset host");
        host.call("on_start", &[])
            .expect("on_start runs after the reset");
    }

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["before".to_owned(), "after".to_owned()],
        "the reset host still carries the plugin's function"
    );
}

/// The candela sugar a plugin ships is spliced in front of the reloaded source
/// too, not only the first one: the reloaded script keeps calling the method
/// form and keeps reaching the plugin's function underneath it.
#[test]
fn a_candela_plugin_wrapper_survives_a_reload() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pins: Calls = Arc::default();
    let mut app = app_with(
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
        let mut host = app.world.resource_mut::<CandelaHost>();
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
