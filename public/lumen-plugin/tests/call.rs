//! What one call carries across the boundary and back.

mod common;

use std::collections::HashMap;

use common::install_fixture;
use lumen_script::{ScriptCommand, ScriptValue};

#[test]
fn every_value_shape_survives_the_round_trip() {
    let plugin = install_fixture("call-shapes", "");
    let shape = |kind: &str| {
        plugin
            .call("fixture_shape", &[ScriptValue::Str(kind.into())])
            .0
    };

    assert_eq!(shape("unit"), Ok(ScriptValue::Unit));
    assert_eq!(shape("bool"), Ok(ScriptValue::Bool(true)));
    assert_eq!(shape("int"), Ok(ScriptValue::I64(-7)));
    assert_eq!(shape("float"), Ok(ScriptValue::F64(2.5)));
    assert_eq!(shape("str"), Ok(ScriptValue::Str("fixture".into())));
    assert_eq!(
        shape("array"),
        Ok(ScriptValue::Array(vec![
            ScriptValue::I64(1),
            ScriptValue::Str("two".into()),
        ]))
    );
    assert_eq!(
        shape("map"),
        Ok(ScriptValue::Map(HashMap::from([
            ("n".to_string(), ScriptValue::I64(3)),
            ("nested".to_string(), ScriptValue::Array(Vec::new())),
        ])))
    );
}

#[test]
fn the_arguments_reach_the_plugin_as_they_were_passed() {
    let plugin = install_fixture("call-args", "");
    let nested = ScriptValue::Array(vec![
        ScriptValue::Map(HashMap::from([("k".to_string(), ScriptValue::F64(0.5))])),
        ScriptValue::Unit,
    ]);
    assert_eq!(
        plugin.call("fixture_echo", std::slice::from_ref(&nested)).0,
        Ok(nested)
    );
}

#[test]
fn a_function_that_fails_raises_its_own_message() {
    let plugin = install_fixture("call-fail", "");
    let (ret, commands) = plugin.call("fixture_fail", &[ScriptValue::Str("no device".into())]);
    assert_eq!(ret, Err("fixture failure: no device".to_string()));
    assert!(commands.is_empty());
}

#[test]
fn what_a_call_emitted_arrives_even_when_it_then_fails() {
    let plugin = install_fixture("call-emit", "");
    let (ret, commands) = plugin.call("fixture_emit", &[ScriptValue::I64(3)]);
    assert_eq!(ret, Ok(ScriptValue::Unit));
    assert_eq!(commands.len(), 3);
    assert!(matches!(&commands[0], ScriptCommand::Print(s) if s == "emit 0"));

    let (ret, commands) = plugin.call("fixture_emit_then_fail", &[]);
    assert_eq!(ret, Err("fixture failed after emitting".to_string()));
    assert!(
        matches!(&commands[..], [ScriptCommand::Print(s)] if s == "before the failure"),
        "{commands:?}"
    );
}

#[test]
fn a_panicking_call_fails_that_call_and_nothing_else() {
    let plugin = install_fixture("call-panic", "");
    let err = plugin.call("fixture_panic", &[]).0.unwrap_err();
    assert!(
        err.starts_with("lumen-plugin-fixture/fixture_panic: panicked:"),
        "{err}"
    );
    assert!(err.contains("fixture panic in call"), "{err}");

    // The library is still usable: the panic was caught on its own side.
    assert_eq!(
        plugin.call("fixture_echo", &[ScriptValue::I64(1)]).0,
        Ok(ScriptValue::I64(1))
    );
}

#[test]
fn a_function_past_the_first_is_called_by_its_own_index() {
    let plugin = install_fixture("call-index", "fn_count = 3");
    for i in 0..3 {
        assert_eq!(
            plugin.call(&format!("fixture_pad{i}"), &[]).0,
            Ok(ScriptValue::I64(i))
        );
    }
}
