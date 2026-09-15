//! The artifact host: a `.cdlb` image, the same builtins, no compiler.
//!
//! `aot.rs` proves the image is well formed and that a hand-built registry
//! binds what it declares. These tests drive the whole thing through
//! [`CandelaVmHost`], which is what a compiler-free target actually installs,
//! so the builtin list, the load, the export call and the signal mirror are
//! held to one contract from one place.

use lumen_core::input::{ClickEvent, PointerButton};
use lumen_core::node::{DomIndex, DomRecord, NodeHandle, publish_dom_index};
use lumen_core::prelude::{App, TickStage};
use lumen_core::property_store::PropertyStore;
use lumen_script::event;
use lumen_script::{
    ScriptCommand, ScriptError, ScriptFn, ScriptFnAppExt, ScriptHost, ScriptLoadFailure, ScriptNs,
    ScriptTy, ScriptValue,
};
use lumen_script_candela::{CandelaHost, CandelaVmHost, ScriptCandelaVmPlugin};

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

/// The event-binding registry is process-global, so the tests that bind
/// through it run one at a time: one test's `clear_all_bindings` would
/// otherwise drop a binding another is about to dispatch through.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reaches for the whole builtin surface through the prelude, so the load only
/// succeeds if every declaration binds.
const SMOKE: &str = r#"
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "hello from candela");
}

fn bump(amount: int) -> int {
    let next = lumen::signal_get_int("count") + amount;
    lumen::signal_set_int("count", next);
    return next;
}

fn main() {}
"#;

/// Declares a `lumen` function Lumen does not register, which is what a script
/// written against a newer builtin surface than the runtime carries looks like.
const UNBOUND: &str = r#"
host "lumen" {
    no_such_builtin(string);
}

fn on_start() {
    lumen::no_such_builtin("x");
}

fn main() {}
"#;

/// Registers a derived signal, which only recomputes when something drives the
/// derivation pass. A direct `call` never reaches it; a tick does.
const DERIVED: &str = r#"
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "hello");
    lumen::derive("shout", ["greeting"], "compute_shout");
}

fn compute_shout(greeting: string) -> string {
    return greeting + "!";
}

fn main() {}
"#;

/// Binds a per-id handler, the registry the click dispatcher reads.
const HANDLERS: &str = r#"
import "lumen.cdl";

fn on_start() {
    lumen::on("click", "save", "do_save");
}

fn do_save() {
    lumen::signal_set("saved", "yes");
}

fn main() {}
"#;

/// Declares a builtin Lumen does not register itself, so only an embedder's
/// `register_script_fn` can put a closure behind it.
const NATIVE: &str = r#"
host "lumen" {
    log_it(...);
}

fn shout() {
    lumen::log_it("tag", 42);
}

fn main() {}
"#;

/// Calls a host function the test registers, so what that call does to the VM
/// is the test's to choose. Three ways in: a plain call, a derivation body,
/// and a DOM event handler.
const EXPLODES: &str = r##"
host "lumen" {
    explode(...);
    signal_set(string, string);
    int node_get_by_id(string);
    int event_on(int, string, string);
}

fn on_start() {
    lumen::signal_set("before", "yes");
    lumen::explode();
}

fn boom() {
    lumen::explode();
}

fn bind() {
    let b = lumen::node_get_by_id("btn");
    lumen::event_on(b, "click", "boom_event");
}

fn boom_event(ev: int) {
    lumen::explode();
}

fn quiet() {
    lumen::signal_set("after", "yes");
}

fn main() {}
"##;

/// Arms a click handler that panics the VM, and nothing else: what a shipped
/// app whose handler trips an internal assertion looks like from the
/// runtime's side.
const EXPLODES_ON_CLICK: &str = r##"
host "lumen" {
    explode(...);
    signal_set(string, string);
    int node_get_by_id(string);
    int event_on(int, string, string);
}

