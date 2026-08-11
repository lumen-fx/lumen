//! Typed signal builtins (parity with the Rhai host's `typed_signals`
//! test): each (set, get) pair round-trips through the engine.

#![allow(deprecated)]

use lumen_script::ScriptCommand;
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
fn signal_set_int_get_int_round_trip() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            signal_set_int("n", 42)
            local v = signal_get_int("n")
            print("n=" .. v)
        end
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    assert!(drain_prints(&cmds).iter().any(|s| s == "n=42"));
}

#[test]
fn signal_set_float_get_float_round_trip() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            signal_set_float("amount", 3.5)
            local v = signal_get_float("amount")
            print("amount=" .. v)
        end
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
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            signal_set_bool("flag", true)
            local v = signal_get_bool("flag")
            print("flag=" .. tostring(v))
        end
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    assert!(drain_prints(&cmds).iter().any(|s| s == "flag=true"));
}

#[test]
fn signal_get_int_missing_returns_nil() {
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            local v = signal_get_int("never_set")
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
    assert!(drain_prints(&cmds).iter().any(|s| s == "ok-nil"));
}

#[test]
fn signal_set_color_get_color_round_trip() {
    let mut host = LuaHost::new();
    host.load(
        r##"
        function on_load()
            signal_set_color("accent", "#ff8800")
            local m = signal_get_color("accent")
            print("r=" .. m.r .. " g=" .. m.g .. " b=" .. m.b .. " a=" .. m.a)
        end
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
fn typed_setter_does_not_emit_set_signal() {
    // Typed setters bypass the command sink entirely - they push
    // PropertyValue directly through the foundation typed-property bus.
    let mut host = LuaHost::new();
    host.load(
        r#"
        function on_load()
            signal_set_int("n_no_mirror", 7)
        end
        "#,
    )
    .expect("load");
    let cmds = host.call_event("on_load", &[]).expect("call");
    let legacy_mirror: Vec<_> = cmds
        .iter()
        .filter(|c| matches!(c, ScriptCommand::SetSignal { .. }))
        .collect();
    assert!(
        legacy_mirror.is_empty(),
        "typed setter must NOT queue a SetSignal mirror; got {legacy_mirror:?}"
    );
}

#[test]
fn typed_setter_writes_to_property_store_via_plugin_tick() {
    use lumen_core::app::{App, Tick};
    use lumen_core::property_store::{Property, PropertyStore};
    use lumen_core::signals::Signals;
    use lumen_script_lua::ScriptLuaPlugin;
    let mut app = App::new();
    app.world.init_resource::<Signals>();
    app.add_plugin(ScriptLuaPlugin::new(
        r#"
function on_start()
    signal_set_int("typed_n_e2e", 99)
end
"#,
    ));
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
