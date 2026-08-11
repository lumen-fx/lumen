//! W6.6 variadic `ScriptHost::call_event`: one entry point replaces
//! the five copy-paste `call_event_*` variants. Verifies each common
//! arity / type combination round-trips through the ScriptValue -> Dynamic
//! translation.

use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_rhai::RhaiHost;

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
    let mut host = RhaiHost::new();
    host.load(r#"fn on_start() { print("started"); }"#)
        .expect("load");
    let cmds = host.call_event("on_start", &[]).expect("call");
    assert_eq!(drain_prints(&cmds), vec!["started"]);
}

#[test]
fn call_event_one_string_arg() {
    let mut host = RhaiHost::new();
    host.load(r#"fn on_click(id) { print("clicked: " + id); }"#)
        .expect("load");
    let cmds = host
        .call_event("on_click", &[ScriptValue::Str("save".into())])
        .expect("call");
    assert_eq!(drain_prints(&cmds), vec!["clicked: save"]);
}

#[test]
fn call_event_id_bool() {
    let mut host = RhaiHost::new();
    host.load(r#"fn on_toggle(id, checked) { print(id + "=" + checked); }"#)
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
    let mut host = RhaiHost::new();
    host.load(r#"fn on_slider(id, value) { print(id + "=" + value); }"#)
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
    // Replaces the old substring-match-on-error-message detection
    // with `EvalAltResult::ErrorFunctionNotFound`. A missing fn must
    // succeed with an empty Vec.
    let mut host = RhaiHost::new();
    host.load(r#"fn other() {}"#).expect("load");
    let cmds = host
        .call_event("nonexistent_handler", &[ScriptValue::Str("x".into())])
        .expect("missing fn should succeed silently");
    assert!(cmds.is_empty());
}

#[test]
fn call_event_runtime_error_surfaces() {
    // Trigger a non-"function not found" runtime error so the
    // structured `is_function_not_found` detection has to be wrong
    // for the error to bubble. Indexing past the end of an array is a
    // canonical Rhai runtime error.
    let mut host = RhaiHost::new();
    host.load(r#"fn on_click(id) { let a = []; a[10] }"#)
        .expect("load");
    let err = host
        .call_event("on_click", &[ScriptValue::Str("x".into())])
        .expect_err("oob index should error");
    // Should be Runtime, not Compile.
    assert!(matches!(err, lumen_script::ScriptError::Runtime(_)));
}