fn on_start() {
    let b = lumen::node_get_by_id("btn");
    lumen::event_on(b, "click", "boom_event");
}

fn boom_event(ev: int) {
    lumen::explode();
}

fn quiet() {
    lumen::signal_set("after", "yes");
}

fn main() {}
"##;

/// Binds a DOM event by token, the path `dispatch_event_handler` serves.
const EVENTS: &str = r##"
host "lumen" {
    int node_get_by_id(string);
    int event_on(int, string, string);
    signal_set(string, string);
}

fn setup() {
    let b = lumen::node_get_by_id("btn");
    lumen::event_on(b, "click", "clicked");
}

fn clicked(ev: int) {
    lumen::signal_set("clicked", "yes");
}

fn main() {}
"##;

fn host_for(source: &str, uri: &str) -> CandelaVmHost {
    let image = CandelaHost::new()
        .compile_bytecode(source, uri)
        .expect("the fixture compiles");
    CandelaVmHost::new(image)
}

fn loaded(source: &str, uri: &str) -> CandelaVmHost {
    let mut host = host_for(source, uri);
    host.load("", uri).expect("the image loads");
    host
}

/// Joins the marshalled args into a `Print` so a test can observe exactly what
/// crossed the boundary. Lands in the `lumen` namespace, the one the fixtures
/// declare.
fn joining_script_fn(name: &str) -> ScriptFn {
    ScriptFn::new(name)
        .ns(ScriptNs::Builtin)
        .variadic()
        .build(|cx| {
            let joined = cx
                .args()
                .iter()
                .map(ScriptValue::stringify)
                .collect::<Vec<_>>()
                .join(",");
            cx.emit(ScriptCommand::Print(joined));
            Ok(ScriptValue::Unit)
        })
}

fn prints(cmds: &[ScriptCommand]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// Publish a two-node document and return the button's packed handle, so
/// `node_get_by_id("btn")` resolves.
fn publish_button() -> u64 {
    let mut w = World::new();
    let root = w.spawn_empty().id();
    let btn = w.spawn_empty().id();
    let rec = |entity, tag: &str, id: Option<&str>, parent, children: &[_]| DomRecord {
        entity,
        generation: 0,
        tag: tag.to_string(),
        id: id.map(str::to_string),
        classes: vec![],
        parent,
        children: children.to_vec(),
        child_index: 0,
        sibling_count: 0,
        doc_order: 0,
    };
    publish_dom_index(DomIndex::build(vec![
        rec(root, "root", Some("app"), None, &[btn]),
        rec(btn, "button", Some("btn"), Some(root), &[]),
    ]));
    NodeHandle::new(btn).pack()
}

#[test]
fn the_whole_prelude_binds_against_the_registered_builtins() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    host.load("", "smoke.cdlb")
        .expect("every host function the prelude declares has a closure behind it");
}

#[test]
fn on_start_runs_and_its_writes_reach_the_mirror() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    host.load("", "smoke.cdlb").expect("the image loads");

    let outcome = host.call("on_start", &[]).expect("on_start runs");
    assert!(outcome.found, "the artifact exports on_start");
    assert_eq!(
        host.mirror_get("greeting"),
        Some(ScriptValue::Str("hello from candela".to_owned())),
        "a builtin the script called wrote through to the host"
    );
    assert!(
        !outcome.commands.is_empty(),
        "the write also queued the command that carries it to the store"
    );
}

#[test]
fn an_exported_handler_is_callable_by_name() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    host.load("", "smoke.cdlb").expect("the image loads");

    let outcome = host
        .call("bump", &[ScriptValue::I64(7)])
        .expect("an exported function runs");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(7)));
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(7)));

    let outcome = host
        .call("bump", &[ScriptValue::I64(5)])
        .expect("state is resident between calls");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(12)));
}

#[test]
fn a_handler_the_artifact_does_not_export_is_a_silent_miss() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    host.load("", "smoke.cdlb").expect("the image loads");

    let outcome = host.call("on_click", &[]).expect("a miss is not an error");
    assert!(!outcome.found);
}

