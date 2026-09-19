//! `ScriptHost::register_script_fn` on the Lua host: the global a function
//! lands on, the table a named namespace becomes, the check a declared
//! parameter type gets, and the replay that survives a reset.
//!
//! The reset case is the one with history: `reset` rebuilds the `Lua` from
//! scratch, so without the replay every embedder-exposed function came back as
//! a nil global and the call failed with no diagnostic.

use lumen_script::{ScriptCommand, ScriptFn, ScriptHost, ScriptNs, ScriptTy, ScriptValue};
use lumen_script_lua::LuaHost;

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

fn returns_int(host: &mut LuaHost, source: &str) -> i64 {
    host.load(source).expect("load");
    let outcome = host.call("probe", &[]).expect("probe runs");
    match outcome.ret {
        Some(ScriptValue::I64(n)) => n,
        other => panic!("expected an integer, got {other:?}"),
    }
}

#[test]
fn a_registered_function_is_a_global() {
    let mut host = LuaHost::new();
    host.register_script_fn(&summing_fn("total", 3))
        .expect("register");

    assert_eq!(
        returns_int(&mut host, "function probe() return total(1, 2, 3) end"),
        6
    );
}

/// Lua binds variadically, so an untyped function takes whatever the call
/// passed: the declared arity is a description, not a gate.
#[test]
fn an_untyped_function_accepts_any_argument_count() {
    let mut host = LuaHost::new();
    host.register_script_fn(&summing_fn("total", 2))
        .expect("register");

    assert_eq!(
        returns_int(&mut host, "function probe() return total(1, 2, 3, 4) end"),
        10
    );
}

/// A declared type is what Lua cannot get from the binding, so the adapter
/// checks it and raises at the call.
#[test]
fn a_declared_parameter_type_raises_on_the_wrong_argument() {
    let mut host = LuaHost::new();
    host.register_script_fn(
        &ScriptFn::new("set_pin")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Bool)
            .build(|cx| Ok(ScriptValue::Bool(cx.int_arg(0) > 0))),
    )
    .expect("register");

    host.load(
        "function ok() return set_pin(3) end\n\
         function bad() return set_pin(\"three\") end",
    )
    .expect("load");
    assert_eq!(
        host.call("ok", &[]).expect("ok runs").ret,
        Some(ScriptValue::Bool(true))
    );
    let err = host
        .call("bad", &[])
        .expect_err("a string where an int is declared fails the call");
    let message = err.to_string();
    assert!(
        message.contains("set_pin") && message.contains("int"),
        "the error names the function and the expected type: {message}"
    );
}

/// A named namespace is a global table the script indexes.
#[test]
fn a_named_namespace_is_a_global_table() {
    let mut host = LuaHost::new();
    host.register_script_fn(&summing_fn("one", 1).with_ns(ScriptNs::Named("dev".into())))
        .expect("register");
    host.register_script_fn(&summing_fn("two", 2).with_ns(ScriptNs::Named("dev".into())))
        .expect("register");

    assert_eq!(
        returns_int(
            &mut host,
            "function probe() return dev.one(4) + dev.two(1, 2) end"
        ),
        7
    );
}

/// A reset rebuilds the VM and puts the registrations back.
#[test]
fn a_registered_function_survives_a_reset() {
    let mut host = LuaHost::new();
    host.register_script_fn(&summing_fn("total", 1))
        .expect("register");
    assert_eq!(
        returns_int(&mut host, "function probe() return total(7) end"),
        7
    );

    ScriptHost::reset(&mut host);
    assert_eq!(
        returns_int(&mut host, "function probe() return total(9) end"),
        9
    );
}

/// A body that emits commands reaches the host sink.
#[test]
fn an_emitting_body_reaches_the_command_sink() {
    let mut host = LuaHost::new();
    host.register_script_fn(&ScriptFn::commands("shout", 1, |cx| {
        cx.emit(ScriptCommand::Print(cx.str_arg(0).to_uppercase()));
    }))
    .expect("register");

    host.load(r#"function probe() shout("hi") end"#)
        .expect("load");
    let outcome = host.call("probe", &[]).expect("probe runs");
    let prints: Vec<String> = outcome
        .commands
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(prints, vec!["HI".to_owned()]);
}

/// A function that fails raises in the script, naming itself.
///
/// Lua sees an ordinary error, so `pcall` catches it and an uncaught one is
/// reported the way every other Lua failure is; either way the host survives.
#[test]
fn a_failing_function_raises_and_names_itself() {
    let mut host = LuaHost::new();
    host.register_script_fn(
        &ScriptFn::new("gpio_read")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .build(|cx| match cx.int_arg(0) {
                21 => Ok(ScriptValue::I64(1)),
                pin => Err(format!("pin {pin} is not wired")),
            }),
    )
    .expect("register");
    host.load("function probe() return gpio_read(7) end\nfunction safe() return gpio_read(21) end")
        .expect("load");

    let message = host
        .call("probe", &[])
        .expect_err("the function refused")
        .to_string();
    assert!(
        message.contains("gpio_read") && message.contains("pin 7 is not wired"),
        "the message has to name the function and carry what it said: {message}"
    );
    assert_eq!(
        host.call("safe", &[]).expect("21 is wired").ret,
        Some(ScriptValue::I64(1)),
        "the host is still usable"
    );
}

/// The script can catch it: `pcall` is what Lua reaches for.
#[test]
fn a_script_catches_a_failing_function() {
    let mut host = LuaHost::new();
    host.register_script_fn(
        &ScriptFn::new("gpio_read")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .build(|_| Err("the bus is down".to_owned())),
    )
    .expect("register");

    assert_eq!(
        returns_int(
            &mut host,
            "function probe()\n\
             \x20 local ok, err = pcall(gpio_read, 7)\n\
             \x20 if ok then return 0 end\n\
             \x20 return -1\n\
             end",
        ),
        -1
    );
}
