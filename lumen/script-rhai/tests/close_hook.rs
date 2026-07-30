//! Headless coverage for the `on_close()` lifecycle hook (the shutdown
//! counterpart of `on_start`).
//!
//! The window backend emits `CloseRequest { vetoed: false }` on the OS
//! close request (window button; Unix SIGINT/SIGTERM) and runs one veto
//! tick before tearing anything down. These tests replicate exactly that
//! protocol against a headless `App`: write the message, tick, then
//! inspect the current-update buffer the same way
//! `lumen-window-winit`'s `process_close_request` does.

use bevy_ecs::message::Messages;
use lumen_core::app::App;
use lumen_core::input::CloseRequest;
use lumen_script::{ScriptContext, ScriptValue};
use lumen_script_rhai::{RhaiHost, ScriptRhaiPlugin};

/// Emit the backend-shaped close request.
fn write_close_request(app: &mut App) {
    app.world
        .resource_mut::<Messages<CloseRequest>>()
        .write(CloseRequest { vetoed: false });
}

/// The backend's post-tick veto check, verbatim: any
/// `CloseRequest { vetoed: true }` written during the tick keeps the
/// window open.
fn vetoed_this_update(app: &mut App) -> bool {
    app.world
        .resource::<Messages<CloseRequest>>()
        .iter_current_update_messages()
        .any(|m| m.vetoed)
}

#[test]
fn on_close_fires_before_shutdown() {
    let mut app = App::new();
    app.add_plugin(ScriptRhaiPlugin::new(
        r#"
fn on_close() {
    let closed = signal("closed", false);
    closed.set(true);
}
"#,
    ));
    app.tick();

    // Hook has not fired yet.
    {
        let mut host = app.world.resource_mut::<RhaiHost>();
        assert_ne!(
            host.root_context().get("closed"),
            Some(ScriptValue::Bool(true)),
            "on_close must not fire before a close request"
        );
    }

    write_close_request(&mut app);
    app.tick();

    let mut host = app.world.resource_mut::<RhaiHost>();
    assert_eq!(
        host.root_context().get("closed"),
        Some(ScriptValue::Bool(true)),
        "on_close must fire on the veto tick that follows a close request"
    );
}

#[test]
fn on_close_without_veto_lets_close_proceed() {
    let mut app = App::new();
    app.add_plugin(ScriptRhaiPlugin::new(
        r#"
fn on_close() {
    let closed = signal("closed", false);
    closed.set(true);
}
"#,
    ));
    app.tick();
    write_close_request(&mut app);
    app.tick();
    assert!(
        !vetoed_this_update(&mut app),
        "an on_close that does not return false must not veto the close"
    );
}

#[test]
fn on_close_returning_false_vetoes_close() {
    let mut app = App::new();
    app.add_plugin(ScriptRhaiPlugin::new(
        r#"
fn on_close() {
    false
}
"#,
    ));
    app.tick();

    write_close_request(&mut app);
    app.tick();
    assert!(
        vetoed_this_update(&mut app),
        "on_close returning false must write CloseRequest {{ vetoed: true }}"
    );

    // The veto response itself must not re-trigger the hook, and a later
    // close request must fire (and veto) again.
    app.tick();
    assert!(
        !vetoed_this_update(&mut app),
        "no veto without a fresh close request"
    );
    write_close_request(&mut app);
    app.tick();
    assert!(
        vetoed_this_update(&mut app),
        "a second close request must run the hook again"
    );
}

#[test]
fn missing_on_close_allows_close() {
    let mut app = App::new();
    app.add_plugin(ScriptRhaiPlugin::new("fn on_start() {}"));
    app.tick();
    write_close_request(&mut app);
    app.tick();
    assert!(
        !vetoed_this_update(&mut app),
        "a script without on_close must not veto the close"
    );
}