#[test]
fn an_unbound_declaration_is_a_load_failure_naming_it() {
    let mut host = host_for(UNBOUND, "unbound.cdl");
    let Err(ScriptError::Compile { message, uri, .. }) = host.load("", "unbound.cdlb") else {
        panic!("a declaration with no closure behind it must fail the load");
    };
    assert_eq!(uri, "unbound.cdlb");
    assert!(
        message.contains("no_such_builtin"),
        "the failure names what is missing: {message}"
    );
}

#[test]
fn the_artifact_host_says_it_carries_no_compiler() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    assert!(host.compile_check(SMOKE, "smoke.cdl").is_err());
    assert!(host.replace(SMOKE, "smoke.cdl").is_err());
}

#[test]
fn the_exports_are_the_names_a_caller_may_use() {
    let mut host = host_for(SMOKE, "smoke.cdl");
    assert!(
        host.exports().is_empty(),
        "nothing is callable before the image loads"
    );

    host.load("", "smoke.cdlb").expect("the image loads");

    let exports = host.exports();
    assert!(exports.iter().any(|e| e == "bump"));
    assert!(exports.iter().any(|e| e == "on_start"));
    assert!(
        !exports.iter().any(|e| e == "main"),
        "main is the image's own entry point, not a name a caller may invoke: {exports:?}"
    );
}

#[test]
fn a_second_load_is_refused_because_the_bindings_are_placed_once() {
    let mut host = loaded(SMOKE, "smoke.cdlb");

    let Err(ScriptError::Runtime(message)) = host.load("", "smoke.cdlb") else {
        panic!("candela-vm binds host functions at load, so a second load has nothing to bind");
    };
    assert!(message.contains("already loaded"), "{message}");
}

#[test]
fn calling_an_export_with_the_wrong_argument_count_is_an_error_not_a_miss() {
    let mut host = loaded(SMOKE, "smoke.cdlb");

    let Err(ScriptError::Runtime(message)) = host.call("bump", &[]) else {
        panic!(
            "a name the image exports but not with this shape is a failure, not the absent-handler case"
        );
    };
    assert!(
        message.contains("bump"),
        "the failure names the function: {message}"
    );

    let outcome = host
        .call("bump", &[ScriptValue::I64(3)])
        .expect("a failed call leaves the VM usable");
    assert_eq!(outcome.ret, Some(ScriptValue::I64(3)));
}

#[test]
fn reset_drops_the_program_and_everything_the_script_registered() {
    let mut host = loaded(SMOKE, "smoke.cdlb");
    host.call("on_start", &[]).expect("on_start runs");
    assert!(host.mirror_get("greeting").is_some());

    host.reset();

    assert_eq!(
        host.mirror_get("greeting"),
        None,
        "the mirror went with the program"
    );
    assert!(host.exports().is_empty());
    let outcome = host
        .call("bump", &[ScriptValue::I64(1)])
        .expect("a call with no program loaded is a miss, not an error");
    assert!(!outcome.found);
    assert!(
        host.call_closure(&"bump".to_owned(), &[]).is_err(),
        "a derivation body cannot run without a program either"
    );
}

#[test]
fn commands_put_back_stay_ahead_of_what_a_later_call_queues() {
    let mut host = loaded(SMOKE, "smoke.cdlb");
    let restashed = host.call("on_start", &[]).expect("on_start runs").commands;
    assert!(!restashed.is_empty());

    // What `ScriptPlugin` does with the commands `on_start` produced: hold them
    // until the first tick drains the sink.
    host.push_commands(restashed);
    let outcome = host
        .call("bump", &[ScriptValue::I64(1)])
        .expect("bump runs");

    let signals: Vec<String> = outcome
        .commands
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::SetSignal { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        signals,
        vec!["greeting".to_owned(), "count".to_owned()],
        "the put-back writes come out before the ones the later call queued"
    );
    assert!(
        host.drain_commands().is_empty(),
        "the call already drained the sink"
    );
}

