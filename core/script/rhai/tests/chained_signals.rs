//! Chained `signals.foo.set(v)` access (round-X). Replaces the
//! procedural `signal_set_int(name, value)` / `signal_get_int(name)` /
//! float / bool / color builtins with a chained idiom that joins
//! property + index segments into a single `PropertyKey::Global` and
//! routes through the same `push_external_property` typed bus the
//! legacy builtins use.
//!
//! Six tests exercise the typed setters (i64 / f64 / bool / color via
//! explicit `set_color`), the dot-joined nested path, and the UNIT
//! fallback on a miss.
//!
//! Verification path: each typed setter writes both into the Rhai
//! host-local mirror AND through the process-wide
//! `push_external_property` bus. The bus is a FIFO channel - parallel
//! test threads would race on the drain - so the tests below verify
//! through the channel's snapshot (`external_property_snapshot`), which
//! re-sends each entry after reading so other tests still observe
//! their writes. Unique key names per test (`chain_int_count`,
//! `chain_float_amount`, ...) keep readers from mis-attributing
//! cross-test writes.

use lumen_core::components::Color;
use lumen_core::property_store::{PropertyKey, PropertyValue, external_property_snapshot};
use lumen_script::ScriptCommand;
use lumen_script_rhai::RhaiHost;
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
    let mut host = RhaiHost::new();
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
        fn on_load() {
            signals.chain_int_count.set(42);
        }
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
        fn on_load() {
            signals.chain_float_amount.set(3.5);
        }
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
        fn on_load() {
            signals.chain_bool_flag.set(true);
        }
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
    // `signals.chain_user.name.set("Alice")` joins `["chain_user",
    // "name"]` -> PropertyKey::Global("chain_user.name"). The
    // PropertyValue is `Str` because the rhs is a string literal
    // (auto-detection of hex was deliberately not implemented; use
    // `set_color` for colors).
    run_on_load(
        r#"
        fn on_load() {
            signals.chain_user.name.set("Alice");
        }
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
    // Hex literals are opt-in via `set_color`; bare `set("#ff8800")`
    // lands as a Str so the auto-detection ambiguity ("is this a hex
    // colour or a plain string starting with '#'?") never surfaces.
    run_on_load(
        r##"
        fn on_load() {
            signals.chain_bg.set_color("#ff8800");
        }
        "##,
    );
    let value = read_snapshot("chain_bg");
    let color: Color = match value {
        Some(PropertyValue::Color(c)) => c,
        other => panic!("expected PropertyValue::Color, got {other:?}"),
    };
    // 0xff / 255 == 1.0, 0x88 / 255 ~ 0.533, 0x00 / 255 == 0.0
    let bytes = color.to_rgba8();
    assert_eq!(
        bytes,
        [0xff, 0x88, 0x00, 0xff],
        "set_color must store the parsed RGBA bytes verbatim"
    );
}

#[test]
fn signals_get_returns_unit_on_miss() {
    // `signals.never_set.get()` with no prior write returns Rhai UNIT
    // (the `()` type). Matches the legacy `signal_get_int(name)` miss
    // semantics; lets scripts pattern-match against `type_of(v) == "()"`.
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            let v = signals.chain_never_set_xyz.get();
            if type_of(v) == "()" {
                print("ok-unit");
            } else {
                print("wrong-type: " + type_of(v));
            }
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let prints = drain_prints(&cmds);
    assert!(
        prints.iter().any(|s| s == "ok-unit"),
        "signals.<missing>.get() must return UNIT; got prints: {prints:?}"
    );
}
