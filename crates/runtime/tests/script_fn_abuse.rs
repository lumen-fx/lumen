//! A script calling a plugin's function the wrong way cannot take the process
//! down, and cannot wedge the app.
//!
//! Every case calls a [`ScriptFn`] in a way its signature does not admit: the
//! wrong argument count, the wrong argument types, more arguments than a
//! variadic binding covers, a structured value where a scalar is expected, a
//! unit return read as a value. The three hosts refuse differently, and the
//! difference is pinned per host rather than smoothed over: Rhai resolves a
//! call by argument type and fails to find the function, Lua checks the
//! arguments in its adapter and raises, candela checks the call against the
//! declaration the host synthesized and refuses the whole handler. After every
//! case the app still ticks and a well-formed call still reaches the property
//! store.

use std::sync::{Arc, Mutex};

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
        // adapter binds typed, so all three hosts have a declaration to check a
        // call against rather than a variadic catch-all.
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
        // functions, so every host passes whatever the script built through.
        // What they hand back is uniform: a candela list holds one element type
        // and a candela map one value type, so a mixed collection could not be
        // read back on that host.
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

impl Outcome {
    /// The single message the script caught, without the `report:caught: `
    /// prefix. Panics when the script caught nothing, which is itself the
    /// interesting failure.
    fn caught(&self) -> &str {
        self.calls
            .iter()
            .find_map(|c| c.strip_prefix("report:caught: "))
            .expect("the script caught an error and reported it")
    }
}

/// Build a headless app from `source` in `engine` with the probe plugin
/// installed, tick it past construction, and read off what happened.
///
/// Four ticks. The first two are what `on_start` needs: its commands are
/// re-stashed into the host sink and drained on the first, and the applier
/// commits them during it. `on_ready` fires on the first tick after the DOM
/// index is published. The rest are the wedge check, so an app the abuse left
/// in a broken state panics here rather than in an assertion about a signal.
fn run(engine: &str, source: &str) -> Outcome {
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

/// Rhai resolves a call by name and argument types, so a short call finds no
/// registration at all: the body never runs, and what the script catches is a
/// missing function rather than a bad argument list.
#[test]
fn rhai_cannot_resolve_a_call_that_passes_too_few_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "rhai",
        r#"
fn on_start() {
    try { mark("short"); } catch (e) { report("caught: " + e.error); }
    control();
}
"#,
    );

    assert_eq!(
        out.calls,
        ["report:caught: ErrorFunctionNotFound", "control"],
        "the body never ran, and the call after the failure did"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// Lua binds one variadic closure per function, so the adapter is what checks
/// the arguments; the script sees the mismatch named.
#[test]
fn lua_raises_on_a_call_that_passes_too_few_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "lua",
        r#"
function on_start()
    local ok, err = pcall(mark, "short")
    if not ok then report("caught: " .. tostring(err):match("mark: [^\n]*")) end
    control()
end
"#,
    );

    assert_eq!(
        out.caught(),
        "mark: expected at least 2 argument(s), got 1",
        "the adapter raised before the body saw a padded argument list"
    );
    assert_eq!(out.calls.last().map(String::as_str), Some("control"));
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// candela checks the call against the declaration the host synthesized from
/// the signature, and refuses the whole handler: neither the bad call nor the
/// statement after it runs.
///
/// The check happens when the handler is called rather than when the program
/// loads, so there is no [`ScriptLoadFailure`] and the diagnostic reaches
/// stderr as an `on_start failed` warning; nothing in the app holds it. What
/// the app can still do is what `on_ready` proves.
#[test]
fn candela_refuses_a_call_that_passes_too_few_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "candela",
        r#"
fn on_start() {
    native::mark("short");
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#,
    );

    assert_eq!(
        out.calls,
        ["control"],
        "on_start ran no statement at all, and the next handler still fired"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
    assert_eq!(out.load_failure, None, "the program itself compiled");
}

// -- b) more arguments than the signature declares ---------------------------

/// An extra argument is one more shape, and a non-variadic signature is bound
/// at its declared arity only.
#[test]
fn rhai_cannot_resolve_a_call_that_passes_too_many_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "rhai",
        r#"
fn on_start() {
    try { mark("over", 1, 2); } catch (e) { report("caught: " + e.error); }
    control();
}
"#,
    );

    assert_eq!(
        out.calls,
        ["report:caught: ErrorFunctionNotFound", "control"],
        "a non-variadic signature is bound at its own arity only"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// The adapter counts the arguments against the declared parameters, and a
/// signature that is not variadic has an upper bound to report.
#[test]
fn lua_raises_on_a_call_that_passes_too_many_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "lua",
        r#"
function on_start()
    local ok, err = pcall(mark, "over", 1, 2)
    if not ok then report("caught: " .. tostring(err):match("mark: [^\n]*")) end
    control()
end
"#,
    );

    assert_eq!(out.caught(), "mark: expected at most 2 argument(s), got 3");
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// The synthesized declaration fixes the argument count, so the extra one
/// costs the handler the same way a missing one does.
#[test]
fn candela_refuses_a_call_that_passes_too_many_arguments() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "candela",
        r#"