#[test]
fn a_store_string_syncs_back_into_the_mirror_with_its_type_intact() {
    let mut host = loaded(SMOKE, "smoke.cdlb");
    host.call("bump", &[ScriptValue::I64(7)])
        .expect("bump runs");
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(7)));

    // The store carries every signal as text; an int signal stays an int.
    host.mirror_sync_str("count", "9");
    assert_eq!(host.mirror_get("count"), Some(ScriptValue::I64(9)));

    host.mirror_sync_str("count", "not-a-number");
    assert_eq!(
        host.mirror_get("count"),
        Some(ScriptValue::I64(9)),
        "a string the mirror's type cannot hold leaves it alone"
    );

    host.mirror_set("label", ScriptValue::Str("set from Rust".to_owned()));
    assert_eq!(
        host.mirror_get("label"),
        Some(ScriptValue::Str("set from Rust".to_owned()))
    );
}

#[test]
fn a_handler_the_script_registered_is_found_by_id_and_by_template_suffix() {
    let mut host = loaded(HANDLERS, "handlers.cdlb");
    assert_eq!(
        host.handler_for("click", "save"),
        None,
        "nothing is registered until on_start runs"
    );

    host.call("on_start", &[]).expect("on_start runs");

    assert_eq!(
        host.handler_for("click", "save"),
        Some("do_save".to_owned())
    );
    assert_eq!(
        host.handler_for("click", "user-card:save"),
        Some("do_save".to_owned()),
        "a template instance's prefixed id resolves to the same handler"
    );
    assert_eq!(host.handler_for("click", "other"), None);

    // The name the registry answers with is the one the dispatcher calls.
    host.call("do_save", &[]).expect("the handler runs");
    assert_eq!(
        host.mirror_get("saved"),
        Some(ScriptValue::Str("yes".to_owned()))
    );
}

#[test]
fn a_derivation_is_registered_pending_and_recomputes_from_its_deps() {
    let mut host = loaded(DERIVED, "derived.cdlb");
    host.call("on_start", &[]).expect("on_start runs");

    assert!(
        host.pending_initial().contains("shout"),
        "a fresh derivation runs once regardless of dirt"
    );
    let dirty = ["greeting"].into_iter().collect();
    let matching = host.derivations_matching(&dirty, &Default::default());
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].0, "shout");
    assert_eq!(matching[0].1, vec!["greeting".to_owned()]);

    // candela has no closure value, so the recompute body is a function name.
    let value = host
        .call_closure(&matching[0].2, &[ScriptValue::Str("hi".to_owned())])
        .expect("the recompute body runs");
    assert_eq!(value, ScriptValue::Str("hi!".to_owned()));

    host.clear_pending(&["shout".to_owned()]);
    assert!(host.pending_initial().is_empty());
    let unrelated = ["something_else"].into_iter().collect();
    assert!(
        host.derivations_matching(&unrelated, &Default::default())
            .is_empty(),
        "a derivation whose deps did not change and is not pending does not match"
    );
}

#[test]
fn a_native_fn_registered_before_the_load_binds_to_the_image() {
    let mut host = host_for(NATIVE, "native.cdl");
    assert!(
        host.registry_mut().is_some(),
        "the binding window is open until the load"
    );
    host.register_script_fn(&joining_script_fn("log_it"))
        .expect("registering before the load is allowed");

    host.load("", "native.cdlb")
        .expect("the declaration binds to the registered fn");

    let outcome = host.call("shout", &[]).expect("shout runs");
    assert_eq!(
        prints(&outcome.commands),
        vec!["tag,42".to_owned()],
        "the script's mixed-typed arguments crossed into the native fn"
    );
}

