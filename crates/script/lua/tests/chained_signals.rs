//! Chained `signals.foo.set(v)` access (parity with the Rhai host's
//! `chained_signals` test): typed setters join the property + index
//! segments into a single `PropertyKey::Global` and route through the
//! `push_external_property` typed bus.
//!
//! Verified through the process-wide `external_property_snapshot`
//! (re-sends each entry after reading). Unique key names per test keep
//! parallel readers from mis-attributing cross-test writes.

use lumen_core::components::Color;
use lumen_core::property_store::{PropertyKey, PropertyValue, external_property_snapshot};
use lumen_script::ScriptCommand;
use lumen_script_lua::LuaHost;
use std::sync::Arc;

fn drain_prints(cmds: &[ScriptCommand]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| match c {
            ScriptCommand::Print(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn run_on_load(source: &str) {
    let mut host = LuaHost::new();
    host.load(source).expect("load");
    let _ = host.call_event("on_load", &[]).expect("call");
}

fn read_snapshot(key: &str) -> Option<PropertyValue> {
    let snap = external_property_snapshot();
    snap.get(&PropertyKey::Global(Arc::<str>::from(key)))
        .cloned()
}

#[test]
fn signals_set_int_round_trips() {
    run_on_load(
        r#"
        function on_load()
            signals.chain_int_count.set(42)
        end
        "#,
    );
    let value = read_snapshot("chain_int_count");
    assert!(
        matches!(value, Some(PropertyValue::I64(42))),
        "chained signals.chain_int_count.set(42) must push PropertyValue::I64; got {value:?}"
    );
}

#[test]
fn signals_set_float_round_trips() {
    run_on_load(
        r#"
        function on_load()
            signals.chain_float_amount.set(3.5)
        end
        "#,
    );
    let value = read_snapshot("chain_float_amount");
    assert!(
        matches!(value, Some(PropertyValue::F64(v)) if v == 3.5),
        "chained signals.chain_float_amount.set(3.5) must push PropertyValue::F64; got {value:?}"
    );
}

#[test]
fn signals_set_bool_round_trips() {
    run_on_load(
        r#"
        function on_load()
            signals.chain_bool_flag.set(true)
        end
        "#,
    );
    let value = read_snapshot("chain_bool_flag");
    assert!(
        matches!(value, Some(PropertyValue::Bool(true))),
        "chained signals.chain_bool_flag.set(true) must push PropertyValue::Bool; got {value:?}"
    );
}

#[test]
fn signals_nested_path_dot_joins() {
    run_on_load(
        r#"
        function on_load()
            signals.chain_user.name.set("Alice")
        end
        "#,
    );
    let value = read_snapshot("chain_user.name");
    let got = match value {
        Some(PropertyValue::Str(ref s)) => Some(s.to_string()),
        _ => None,
    };
    assert_eq!(
        got.as_deref(),
        Some("Alice"),
        "nested path must dot-join to 'chain_user.name' and land as PropertyValue::Str"
    );
}

#[test]
fn signals_set_color_via_method() {
    run_on_load(
        r##"
        function on_load()
            signals.chain_bg.set_color("#ff8800")
        end
        "##,
    );
    let value = read_snapshot("chain_bg");
    let color: Color = match value {
        Some(PropertyValue::Color(c)) => c,
        other => panic!("expected PropertyValue::Color, got {other:?}"),
    };
    let bytes = color.to_rgba8();
    assert_eq!(
        bytes,
        [0xff, 0x88, 0x00, 0xff],
        "set_color must store the parsed RGBA bytes verbatim"
    );
}

#[test]
fn signals_get_returns_nil_on_miss() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            local v = signals.chain_never_set_xyz.get()
            if type(v) == "nil" then
                print("ok-nil")
            else
                print("wrong-type: " .. type(v))
            end
        end
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let prints = drain_prints(&cmds);
    assert!(
        prints.iter().any(|s| s == "ok-nil"),
        "signals.<missing>.get() must return nil; got prints: {prints:?}"
    );
}

#[test]
fn signals_colon_call_form_also_works() {
    // Lua supports both `signals.x.set(v)` (dot) and `signals.x:set(v)`
    // (colon) - the value is the last argument either way.
    run_on_load(
        r#"
        function on_load()
            signals.chain_colon_count:set(11)
        end
        "#,
    );
    let value = read_snapshot("chain_colon_count");
    assert!(
        matches!(value, Some(PropertyValue::I64(11))),
        "colon-form signals.x:set(11) must also route the typed write; got {value:?}"
    );
}
