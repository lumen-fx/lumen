//! Headless proof: the `counter.lua` sample (a port of the counter
//! app's inline Rhai `<script>`) drives the SAME host-generic runtime
//! through 60 ticks with the Lua engine, and a click handler firing
//! updates a reactive signal + its derived label.
//!
//! This is the crate-level equivalent of `lumenc run <app> --headless
//! --ticks 60` with the Lua engine: a windowless `App` running the real
//! `ScriptPlugin<LuaHost>` system set (derivations, command drain, ...),
//! ticked repeatedly, with clicks injected the way `lumen-input`'s
//! dispatcher would forward them to `on_click`.

#![allow(deprecated)]

use bevy_ecs::message::Messages;
use lumen_core::app::{App, Tick};
use lumen_core::signals::Signals;
use lumen_script::ScriptCommand;
use lumen_script_lua::{LuaHost, ScriptCommandEvent, ScriptLuaPlugin};

/// Drain the tick's `ScriptCommandEvent`s into the `Signals` resource -
/// what lumenc's `apply_script_commands` does in production.
fn drain_into_signals(app: &mut App) {
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

/// Fire `on_click(id)` on the host the way `dispatch_clicks_and_doubles`
/// would after a pointer release, then re-stash its commands so the next
/// `tick_script` flushes them onto the event bus.
fn inject_click(app: &mut App) {
    let mut host = app.world.resource_mut::<LuaHost>();
    let cmds = host
        .call_event("on_click", &[lumen_script::ScriptValue::Str("tile".into())])
        .expect("on_click");
    host.push_commands_back(cmds);
}

#[test]
fn counter_lua_runs_headless_60_ticks_and_reacts_to_clicks() {
    let mut app = App::new();
    app.world.insert_resource(Signals::default());
    app.add_plugin(ScriptLuaPlugin::new(include_str!("counter.lua")));

    // 60 windowless ticks. Inject three clicks along the way; each must
    // bump the `clicks` signal and re-derive `counter_label`.
    let click_ticks = [10u32, 25, 40];
    for tick in 0..60u32 {
        if click_ticks.contains(&tick) {
            inject_click(&mut app);
        }
        app.world.run_schedule(Tick);
        drain_into_signals(&mut app);
    }

    let signals = app.world.resource::<Signals>();
    assert_eq!(
        signals.get("clicks"),
        Some("3"),
        "three injected clicks must leave clicks=3"
    );
    assert_eq!(
        signals.get("counter_label"),
        Some("Lumen - clicks: 3"),
        "the derived counter_label must track the clicks signal"
    );
}
