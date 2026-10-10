//! A script calling a plugin's function the wrong way cannot take the process
//! down, and cannot wedge the app.
//!
//! Every case calls a [`ScriptFn`] in a way its signature does not admit: the
//! wrong argument count, the wrong argument types, more arguments than a
//! variadic binding covers, a structured value where a scalar is expected, a
//! unit return read as a value. candela checks the call against the
//! declaration the host synthesized and refuses the whole program before a
//! handler ever runs. After every case the app still ticks, and where a
//! handler does run a well-formed call still reaches the property store.

use std::sync::{Arc, Mutex};

// The candela host, compiled in: the module registry answers the program's
// implied host dependency from it.
use lumen_candela_dev as _;
use lumen_core::app::{App as EcsApp, Plugin};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_ir::artifact::{self, CompiledApp, CompiledScript};
use lumen_ir::layout_ir::{Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::{
    ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptLoadFailure, ScriptTy, ScriptValue,
};

/// Nav, the DOM snapshot, and the property store are process-global, so the
/// headless apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the plugin's function bodies saw, in call order.
type Log = Arc<Mutex<Vec<String>>>;

/// A value with its type spelled out, so an argument that arrived padded,
/// coerced, or restructured shows up in the log instead of being stringified
/// into something that looks right.
fn tagged(v: &ScriptValue) -> String {
    match v {
        ScriptValue::Unit => "unit".to_string(),
        ScriptValue::Bool(b) => format!("bool:{b}"),
        ScriptValue::I64(n) => format!("int:{n}"),
        ScriptValue::F64(n) => format!("float:{n}"),
        ScriptValue::Str(s) => format!("str:{s}"),
        ScriptValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(tagged).collect();
            format!("[{}]", parts.join(","))
        }
        ScriptValue::Map(entries) => {
            let mut keys: Vec<&String> = entries.keys().collect();
            keys.sort_unstable();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{k}={}", tagged(&entries[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// One call's argument list, as the body received it.
fn render(args: &[ScriptValue]) -> String {
    let parts: Vec<String> = args.iter().map(tagged).collect();
    parts.join("|")
}

/// The plugin under abuse: one typed function, one variadic one, two that hand
/// back structured values, one that carries a string back out of the script,
/// and a control whose signal says the app is still alive.
struct ProbePlugin {
    log: Log,
}

impl Plugin for ProbePlugin {
    fn build(self, app: &mut EcsApp) {
        let log = self.log;

        // Typed and non-variadic. `(string, int) -> unit` is a shape candela's
        // adapter binds typed, so the host has a declaration to check a call
        // against rather than a variadic catch-all.
        let l = log.clone();
        app.add_script_fn(
            ScriptFn::new("mark")
                .param("label", ScriptTy::Str)
                .param("count", ScriptTy::Int)
                .ret(ScriptTy::Unit)
                .build(move |cx| {
                    l.lock()
                        .unwrap()
                        .push(format!("mark({})", render(cx.args())));
                    Ok(ScriptValue::Unit)
                }),
        );

        // Carries a string the script computed back to the test, so a value
        // that only exists inside the script is still observable.
        let l = log.clone();
        app.add_script_fn(
            ScriptFn::new("report")
                .param("text", ScriptTy::Str)
                .ret(ScriptTy::Unit)
                .build(move |cx| {
                    l.lock().unwrap().push(format!("report:{}", cx.str_arg(0)));
                    Ok(ScriptValue::Unit)
                }),
        );

        // Variadic with no declared parameter: the shape the C ABI's
        // `lumen_app_expose` and the SDK's `native_fn` produce.
        let l = log.clone();
        app.add_script_fn(
            ScriptFn::new("blend")
                .min_arity(0)
                .variadic()
                .build(move |cx| {
                    l.lock().unwrap().push(format!(
                        "blend/{}({})",
                        cx.args().len(),
                        render(cx.args())
                    ));
                    Ok(ScriptValue::I64(cx.args().len() as i64))
                }),
        );

        // Structured values in and out. Both are untyped one-argument
        // functions, so the host passes whatever the script built through.
        // What they hand back is uniform: a candela list holds one element type
        // and a candela map one value type, so a mixed collection could not be
        // read back.
        let l = log.clone();
        app.add_script_fn(ScriptFn::value("shape_map", 1, move |args| {
            l.lock()
                .unwrap()
                .push(format!("shape_map({})", render(args)));
            ScriptValue::Map(std::collections::HashMap::from([
                ("tag".to_string(), ScriptValue::Str("map-out".to_string())),
                ("echo".to_string(), ScriptValue::Str(render(args))),
            ]))
        }));

        let l = log.clone();
        app.add_script_fn(ScriptFn::value("shape_list", 1, move |args| {
            l.lock()
                .unwrap()
                .push(format!("shape_list({})", render(args)));
            ScriptValue::Array(vec![
                ScriptValue::Str("list-out".to_string()),
                ScriptValue::Str("9".to_string()),
            ])
        }));

        // The control. Its signal reaching the property store is what says the
        // app survived the abuse and is still applying script commands.
        let l = log.clone();
        app.add_script_fn(ScriptFn::commands("control", 0, move |cx| {
            l.lock().unwrap().push("control".to_string());
            cx.emit(ScriptCommand::SetSignal {
                name: "control".to_string(),
                value: "ok".to_string(),
            });
        }));
    }
}

/// What one abused app left behind.
struct Outcome {
    /// The probe calls that reached a body, in order, plus what the script
    /// reported back.
    calls: Vec<String>,
    /// The control signal, as the property store holds it.
    control: Option<String>,
    /// The load failure, when the program never compiled.
    load_failure: Option<String>,
}

/// Build a headless app running `source` as its candela program with the
/// probe plugin installed, tick it past construction, and read off what
/// happened.
///
/// Four ticks. The first two are what `on_start` needs: its commands are
/// re-stashed into the host sink and drained on the first, and the applier
/// commits them during it. `on_ready` fires on the first tick after the DOM
/// index is published. The rest are the wedge check, so an app the abuse left
/// in a broken state panics here rather than in an assertion about a signal.
fn run(source: &str) -> Outcome {
    let log: Log = Arc::default();
    let dir = std::env::temp_dir().join(format!(
        "lumen_script_fn_abuse_{}_{}",
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
            source_map: Default::default(),
        }],
        ..Default::default()
    })
    .expect("serialize artifact");
    let mut opts = RunOptions::new(&dir)
        .with_artifact_bytes(bytes)
        .with_plugin(ProbePlugin { log: log.clone() });
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    for _ in 0..4 {
        app.tick();
    }
    Outcome {
        calls: log.lock().unwrap().clone(),
        control: signal(&app, "control"),
        load_failure: app
            .world
            .get_resource::<ScriptLoadFailure>()
            .map(|f| f.0.clone()),
    }
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

// -- a) fewer arguments than the signature declares --------------------------

/// candela checks the call against the declaration the host synthesized from
/// the signature, and refuses the whole program: no handler runs at all.
///
/// A handler candela can type from its own declaration is compiled when the
/// program loads, so the bad call is a load failure the app holds in
/// [`ScriptLoadFailure`] and shows as its script banner. The app itself still
/// comes up and ticks; what it comes up without is the script.
#[test]
fn candela_refuses_a_call_that_passes_too_few_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    native::mark("short");
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#);

    assert!(out.calls.is_empty(), "no handler ran: {:?}", out.calls);
    let failure = out.load_failure.expect("the program did not compile");
    assert!(
        failure.contains("mark"),
        "the failure names the call the declaration does not describe: {failure}"
    );
}

// -- b) more arguments than the signature declares ---------------------------

/// The synthesized declaration fixes the argument count, so the extra one
/// costs the program the same way a missing one does.
#[test]
fn candela_refuses_a_call_that_passes_too_many_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    native::mark("over", 1, 2);
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#);

    assert!(out.calls.is_empty(), "no handler ran: {:?}", out.calls);
    let failure = out.load_failure.expect("the program did not compile");
    assert!(failure.contains("mark"), "{failure}");
}