fn on_start() {
    native::mark("over", 1, 2);
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#,
    );

    assert_eq!(out.calls, ["control"]);
    assert_eq!(out.control.as_deref(), Some("ok"));
    assert_eq!(out.load_failure, None);
}

// -- c) the wrong argument types ---------------------------------------------

/// Rhai narrows a declared parameter to its own Rust type, so swapped
/// arguments are one more shape nothing is registered under. The script is
/// told the function was not found, not that an argument had the wrong type.
#[test]
fn rhai_reports_the_wrong_argument_types_as_a_missing_function() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "rhai",
        r#"
fn on_start() {
    try { mark(1, "two"); } catch (e) { report("caught: " + e.error); }
    control();
}
"#,
    );

    assert_eq!(
        out.calls,
        ["report:caught: ErrorFunctionNotFound", "control"],
        "the mismatch is a resolution failure on this host, not a type error"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// Lua's adapter names the parameter and both types, which is the message
/// `ScriptSig::check_args` produces.
#[test]
fn lua_names_the_parameter_when_an_argument_has_the_wrong_type() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "lua",
        r#"
function on_start()
    local ok, err = pcall(mark, 1, "two")
    if not ok then report("caught: " .. tostring(err):match("mark: [^\n]*")) end
    control()
end
"#,
    );

    assert_eq!(
        out.caught(),
        "mark: argument 1 (`label`) expects string, got int"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// The declaration carries the parameter types too, so a swapped pair is
/// refused with the same reach as a wrong count: the whole handler.
#[test]
fn candela_refuses_a_call_whose_argument_has_the_wrong_type() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "candela",
        r#"
fn on_start() {
    native::mark(1, "two");
    native::report("after");
}

fn on_ready() {
    native::control();
}

fn main() {}
"#,
    );

    assert_eq!(out.calls, ["control"]);
    assert_eq!(out.control.as_deref(), Some("ok"));
    assert_eq!(out.load_failure, None);
}

// -- d) variadic calls, including past the bound -----------------------------

/// Rhai has no native variadics, so a variadic signature is one registration
/// per argument count up to `MAX_VARIADIC_ARITY`. A call past the bound is a
/// shape nothing is registered under, and the script is told so.
#[test]
fn rhai_binds_a_variadic_function_up_to_the_arity_bound_and_no_further() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "rhai",
        r#"
fn on_start() {
    blend();
    blend(1);
    try { blend(1, 2, 3, 4, 5, 6, 7, 8, 9); } catch (e) { report("caught: " + e.error); }
    control();
}
"#,
    );

    assert_eq!(
        out.calls,
        [
            "blend/0()",
            "blend/1(int:1)",
            "report:caught: ErrorFunctionNotFound",
            "control",
        ],
        "nine arguments is one past the eight a variadic signature binds for"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// Lua's binding is natively variadic and the signature declares no parameter
/// to check, so every argument reaches the body however many there are.
#[test]
fn lua_passes_every_argument_to_a_variadic_function() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "lua",
        r#"
function on_start()
    blend()
    blend(1)
    blend(1, 2, 3, 4, 5, 6, 7, 8, 9)
    control()
end
"#,
    );

    assert_eq!(
        out.calls,
        [
            "blend/0()",
            "blend/1(int:1)",
            "blend/9(int:1|int:2|int:3|int:4|int:5|int:6|int:7|int:8|int:9)",
            "control",
        ],
        "the arity bound is a Rhai registration cost, not a limit on the API"
    );
    assert_eq!(out.control.as_deref(), Some("ok"));
}

/// candela binds a variadic signature as one host function taking a slice, so
/// it too takes a call past the Rhai bound.
#[test]
fn candela_passes_every_argument_to_a_variadic_function() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "candela",
        r#"
fn on_start() {
    native::blend();
    native::blend(1);
    native::blend(1, 2, 3, 4, 5, 6, 7, 8, 9);
    native::control();
}

fn main() {}
"#,
    );

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