/// A plugin's own namespace reaches an artifact through the declarations the
/// source carried when it was built.
///
/// Nothing synthesizes them here: the build had no plugin loaded, so the app
/// spells the block and the registration binds against it at load.
#[test]
fn an_artifact_binds_a_plugin_namespace_the_source_declares() {
    const GPIO: &str = r#"
host "gpio" {
    int read(int);
}

fn go() { return gpio::read(21); }

fn main() {}
"#;
    let mut host = host_for(GPIO, "gpio.cdl");
    host.register_script_fn(
        &ScriptFn::new("read")
            .ns(ScriptNs::Named("gpio".to_owned()))
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .build(|cx| Ok(ScriptValue::I64(cx.int_arg(0) * 2))),
    )
    .expect("registering before the load is allowed");

    host.load("", "gpio.cdlb")
        .expect("the declaration binds to the registered fn");
    assert_eq!(
        host.call("go", &[]).expect("go runs").ret,
        Some(ScriptValue::I64(42))
    );
}

/// An artifact has nothing left to compile, so a plugin's `.cdl` wrapper is
/// reported rather than silently dropped, and the load still succeeds.
#[test]
fn an_artifact_says_it_cannot_compile_a_wrapper() {
    let mut host = host_for(NATIVE, "native.cdl");
    host.register_script_fn(&joining_script_fn("log_it"))
        .expect("registering before the load is allowed");
    host.add_prelude("gpio", "fn pin(number) { return number; }\n");

    host.load("", "native.cdlb")
        .expect("the image still loads without the wrapper");
    assert_eq!(
        prints(&host.call("shout", &[]).expect("shout runs").commands),
        vec!["tag,42".to_owned()]
    );
}

#[test]
fn a_native_fn_registered_after_the_load_is_refused_by_name() {
    let mut host = host_for(NATIVE, "native.cdl");
    host.register_script_fn(&joining_script_fn("log_it"))
        .expect("registering before the load is allowed");
    host.load("", "native.cdlb").expect("the image loads");

    assert!(
        host.registry_mut().is_none(),
        "the binding window closes at the load"
    );
    let Err(ScriptError::Runtime(message)) = host.register_script_fn(&joining_script_fn("later"))
    else {
        panic!("a host fn registered after the load has nothing left to bind to");
    };
    assert!(
        message.contains("later") && message.contains("before the load"),
        "the refusal names the fn and what to do instead: {message}"
    );
}

#[test]
fn a_bound_event_reaches_its_handler_until_the_binding_is_dropped() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    event::clear_all_bindings();
    let btn = publish_button();

    let mut host = loaded(EVENTS, "events.cdlb");
    let outcome = host.call("setup", &[]).expect("setup runs");
    let (token, node) = outcome
        .commands
        .iter()
        .find_map(|c| match c {
            ScriptCommand::BindEvent { token, node, .. } => Some((*token, *node)),
            _ => None,
        })
        .expect("event_on emits BindEvent");
    assert_eq!(node, btn, "bound against the button's real handle");

    assert!(
        host.dispatch_event_handler(token)
            .expect("the handler runs"),
        "the token resolved to the function event_on named"
    );
    assert_eq!(
        host.mirror_get("clicked"),
        Some(ScriptValue::Str("yes".to_owned()))
    );

    host.drop_event_handler(token);
    assert!(
        !host
            .dispatch_event_handler(token)
            .expect("an unbound token is not an error"),
        "a dropped binding delivers nothing"
    );
    event::clear_all_bindings();
}

/// A builtin that fails the way an internal VM assertion does.
///
/// `register_script_fn` puts this Rust closure behind `lumen::explode`
/// untouched, so the panic unwinds out of `RuntimeProgram::call` at exactly
/// the boundary a corrupted VM's assertion would.
fn exploding_script_fn() -> ScriptFn {
    ScriptFn::new("explode")
        .ns(ScriptNs::Builtin)
        .variadic()
        .build(|_| panic!("an internal VM assertion fired"))
}

