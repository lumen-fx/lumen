//! W7.x typed signal Rhai builtins. The procedural `signal_set_int` /
//! `signal_get_int` / float / bool / color variants land alongside the
//! existing string-typed `signal(name, default)` handle so embedders /
//! script authors can avoid stringifying every value at every read.
//!
//! Each test loads a tiny script that exercises one (set, get) pair
//! through the engine and asserts the round-trip behaves as documented.

#![allow(deprecated)]

use lumen_script::ScriptCommand;
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
fn signal_set_int_get_int_round_trip() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            signal_set_int("n", 42);
            let v = signal_get_int("n");
            print("n=" + v);
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    assert!(drain_prints(&cmds).iter().any(|s| s == "n=42"));
}

#[test]
fn signal_set_float_get_float_round_trip() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            signal_set_float("amount", 3.5);
            let v = signal_get_float("amount");
            print("amount=" + v);
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let prints = drain_prints(&cmds);
    assert!(
        prints.iter().any(|s| s.starts_with("amount=3.5")),
        "got prints: {prints:?}"
    );
}

#[test]
fn signal_set_bool_get_bool_round_trip() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            signal_set_bool("flag", true);
            let v = signal_get_bool("flag");
            print("flag=" + v);
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    assert!(drain_prints(&cmds).iter().any(|s| s == "flag=true"));
}

#[test]
fn signal_get_int_missing_returns_unit() {
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            let v = signal_get_int("never_set");
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
    assert!(drain_prints(&cmds).iter().any(|s| s == "ok-unit"));
}

#[test]
fn signal_set_color_get_color_round_trip() {
    let mut host = RhaiHost::new();
    host.load(
        r##"
        fn on_load() {
            signal_set_color("accent", "#ff8800");
            let m = signal_get_color("accent");
            print("r=" + m.r + " g=" + m.g + " b=" + m.b + " a=" + m.a);
        }
        "##,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let prints = drain_prints(&cmds);
    assert!(
        prints.iter().any(|s| s == "r=255 g=136 b=0 a=255"),
        "got prints: {prints:?}"
    );
}

#[test]
fn signal_set_color_survives_arbitrary_input() {
    // Script input is arbitrary: a multi-byte string whose byte length
    // matches a hex shape used to panic the shared parser on a char
    // boundary. It must be a quiet no-op instead.
    let mut host = RhaiHost::new();
    // Two euro signs: six bytes, so the length check matches `rrggbb` while
    // byte index 2 sits inside a character.
    let euro = '\u{20ac}';
    host.load(&format!(
        r##"
        fn on_load() {{
            signal_set_color("accent", "{euro}{euro}");
            print("still-running");
        }}
        "##
    ))
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    assert!(drain_prints(&cmds).iter().any(|s| s == "still-running"));
}

#[test]
fn signal_set_int_does_not_emit_set_signal_anymore() {
    // Round 4: typed setters bypass the Rhai `ScriptCommand` sink
    // entirely - they push `PropertyValue::I64` directly through the
    // foundation typed-property bus. No `SetSignal` round-trip; no
    // `SetProperty` variant on the ScriptCommand boundary (that would
    // break the lumenc match without a wildcard arm).
    let mut host = RhaiHost::new();
    host.load(
        r#"
        fn on_load() {
            signal_set_int("n_no_mirror", 7);
        }
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let legacy_mirror: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            ScriptCommand::SetSignal { name, value } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    assert!(
        legacy_mirror.is_empty(),
        "typed setter must NOT queue legacy SetSignal mirror; got {legacy_mirror:?}"
    );
}

#[test]
fn typed_setter_writes_to_property_store_via_plugin_tick() {
    // End-to-end: install the ScriptRhaiPlugin, run a tick, observe that
    // the foundation PropertyStore holds the typed `PropertyValue::I64`
    // - and that the legacy `Signals` resource was not touched. The
    // typed write travels through `push_external_property` -> tick drain
    // -> `PropertyStore::set`.
    use lumen_core::app::{App, Tick};
    use lumen_core::property_store::{Property, PropertyStore};
    use lumen_core::signals::Signals;
    use lumen_script_rhai::ScriptRhaiPlugin;
    let mut app = App::new();
    app.world.init_resource::<Signals>();
    app.add_plugin(ScriptRhaiPlugin::new(
        r#"
fn on_start() {
    signal_set_int("typed_n_e2e", 99);
}
"#,
    ));
    // One tick is enough: on_start ran during plugin install and pushed
    // through the bus; the per-tick `drain_external_properties` system
    // picks it up on the very first schedule run.
    app.world.run_schedule(Tick);
    let store = app.world.resource::<PropertyStore>();
    let n: Property<i64> = Property::new("typed_n_e2e");
    assert_eq!(
        n.get(store),
        Some(99),
        "typed setter must populate PropertyStore with PropertyValue::I64"
    );
    let signals = app.world.resource::<Signals>();
    assert!(
        signals.get("typed_n_e2e").is_none(),
        "typed setter must NOT write to legacy Signals; got {:?}",
        signals.get("typed_n_e2e")
    );
}
