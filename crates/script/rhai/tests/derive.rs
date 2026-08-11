//! Integration test for the `derive(name, deps, fn)` Rhai builtin.
//!
//! Exercises the reactive loop end-to-end: a script-side signal write
//! marks `Signals::dirty` -> `apply_derivations` re-runs the closure ->
//! derived signal value lands in the `Signals` resource.

#![allow(deprecated)]
use bevy_ecs::message::Messages;
use lumen_core::app::{App, Tick};
use lumen_core::signals::Signals;
use lumen_script::{ScriptCommand, ScriptValue};
use lumen_script_rhai::{RhaiHost, ScriptCommandEvent, ScriptRhaiPlugin};

/// Drain `ScriptCommandEvent`s into `Signals` (the lumenc runtime
/// normally does this via `apply_script_commands`; bare unit tests
/// don't install that system).
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

/// Invoke a script-side function on the host and immediately drain
/// any commands it queued (so the next tick sees the writes).
fn call_script_fn(app: &mut App, fn_name: &str, arg: &str) {
    let mut host = app.world.resource_mut::<RhaiHost>();
    match host.call_event(fn_name, &[ScriptValue::Str(arg.to_string())]) {
        Ok(cmds) => host.push_commands_back(cmds),
        Err(e) => panic!("call_event('{fn_name}') failed: {e}"),
    }
}

#[test]
fn derive_emits_initial_value_then_reacts_to_dep_writes() {
    let mut app = App::new();
    app.world.insert_resource(Signals::default());
    app.add_plugin(ScriptRhaiPlugin::new(
        r#"
fn on_start() {
    let count = signal("count", 0);
    derive("doubled", [count], |n| n * 2);
}

fn bump(by) {
    let count = signal("count", 0);
    count.set(count.get() + parse_int(by));
}
"#,
    ));

    // Tick 1: on_start ran during plugin install. apply_derivations
    // hits the pending_initial path and emits the first SetSignal for
    // "doubled" (= 0 * 2).
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    assert_eq!(
        app.world.resource::<Signals>().get("doubled"),
        Some("0"),
        "initial run should emit doubled=0"
    );

    // Run bump(5). It calls count.set(5) which (a) updates the host
    // mirror immediately and (b) emits a ScriptCommand::SetSignal.
    call_script_fn(&mut app, "bump", "5");
    // Run the schedule so tick_script flushes those queued cmds and
    // apply_script_commands (here drain_script_commands) writes
    // count=5 into ECS Signals. This also marks count dirty.
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    // count is now "5" in Signals. The next tick's apply_derivations
    // should re-run the closure with count=5.
    app.world.run_schedule(Tick);
    drain_script_commands(&mut app);
    assert_eq!(
        app.world.resource::<Signals>().get("doubled"),
        Some("10"),
        "derive should react to count=5 within one tick of the write"
    );
}