/// A loaded artifact host whose `explode` builtin panics.
fn exploding_host() -> CandelaVmHost {
    let mut host = host_for(EXPLODES, "explodes.cdl");
    host.register_script_fn(&exploding_script_fn())
        .expect("registering before the load is allowed");
    host.load("", "explodes.cdlb").expect("the image loads");
    host
}

/// A panic out of the VM stays inside the host, on the plain call path.
///
/// candela reports a script's own problems as errors, so a panic means the
/// interpreter itself gave up and its state cannot be trusted. The call comes
/// back as a script error, the image goes with it, and later probes miss
/// instead of the process ending. `CandelaHost` is held to the same contract
/// by `host.rs`.
#[test]
fn a_vm_panic_in_a_call_is_contained_and_disables_the_image() {
    let mut host = exploding_host();

    let Err(ScriptError::Runtime(message)) = host.call("boom", &[]) else {
        panic!("a panic out of the VM must come back as an error, not end the process");
    };
    assert!(
        message.contains("candela VM panicked"),
        "the error says what happened: {message}"
    );
    assert!(
        message.contains("boom"),
        "the error names the call it came out of: {message}"
    );

    let after = host
        .call("quiet", &[])
        .expect("a later probe answers rather than dying");
    assert!(!after.found, "nothing is callable once the image is gone");
    assert!(
        host.exports().is_empty(),
        "and nothing is advertised as callable either"
    );
}

/// The same containment on the derivation-body path.
#[test]
fn a_vm_panic_in_a_closure_call_is_contained() {
    let mut host = exploding_host();

    let Err(ScriptError::Runtime(message)) = host.call_closure(&"boom".to_owned(), &[]) else {
        panic!("a derivation body that panics must come back as an error");
    };
    assert!(message.contains("candela VM panicked"), "{message}");

    assert!(
        host.call_closure(&"boom".to_owned(), &[]).is_err(),
        "a body cannot run without a program either"
    );
}

/// The same containment on the DOM-event path, which is the one a click
/// reaches: the runtime dispatches a bound token straight into the host.
#[test]
fn a_vm_panic_in_an_event_handler_is_contained() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    event::clear_all_bindings();
    publish_button();
    let mut host = exploding_host();

    let outcome = host.call("bind", &[]).expect("bind runs");
    let token = outcome
        .commands
        .iter()
        .find_map(|c| match c {
            ScriptCommand::BindEvent { token, .. } => Some(*token),
            _ => None,
        })
        .expect("event_on emits BindEvent");

    let Err(ScriptError::Runtime(message)) = host.dispatch_event_handler(token) else {
        panic!("a handler that panics must come back as an error, not end the process");
    };
    assert!(message.contains("candela VM panicked"), "{message}");

    assert!(
        !host
            .dispatch_event_handler(token)
            .expect("a later dispatch answers rather than dying"),
        "the handler is gone with the image it lived in"
    );
    event::clear_all_bindings();
}

/// The whole thing under a real app: the fault happens where a user meets it,
/// during the app's own start-up, and the app outlives it and keeps ticking.
#[test]
fn an_app_outlives_a_panic_out_of_its_image() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    event::clear_all_bindings();
    let image = CandelaHost::new()
        .compile_bytecode(EXPLODES, "explodes.cdl")
        .expect("the fixture compiles");
    let mut app = App::new();
    app.add_script_fn(exploding_script_fn());
    app.add_plugin(ScriptCandelaVmPlugin::new(image).with_uri("explodes.cdlb"));

    assert!(
        app.world.get_resource::<ScriptLoadFailure>().is_none(),
        "the image itself loaded; what failed was the call into it"
    );
    assert_eq!(
        app.world.resource::<CandelaVmHost>().mirror_get("before"),
        Some(ScriptValue::Str("yes".to_owned())),
        "what on_start wrote before the fault still landed"
    );

    app.tick();
    app.tick();

    let mut host = app.world.resource_mut::<CandelaVmHost>();
    assert!(
        !host
            .call("quiet", &[])
            .expect("the host still answers")
            .found,
        "the image went with the panic, so every later call is a miss"
    );
    event::clear_all_bindings();
}

