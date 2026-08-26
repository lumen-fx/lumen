//! The other direction: what a plugin pushes at the app, from its own
//! thread and from inside a call.

mod common;

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use common::{ChannelHooks, app_dir, env, fixture_module, install_fixture};
use lumen_core::app::App;
use lumen_plugin::abi::LogLevel;
use lumen_plugin::{HostHooks, PluginEvent, PluginSet, ScriptCommand, ScriptValue};

#[test]
fn a_plugins_thread_delivers_its_events_and_asks_for_a_tick() {
    let dir = app_dir("events-thread");
    let (hooks, rx) = ChannelHooks::new();
    let (set, failures) = PluginSet::load(
        &[fixture_module("events-thread", "thread_events = 5")],
        &env(&dir),
        Arc::clone(&hooks) as Arc<dyn HostHooks>,
    );
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(set.len(), 1);

    for i in 0..5 {
        let event = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("event {i}: {e}"));
        match event {
            PluginEvent::Call {
                event,
                key,
                fallback,
                args,
            } => {
                assert_eq!(event, "on_fixture");
                assert_eq!(key, format!("thread{i}"));
                assert_eq!(fallback, "on_any");
                assert_eq!(args, vec![ScriptValue::I64(i)]);
            }
            other => panic!("event {i} is not a handler call: {other:?}"),
        }
    }
    assert!(hooks.wakes() >= 1, "an event without a wake sleeps");
    assert!(
        matches!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ),
        "the thread stopped after what it was asked for"
    );
}

#[test]
fn a_thread_that_emits_commands_delivers_them_the_same_way() {
    let dir = app_dir("events-commands");
    let (hooks, rx) = ChannelHooks::new();
    let (_set, failures) = PluginSet::load(
        &[fixture_module("events-commands", "thread_commands = 2")],
        &env(&dir),
        Arc::clone(&hooks) as Arc<dyn HostHooks>,
    );
    assert!(failures.is_empty(), "{failures:?}");
    for i in 0..2 {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            PluginEvent::Commands(commands) => {
                assert!(
                    matches!(&commands[..], [ScriptCommand::Print(s)] if *s == format!("thread command {i}")),
                    "{commands:?}"
                );
            }
            other => panic!("not commands: {other:?}"),
        }
    }
}

#[test]
fn an_event_pushed_from_inside_a_call_reaches_the_host() {
    let plugin = install_fixture("events-in-call", "");
    assert_eq!(
        plugin
            .call("fixture_event", &[ScriptValue::Str("k".into())])
            .0,
        Ok(ScriptValue::Bool(true))
    );
    let events = plugin.hooks.events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].0, "lumen-plugin-fixture",
        "the event names its module"
    );
    assert!(matches!(
        &events[0].1,
        PluginEvent::Call { event, key, .. } if event == "on_fixture" && key == "k"
    ));

    assert_eq!(
        plugin.call("fixture_commands", &[]).0,
        Ok(ScriptValue::Bool(true))
    );
    assert!(matches!(
        plugin.hooks.events()[1].1,
        PluginEvent::Commands(_)
    ));

    assert_eq!(
        plugin
            .call("fixture_log", &[ScriptValue::Str("careful".into())])
            .0,
        Ok(ScriptValue::Unit)
    );
    let logs = plugin.hooks.logs();
    assert_eq!(logs[0].1, LogLevel::Warn);
    assert_eq!(logs[0].2, "careful");
}

#[test]
fn emitting_after_the_host_hung_up_reports_it_rather_than_crashing() {
    let dir = app_dir("events-hangup");
    let (hooks, rx) = ChannelHooks::new();
    let (set, failures) = PluginSet::load(
        &[fixture_module("events-hangup", "")],
        &env(&dir),
        Arc::clone(&hooks) as Arc<dyn HostHooks>,
    );
    assert!(failures.is_empty(), "{failures:?}");
    let mut app = App::new();
    set.install(&mut app);

    drop(rx);
    let call = |name: &str, args: &[ScriptValue]| {
        app.world
            .resource::<lumen_script::ScriptFnRegistry>()
            .fns()
            .iter()
            .find(|f| f.name == name)
            .expect("the function is bound")
            .invoke(args)
            .0
    };
    assert_eq!(
        call("fixture_event", &[ScriptValue::Str("k".into())]),
        Ok(ScriptValue::Bool(false)),
        "a plugin learns the engine stopped taking events"
    );
    assert_eq!(call("fixture_commands", &[]), Ok(ScriptValue::Bool(false)));
}