// -- c) the wrong argument types ---------------------------------------------

/// The declaration carries the parameter types too, so a swapped pair is
/// refused with the same reach as a wrong count: the whole program.
#[test]
fn candela_refuses_a_call_whose_argument_has_the_wrong_type() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    native::mark(1, "two");
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#);

    assert!(out.calls.is_empty(), "no handler ran: {:?}", out.calls);
    let failure = out.load_failure.expect("the program did not compile");
    assert!(failure.contains("mark"), "{failure}");
}

// -- d) variadic calls -------------------------------------------------------

/// candela binds a variadic signature as one host function taking a slice, so
/// every argument reaches the body however many there are.
#[test]
fn candela_passes_every_argument_to_a_variadic_function() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    native::blend();
    native::blend(1);
    native::blend(1, 2, 3, 4, 5, 6, 7, 8, 9);
    native::control();
}

fn main() {}
"#);

    assert_eq!(
        out.calls,
        [
            "blend/0()",
            "blend/1(int:1)",
            "blend/9(int:1|int:2|int:3|int:4|int:5|int:6|int:7|int:8|int:9)",
            "control",
        ]
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

// -- e) maps and lists, in and out -------------------------------------------

/// A map and a list survive the crossing in both directions: the body sees the
/// entries the script built, and the collection it returns is indexable in the
/// script that called it.
///
/// A map literal holds one value type, and a value handed back from a
/// variadic host function arrives as `any`, so it is read through the
/// `as_map` / `as_list` downcasts.
#[test]
fn a_map_and_a_list_cross_the_boundary_in_both_directions() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    let m = {"a": "1", "b": "two"};
    let out = as_map(native::shape_map(m));
    native::report(as_str(out.get("tag")));
    let xs = ["one", "two"];
    let l = as_list(native::shape_list(xs));
    native::report(as_str(l[0]));
    native::control();
}