/// A map and a list survive the crossing in both directions on every host: the
/// body sees the same entries whichever language built them, and the
/// collection it returns is indexable in the script that called it.
///
/// The candela source spells its own collections a little differently. A map
/// literal there holds one value type, and a value handed back from a variadic
/// host function arrives as `any`, so it is read through the `as_map` /
/// `as_list` downcasts.
#[test]
fn a_map_and_a_list_cross_the_boundary_in_both_directions() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (engine, source) in [
        (
            "rhai",
            r#"
fn on_start() {
    let out = shape_map(#{ "a": "1", "b": "two" });
    report(out.tag);
    let l = shape_list(["one", "two"]);
    report(l[0]);
    control();
}
"#,
        ),
        (
            "lua",
            r#"
function on_start()
    local out = shape_map({ a = "1", b = "two" })
    report(out.tag)
    local l = shape_list({ "one", "two" })
    report(l[1])
    control()
end
"#,
        ),
        (
            "candela",
            r#"
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
"#,
        ),
    ] {
        let out = run(engine, source);
        assert_eq!(
            out.calls,
            [
                "shape_map({a=str:1,b=str:two})",
                "report:map-out",
                "shape_list([str:one,str:two])",
                "report:list-out",
                "control",
            ],
            "{engine}: the collections arrived whole and came back indexable"
        );
        assert_eq!(out.control.as_deref(), Some("ok"), "{engine}");
    }
}

// -- f) a unit return read as a value ----------------------------------------

/// Binding the result of a unit-returning function is legal on all three
/// hosts, and each spells the absent value its own way: `()` in Rhai, `nil` in
/// Lua, `null` in candela. None of them refuse the program.
#[test]
fn a_unit_return_binds_to_a_variable_on_every_host() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (engine, source, expected) in [
        (
            "rhai",
            r#"
fn on_start() {
    let x = control();
    report("x is " + type_of(x));
}
"#,
            "report:x is ()",
        ),
        (
            "lua",
            r#"
function on_start()
    local x = control()
    report("x is " .. type(x))
end
"#,
            "report:x is nil",
        ),
        (
            "candela",
            r#"
fn on_start() {
    let x = native::control();
    if x == null {
        native::report("x is null");
    }
}

fn main() {}
"#,
            "report:x is null",
        ),
    ] {
        let out = run(engine, source);
        assert_eq!(
            out.calls,
            ["control", expected],
            "{engine}: the call ran and its absent return was readable"
        );
        assert_eq!(out.control.as_deref(), Some("ok"), "{engine}");
    }
}

// -- an error nobody catches -------------------------------------------------

/// An error the script does not catch stops its handler and discards what the
/// handler had already queued, so a half-applied batch never reaches the app.
/// The next handler still fires, which is what says the app is not wedged.
#[test]
fn an_uncaught_lua_error_discards_what_the_handler_had_queued() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "lua",
        r#"
function on_start()
    control()
    mark("short")
end

function on_ready()
    report("ready")
end
"#,
    );

    assert_eq!(
        out.calls,
        ["control", "report:ready"],
        "on_start stopped at the bad call, and on_ready still ran"
    );
    assert_eq!(
        out.control, None,
        "the command the handler queued before it failed was dropped"
    );
}

/// The same script on Rhai, where the partial batch is dropped too.
///
/// The failure a short call raises here is "function not found", which is also
/// how Rhai answers a probe for a handler the script never defined. Telling the
/// two apart is what keeps this case an error rather than a silent miss that
/// leaves the handler half-applied with nothing on stderr.
#[test]
fn an_uncaught_rhai_error_discards_what_the_handler_had_queued() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let out = run(
        "rhai",
        r#"
fn on_start() {
    control();
    mark("short");
}

fn on_ready() {
    report("ready");
}
"#,
    );

    assert_eq!(
        out.calls,
        ["control", "report:ready"],
        "on_start stopped at the bad call, and on_ready still ran"
    );
    assert_eq!(
        out.control, None,
        "the command the handler queued before it failed was dropped"
    );
}

/// A name the script misspells is the same failure as a call the signature
/// does not admit: the handler stops, its queued commands are dropped, and the
/// app carries on. Neither host reads it as the handler itself being absent.
#[test]
fn a_misspelled_function_name_stops_its_handler_on_every_host() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (engine, source) in [
        (
            "rhai",
            r#"
fn on_start() {
    control();
    marc("typo");
}

fn on_ready() {
    report("ready");
}
"#,
        ),
        (
            "lua",
            r#"
function on_start()
    control()
    marc("typo")
end

function on_ready()
    report("ready")
end
"#,
        ),
    ] {
        let out = run(engine, source);
        assert_eq!(
            out.calls,
            ["control", "report:ready"],
            "{engine}: the handler stopped at the typo and the next one still ran"
        );
        assert_eq!(out.control, None, "{engine}: the partial batch was dropped");
    }
}
