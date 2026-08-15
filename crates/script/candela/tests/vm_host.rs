//! The artifact host: a `.cdlb` image, the same builtins, no compiler.
//!
//! `aot.rs` proves the image is well formed and that a hand-built registry
//! binds what it declares. These tests drive the whole thing through
//! [`CandelaVmHost`], which is what a compiler-free target actually installs,
//! so the builtin list, the load, the export call and the signal mirror are
//! held to one contract from one place.

use lumen_script::{ScriptError, ScriptHost, ScriptValue};
use lumen_script_candela::{CandelaVmHost, compile_bytecode};

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

fn host_for(source: &str, uri: &str) -> CandelaVmHost {
    let image = compile_bytecode(source, uri).expect("the fixture compiles");
    CandelaVmHost::new(image)
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
