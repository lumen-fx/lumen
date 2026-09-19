//! `derive(name, deps, fn)` end-to-end (parity with the Rhai host's
//! `derive` test): a script-side signal write marks the dep dirty ->
//! `apply_derivations` re-runs the closure -> derived signal lands in
//! the `Signals` resource.

#![allow(deprecated)]

use bevy_ecs::message::Messages;
use lumen_core::app::{App, Tick};
use lumen_core::signals::Signals;
use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_lua::{LuaHost, ScriptCommandEvent, ScriptLuaPlugin};

fn drain_script_commands(app: &mut App) {
    let cmds: Vec<ScriptCommand> = {
        let messages = app.world.resource::<Messages<ScriptCommandEvent>>();
        let mut reader = messages.get_cursor();
        reader.read(messages).map(|ev| ev.0.clone()).collect()
    };
    let mut signals = app.world.resource_mut::<Signals>();
    for cmd in cmds {
        if let ScriptCommand::SetSignal { name, value } = cmd {
            signals.set(name, value);
        }
    }
}

fn call_script_fn(app: &mut App, fn_name: &str, arg: &str) {
    let mut host = app.world.resource_mut::<LuaHost>();
    match host.call_event(fn_name, &[ScriptValue::Str(arg.to_string())]) {
        Ok(cmds) => host.push_commands_back(cmds),
        Err(e) => panic!("call_event('{fn_name}') failed: {e}"),
    }
}

#[test]
fn derive_emits_initial_value_then_reacts_to_dep_writes() {
    let mut app = App::new();
    app.world.insert_resource(Signals::default());
    app.add_plugin(ScriptLuaPlugin::new(
        r#"
function on_start()
    local count = signal("count", 0)
    derive("doubled", {count}, function(n) return n * 2 end)
end

function bump(by)
    local count = signal("count", 0)
    count:set(count:get() + tonumber(by))
end
"#,
    ));

    // Tick 1: on_start ran during plugin install; apply_derivations hits
    // the pending_initial path and emits doubled = 0 * 2.
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    assert_eq!(
        app.world.resource::<Signals>().get("doubled"),
        Some("0"),
        "initial run should emit doubled=0"
    );

    call_script_fn(&mut app, "bump", "5");
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    assert_eq!(
        app.world.resource::<Signals>().get("doubled"),
        Some("10"),
        "derive should react to count=5 within one tick of the write"
    );
}
