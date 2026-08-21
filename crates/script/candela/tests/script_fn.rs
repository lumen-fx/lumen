//! `ScriptHost::register_script_fn` on the candela host: a native function
//! registered through the fork's variadic host-fn API, callable from a `.cdl`
//! script as `lumen::<name>(...)` with any argument count. Mirrors the
//! Rhai/Lua hosts' extension point.

use lumen_script::{ScriptCommand, ScriptFn, ScriptHost, ScriptNs, ScriptValue};
use lumen_script_candela::CandelaHost;

/// Join the marshalled args into a `Print` command so the test can observe
/// exactly what crossed the boundary. Lands in the `lumen` namespace, the one
/// the fixtures declare.
fn joining_script_fn() -> ScriptFn {
    ScriptFn::new("log_it")
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
            ScriptValue::Unit
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

#[test]
fn a_registered_fn_is_callable_from_script_and_emits_its_command() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn on_start() {
    lumen::log_it("tag", 42, true);
}
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");

    let outcome = host.call("on_start", &[]).expect("on_start");
    assert!(outcome.found);
    // The mixed-typed positional args marshalled string/int/bool into the fn.
    assert_eq!(prints(&outcome.commands), vec!["tag,42,true".to_owned()]);
}

#[test]
fn one_registration_serves_any_arity() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn none() { lumen::log_it(); }
fn one() { lumen::log_it("solo"); }
fn many() { lumen::log_it("a", "b", "c", "d"); }
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");

    assert_eq!(
        prints(&host.call("none", &[]).unwrap().commands),
        vec![String::new()]
    );
    assert_eq!(
        prints(&host.call("one", &[]).unwrap().commands),
        vec!["solo".to_owned()]
    );
    assert_eq!(
        prints(&host.call("many", &[]).unwrap().commands),
        vec!["a,b,c,d".to_owned()]
    );
}

/// A reset drops the program, not the embedder's registrations.
#[test]
fn a_registered_function_survives_a_reset() {
    let mut host = CandelaHost::new();
    host.register_script_fn(&joining_script_fn())
        .expect("register");

    let src = r#"
host "lumen" {
    log_it(...);
}
fn shout() { lumen::log_it("after"); }
fn main() {}
"#;
    host.load(src, "cmd.cdl").expect("load");
    ScriptHost::reset(&mut host);
    host.load(src, "cmd.cdl").expect("load again");

    assert_eq!(
        prints(&host.call("shout", &[]).expect("shout runs").commands),
        vec!["after".to_owned()]
    );
}

/// `compile_check` checks against what the app will run.
///
/// candela binds every `host` block while it compiles, so a check on an engine
/// carrying only Lumen's own builtins fails on a source declaring
/// `host "native" { .. }` while the app it rejects runs fine. The host replays
/// its registrations into the scratch engine to keep the two verdicts the same.
#[test]
fn compile_check_sees_the_registered_native_functions() {
    let src = r#"
host "native" {
    any answer(...);
}
fn on_start() { let n = native::answer(); }
fn main() {}
"#;

    let bare = CandelaHost::new();
    assert!(
        bare.compile_check(src, "native.cdl").is_err(),
        "with nothing registered the declaration has nothing behind it"
    );

    let mut host = CandelaHost::new();
    // The default namespace is `native`, the one the source declares.
    host.register_script_fn(&ScriptFn::value("answer", 0, |_| ScriptValue::I64(42)))
        .expect("register");
    host.compile_check(src, "native.cdl")
        .expect("the declaration binds to the registered fn");
}
