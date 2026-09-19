//! When a registration is accepted, and what a late one costs.
//!
//! The channel a plugin registers through is open for exactly as long as it
//! takes the script hosts to bind: `ScriptPlugin::build` drains the registry,
//! hands each host what it may see, and seals it. A plugin installed through
//! `RunOptions::with_plugin` runs inside that window. Anything that registers
//! afterwards has nothing left to bind to, and these pin the outcome rather
//! than leaving it to be discovered as a function a script cannot call.

use std::sync::{Arc, Mutex};

use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{
    ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptFnRegistry, ScriptHost, ScriptNs, ScriptValue,
};

/// Nav, the DOM snapshot and the property store are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What each registered body recorded, readable once the app has ticked.
type Calls = Arc<Mutex<Vec<String>>>;

/// A plugin that registers `mark()` under the `probe` namespace, reporting
/// `tag` so a test can tell which of two plugins the script reached.
struct MarkPlugin {
    tag: &'static str,
    calls: Calls,
}

impl Plugin for MarkPlugin {
    fn build(self, app: &mut EcsApp) {
        let (tag, calls) = (self.tag, self.calls);
        app.add_script_fn(
            ScriptFn::new("mark")
                .ns(ScriptNs::Named("probe".to_string()))
                .build(move |cx| {
                    calls.lock().unwrap().push(tag.to_string());
                    cx.emit(ScriptCommand::SetSignal {
                        name: "mark".to_string(),
                        value: tag.to_string(),
                    });
                    Ok(ScriptValue::Str(tag.to_string()))
                }),
        );
    }
}

/// Build a headless app from `source` in `engine`, with `plugins` installed in
/// order.
fn app_with(
    engine: &str,
    source: &str,
    plugins: Vec<Box<dyn FnOnce(RunOptions) -> RunOptions>>,
) -> EcsApp {
    let dir = std::env::temp_dir().join(format!(
        "lumen_script_fn_registration_{}_{}",
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

    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    for install in plugins {
        opts = install(opts);
    }
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

/// The call each language writes to reach `probe::mark`.
fn call_source(engine: &str) -> &'static str {
    match engine {
        "rhai" => "fn on_start() { probe::mark(); }",
        "lua" => "function on_start() probe.mark() end",
        _ => "fn on_start() { let m = probe::mark(); }\nfn main() {}\n",
    }
}

/// Two plugins wanting the same namespace and name: the later one is bound.
///
/// Every engine behind a host does this with a repeated registration, so the
/// registry keeps the order and lets it stand rather than refusing the second
/// plugin or leaving which one wins to the host.
#[test]
fn the_later_of_two_plugins_registering_one_name_is_the_one_bound() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for engine in ["rhai", "lua", "candela"] {
        let calls: Calls = Arc::default();
        let (first, second) = (calls.clone(), calls.clone());
        let app = app_with(
            engine,
            call_source(engine),
            vec![
                Box::new(move |o: RunOptions| {
                    o.with_plugin(MarkPlugin {
                        tag: "first",
                        calls: first,
                    })
                }),
                Box::new(move |o: RunOptions| {
                    o.with_plugin(MarkPlugin {
                        tag: "second",
                        calls: second,
                    })
                }),
            ],
        );

        assert_eq!(
            signal(&app, "mark").as_deref(),
            Some("second"),
            "{engine}: the second registration is the one the script reached"
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["second".to_owned()],
            "{engine}: the shadowed body did not also run"
        );
    }
}

/// A registration made after the hosts have bound is refused.
///
/// The registry is sealed once every host has drained it, so a call arriving
/// afterwards leaves the channel as it was. The host is asked directly, because
/// a function that never entered the registry is one no script can name.
#[test]
fn a_registration_after_the_hosts_have_bound_changes_nothing() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls: Calls = Arc::default();
    let mut app = app_with(
        "rhai",
        call_source("rhai"),
        vec![Box::new({
            let calls = calls.clone();
            move |o: RunOptions| {
                o.with_plugin(MarkPlugin {
                    tag: "installed",
                    calls,
                })
            }
        })],
    );

    assert!(
        app.world.resource::<ScriptFnRegistry>().is_sealed(),
        "the hosts have bound, so the channel is closed"
    );
    let before = app.world.resource::<ScriptFnRegistry>().fns().len();
    app.add_script_fn(ScriptFn::value("late", 0, |_| ScriptValue::Unit));
    app.add_script_prelude("candela", "late", "fn late_helper() { return 1; }\n");
    let registry = app.world.resource::<ScriptFnRegistry>();
    assert_eq!(
        registry.fns().len(),
        before,
        "a sealed channel takes no more functions"
    );
    assert!(
        registry.preludes_for_lang("candela").is_empty(),
        "nor any more plugin source"
    );

    // The host bound what the registry held at seal time, and nothing since.
    // Rhai resolves a call when it runs it, so the miss shows at the call.
    let mut host = app.world.resource_mut::<lumen_script_rhai::RhaiHost>();
    host.replace("fn probe_late() { late(); }", "late.rhai")
        .expect("the source compiles; the name is only unresolved at the call");
    assert!(
        host.call("probe_late", &[]).is_err(),
        "the late function is not callable"
    );
    assert_eq!(calls.lock().unwrap().as_slice(), ["installed".to_owned()]);
}

/// A function a plugin registers under a namespace no host can bind still
/// leaves the app's own script running.
///
/// candela declares what it binds, so a name its grammar rejects would fail the
/// app's compile. The registration is refused instead, and the rest of the
/// program is untouched.
#[test]
fn a_registration_no_host_can_bind_leaves_the_script_running() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    struct BadNamePlugin;

    impl Plugin for BadNamePlugin {
        fn build(self, app: &mut EcsApp) {
            app.add_script_fn(
                ScriptFn::value("not-an-identifier", 0, |_| ScriptValue::Unit)
                    .with_ns(ScriptNs::Named("probe".to_string())),
            );
        }
    }

    let app = app_with(
        "candela",
        "import \"lumen.cdl\";\n\
         fn on_start() { lumen::signal_set(\"alive\", \"yes\"); }\n\
         fn main() {}\n",
        vec![Box::new(|o: RunOptions| o.with_plugin(BadNamePlugin))],
    );
    assert_eq!(
        signal(&app, "alive").as_deref(),
        Some("yes"),
        "the app's own script compiled and ran"
    );
}
