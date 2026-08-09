//! `ScriptHost::register_command_fn` on the candela host: a portable native
//! command fn (`Fn(&[ScriptValue]) -> Vec<ScriptCommand>`) registered through
//! the fork's variadic host-fn API, callable from a `.cdl` script as
//! `lumen::<name>(...)` with any argument count. Mirrors the Rhai/Lua hosts'
//! command-fn extension point.

use std::sync::Arc;

use lumen_script::{ScriptCommand, ScriptHost, ScriptValue};
use lumen_script_candela::CandelaHost;

/// Join the marshalled args into a `Print` command so the test can observe
/// exactly what crossed the boundary.
fn joining_command_fn() -> lumen_script::CommandFn {
    Arc::new(|args: &[ScriptValue]| {
        let joined = args
            .iter()
            .map(ScriptValue::stringify)
            .collect::<Vec<_>>()
            .join(",");
        vec![ScriptCommand::Print(joined)]
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
fn command_fn_is_callable_from_script_and_emits_its_command() {
    let mut host = CandelaHost::new();
    host.register_command_fn("log_it", 0, joining_command_fn())
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
    host.register_command_fn("log_it", 0, joining_command_fn())
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
