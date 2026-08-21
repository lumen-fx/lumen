//! `ScriptHost::register_script_fn` on the Rhai host: the arity range a
//! signature binds, the type errors a declared parameter preserves, the
//! namespace a `ScriptNs::Named` function lands in, and the replay that
//! survives a reset.

use lumen_script::{ScriptFn, ScriptHost, ScriptNs, ScriptTy, ScriptValue};
use lumen_script_rhai::RhaiHost;

/// Sum whatever it was passed, so a test can tell how many arguments crossed.
fn summing_fn(name: &str, arity: usize) -> ScriptFn {
    ScriptFn::value(name, arity, |args| {
        ScriptValue::I64(
            args.iter()
                .map(|v| match v {
                    ScriptValue::I64(n) => *n,
                    _ => 0,
                })
                .sum(),
        )
    })
}

fn returns_int(host: &mut RhaiHost, source: &str) -> i64 {
    host.load(source).expect("load");
    let outcome = host.call("probe", &[]).expect("probe runs");
    match outcome.ret {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("expected an integer, got {other:?}"),
    }
}

/// Five arguments cross intact. The previous command-fn channel capped at
/// four and refused anything wider.
#[test]
fn a_five_argument_function_binds_and_is_called() {
    let mut host = RhaiHost::new();
    host.register_script_fn(&summing_fn("total", 5))
        .expect("register");

    assert_eq!(
        returns_int(&mut host, "fn probe() { total(1, 2, 3, 4, 5) }"),
        15
    );
}

/// An optional trailing parameter binds at both counts, which is how one
/// description answers `page("x")` and `page()`.
#[test]
fn an_optional_parameter_binds_the_shorter_call_too() {
    let mut host = RhaiHost::new();
    host.register_script_fn(
        &ScriptFn::new("width")
            .param("scale", ScriptTy::Int)
            .min_arity(0)
            .build(|cx| ScriptValue::I64(cx.int_arg(0) + 10)),
    )
    .expect("register");

    assert_eq!(returns_int(&mut host, "fn probe() { width(5) }"), 15);
    assert_eq!(returns_int(&mut host, "fn probe() { width() }"), 10);
}

/// A declared parameter type is the Rhai parameter type, so a call passing
/// something else does not resolve and the body never runs.
///
/// The mismatch reaches the app as a runtime error naming the call. Only a miss
/// on the handler being asked for is silent, because that is how the runtime
/// probes for an optional handler; a miss inside a handler is a script calling
/// something that is not there.
#[test]
fn a_declared_parameter_type_rejects_the_wrong_argument() {
    let mut host = RhaiHost::new();
    host.register_script_fn(
        &ScriptFn::new("set_pin")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Bool)
            .build(|cx| ScriptValue::Bool(cx.int_arg(0) > 0)),
    )
    .expect("register");

    host.load(r#"fn ok() { set_pin(3) } fn bad() { set_pin("three") }"#)
        .expect("load");
    assert_eq!(
        host.call("ok", &[]).expect("ok runs").ret,
        Some(ScriptValue::Bool(true)),
        "the declared shape resolves and the body runs"
    );
    let err = host
        .call("bad", &[])
        .expect_err("a string where an int is declared does not resolve")
        .to_string();
    assert!(
        err.contains("set_pin"),
        "the error names the call the script got wrong: {err}"
    );
}

/// A migrated builtin keeps the call-site typing it had when the host
/// registered it by hand: `set_text` takes two strings, and a call passing
/// numbers does not resolve.
#[test]
fn a_shared_builtin_keeps_its_call_site_types() {
    let mut host = RhaiHost::new();
    host.load(r#"fn ok() { set_text("out", "hi") } fn bad() { set_text(1, 2) }"#)
        .expect("load");

    let outcome = host.call("ok", &[]).expect("ok runs");
    assert!(
        outcome
            .commands
            .iter()
            .any(|c| matches!(c, lumen_script::ScriptCommand::SetText { .. })),
        "the declared shape queues the command"
    );
    let err = host
        .call("bad", &[])
        .expect_err("a call with the wrong argument types does not resolve")
        .to_string();
    assert!(
        err.contains("set_text"),
        "the error names the call the script got wrong: {err}"
    );
}

/// A named namespace is a static module: the script calls `ns::name(...)`, and
/// a second function in the same namespace joins the first rather than
/// replacing the module.
#[test]
fn a_named_namespace_is_a_static_module() {
    let mut host = RhaiHost::new();
    host.register_script_fn(&summing_fn("one", 1).with_ns(ScriptNs::Named("dev".into())))
        .expect("register");
    host.register_script_fn(&summing_fn("two", 2).with_ns(ScriptNs::Named("dev".into())))
        .expect("register");

    assert_eq!(
        returns_int(&mut host, "fn probe() { dev::one(4) + dev::two(1, 2) }"),
        7
    );
}

/// A reset drops the program, not the embedder's registrations.
#[test]
fn a_registered_function_survives_a_reset() {
    let mut host = RhaiHost::new();
    host.register_script_fn(&summing_fn("total", 1))
        .expect("register");
    assert_eq!(returns_int(&mut host, "fn probe() { total(7) }"), 7);

    ScriptHost::reset(&mut host);
    assert_eq!(returns_int(&mut host, "fn probe() { total(9) }"), 9);
}

/// A body that emits commands reaches the host sink, drained with the call it
/// happened in.
#[test]
fn an_emitting_body_reaches_the_command_sink() {
    let mut host = RhaiHost::new();
    host.register_script_fn(&ScriptFn::commands("shout", 1, |cx| {
        cx.emit(lumen_script::ScriptCommand::Print(
            cx.str_arg(0).to_uppercase(),
        ));
    }))
    .expect("register");

    host.load(r#"fn probe() { shout("hi") }"#).expect("load");
    let outcome = host.call("probe", &[]).expect("probe runs");
    let prints: Vec<String> = outcome
        .commands
        .iter()
        .filter_map(|c| match c {
            lumen_script::ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(prints, vec!["HI".to_owned()]);
}

/// A handler that calls something unbound is an error, not an absent handler.
///
/// The runtime probes for optional handlers by calling them, so a miss on the
/// name it asked for comes back as `found: false`. A miss on any other name is
/// a script calling something nobody registered, and answering `found: false`
/// for it reports the handler absent: nothing reaches stderr, and the app looks
/// like it is ignoring input.
#[test]
fn a_miss_inside_a_handler_is_reported_rather_than_read_as_an_absent_handler() {
    let mut host = RhaiHost::new();
    host.load("fn on_start() { never_registered(1); }")
        .expect("load");

    let err = host
        .call("on_start", &[])
        .expect_err("the call inside the handler resolves to nothing")
        .to_string();
    assert!(
        err.contains("never_registered"),
        "the error names the function the script could not call: {err}"
    );

    // A handler that is absent is still the silent probe it has to
    // be; every optional hook the runtime offers is found this way.
    let outcome = host
        .call("on_close", &[])
        .expect("an absent handler is a miss");
    assert!(!outcome.found);
}
