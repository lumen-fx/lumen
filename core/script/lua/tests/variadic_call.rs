//! Variadic `ScriptHost::call` round-trips each common arity / type
//! through the ScriptValue <-> mlua::Value translation (parity with the
//! Rhai host's `variadic_call` test).

use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_lua::LuaHost;

fn drain_prints(cmds: &[ScriptCommand]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn call_event_zero_args() {
    let mut host = LuaHost::new();
    host.load(r#"function on_start() print("started") end"#)
        .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    assert_eq!(drain_prints(&cmds), vec!["started"]);
}

#[test]
fn call_event_one_string_arg() {
    let mut host = LuaHost::new();
    host.load(r#"function on_click(id) print("clicked: " .. id) end"#)
        .expect("load");
    let cmds = host
        .call_event("on_click", &[ScriptValue::Str("save".into())])
        .expect("call");
    assert_eq!(drain_prints(&cmds), vec!["clicked: save"]);
}

#[test]
fn call_event_id_bool() {
    let mut host = LuaHost::new();
    host.load(r#"function on_toggle(id, checked) print(id .. "=" .. tostring(checked)) end"#)
        .expect("load");
    let cmds = host
        .call_event(
            "on_toggle",
            &[ScriptValue::Str("notify".into()), ScriptValue::Bool(true)],
        )
        .expect("call");
    assert_eq!(drain_prints(&cmds), vec!["notify=true"]);
}

#[test]
fn call_event_id_f64() {
    let mut host = LuaHost::new();
    host.load(r#"function on_slider(id, value) print(id .. "=" .. value) end"#)
        .expect("load");
    let cmds = host
        .call_event(
            "on_slider",
            &[ScriptValue::Str("vol".into()), ScriptValue::F64(0.75)],
        )
        .expect("call");
    assert_eq!(drain_prints(&cmds), vec!["vol=0.75"]);
}

#[test]
fn call_event_missing_function_is_silent() {
    let mut host = LuaHost::new();
    host.load(r#"function other() end"#).expect("load");
    let cmds = host
        .call_event("nonexistent_handler", &[ScriptValue::Str("x".into())])
        .expect("missing fn should succeed silently");
    assert!(cmds.is_empty());
}

#[test]
fn call_event_runtime_error_surfaces() {
    let mut host = LuaHost::new();
    host.load(r#"function on_click(id) error("boom") end"#)
        .expect("load");
    let err = host
        .call_event("on_click", &[ScriptValue::Str("x".into())])
        .expect_err("error() should surface");
    assert!(matches!(err, lumen_script::ScriptError::Runtime(_)));
}