/// The path that ends the process today: a click on a bound element, carried
/// by the runtime's own dispatcher straight into the host. The tick the fault
/// happens on returns, the next tick runs, and a write after the fault lands.
#[test]
fn a_click_whose_handler_panics_the_vm_leaves_the_app_ticking() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    event::clear_all_bindings();
    let btn = publish_button();
    let image = CandelaHost::new()
        .compile_bytecode(EXPLODES_ON_CLICK, "explodes-on-click.cdl")
        .expect("the fixture compiles");
    let mut app = App::new();
    app.add_script_fn(exploding_script_fn());
    app.add_plugin(ScriptCandelaVmPlugin::new(image).with_uri("explodes-on-click.cdlb"));
    app.add_systems(
        TickStage::Systems,
        lumen_script::dispatch_pointer_and_key_events::<CandelaVmHost>,
    );
    assert!(
        app.world.get_resource::<ScriptLoadFailure>().is_none(),
        "the image loaded and on_start armed the handler"
    );
    // The binding `on_start` asked for, registered the way the DOM applier
    // registers it when the command lands.
    let mut host = app.world.resource_mut::<CandelaVmHost>();
    let (token, node) = host
        .drain_commands()
        .into_iter()
        .find_map(|c| match c {
            ScriptCommand::BindEvent { token, node, .. } => Some((token, node)),
            _ => None,
        })
        .expect("event_on emits BindEvent");
    assert_eq!(node, btn, "bound against the button's real handle");
    event::register_host_binding(token, node, "click".to_owned(), false);

    // The click, as the input pipeline reports one.
    app.world.write_message(ClickEvent {
        entity: Entity::try_from_bits(btn).expect("a packed handle"),
        position: Default::default(),
        button: PointerButton::Primary,
    });
    app.tick();
    app.tick();

    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str("after", "yes");
    app.tick();
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("after")
            .as_deref(),
        Some("yes"),
        "the app outlived the fault and a later write landed"
    );
    assert!(
        !app.world
            .resource_mut::<CandelaVmHost>()
            .call("quiet", &[])
            .expect("the host still answers")
            .found,
        "the image went with the panic"
    );
    event::clear_all_bindings();
}

#[test]
fn the_artifact_host_offers_the_compiler_host_s_builtin_surface() {
    let vm = CandelaVmHost::new(Vec::new());
    assert_eq!(vm.lang(), "candela");

    let names =
        |b: &[lumen_script::BuiltinFn]| -> Vec<&'static str> { b.iter().map(|f| f.name).collect() };
    assert_eq!(
        names(vm.builtins()),
        names(CandelaHost::new().builtins()),
        "one builtin list serves both hosts, so a script built to bytecode calls what it compiled against"
    );
}

#[test]
fn the_plugin_boots_an_image_and_a_tick_runs_its_derivation() {
    let image = CandelaHost::new()
        .compile_bytecode(DERIVED, "derived.cdl")
        .expect("the fixture compiles");
    let mut app = App::new();
    app.add_plugin(ScriptCandelaVmPlugin::new(image).with_uri("derived.cdlb"));
    assert!(
        app.world.get_resource::<ScriptLoadFailure>().is_none(),
        "the image binds against the registered builtins"
    );

    assert_eq!(
        app.world.resource::<CandelaVmHost>().mirror_get("greeting"),
        Some(ScriptValue::Str("hello".to_owned())),
        "the plugin loaded the image and fired on_start before installing the host"
    );

    app.tick();

    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("shout")
            .as_deref(),
        Some("hello!"),
        "the derivation pass called back into the artifact and committed the result"
    );
    assert!(
        app.world
            .resource::<CandelaVmHost>()
            .pending_initial()
            .is_empty(),
        "a derivation that evaluated is no longer awaiting its initial run"
    );
}