fn main() {}
"#);
    assert_eq!(
        out.calls,
        [
            "shape_map({a=str:1,b=str:two})",
            "report:map-out",
            "shape_list([str:one,str:two])",
            "report:list-out",
            "control",
        ],
        "the collections arrived whole and came back indexable"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

// -- f) a unit return read as a value ----------------------------------------

/// Binding the result of a unit-returning function is legal: the absent value
/// reads as `null`, and the program is not refused.
#[test]
fn a_unit_return_binds_to_a_variable() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    let x = native::control();
    if x == null {
        native::report("x is null");
    }
}

fn main() {}
"#);
    assert_eq!(
        out.calls,
        ["control", "report:x is null"],
        "the call ran and its absent return was readable"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

// -- an error nobody catches -------------------------------------------------

/// An error the script does not catch stops its handler, and what the handler
/// had already queued still applies, as a browser keeps what a throwing
/// listener did. The next handler still fires, which is what says the app is
/// not wedged.
///
/// Every call candela can check is checked before the program runs, so the
/// error here is one only a run can find: an index past the end of a list,
/// computed from a signal no one set.
#[test]
fn an_uncaught_error_keeps_what_the_handler_had_queued() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
import "lumen.cdl";

fn on_start() {
    native::control();
    let xs = ["only"];
    let i = lumen::signal_get_int("never-set") + 5;
    native::report(xs[i]);
}

fn on_ready() {
    native::report("ready");
}

fn main() {}
"#);

    assert_eq!(
        out.load_failure, None,
        "the program compiled; the error is a run-time one"
    );
    assert_eq!(
        out.calls,
        ["control", "report:ready"],
        "on_start stopped at the bad index, and on_ready still ran"
    );
    assert_eq!(
        out.control.as_deref(),
        Some("ok"),
        "the command the handler queued before it failed still applied"
    );
}

/// A name the script misspells is refused like a call the signature does not
/// admit: the whole program, before any handler queues anything. It is never
/// read as an optional handler or function that is simply absent.
#[test]
fn a_misspelled_function_name_refuses_the_program() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(r#"
fn on_start() {
    native::control();
    native::marc("typo");
}

fn on_ready() {
    native::report("ready");
}

fn main() {}
"#);

    assert!(out.calls.is_empty(), "no handler ran: {:?}", out.calls);
    assert_eq!(out.control, None, "nothing the program queued was applied");
    let failure = out.load_failure.expect("the program did not compile");
    assert!(
        failure.contains("marc"),
        "the failure names the misspelled call: {failure}"
    );
}
