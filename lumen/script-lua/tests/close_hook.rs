//! `on_close()` lifecycle hook (parity with the Rhai host's `close_hook`
//! test): the veto tick runs the hook, and returning `false` writes
//! `CloseRequest { vetoed: true }`.

use bevy_ecs::message::Messages;
use lumen_core::app::App;
use lumen_core::input::CloseRequest;
use lumen_script::{ScriptContext, ScriptValue};
use lumen_script_lua::{LuaHost, ScriptLuaPlugin};

fn write_close_request(app: &mut App) {
    app.world
        .resource_mut::<Messages<CloseRequest>>()
        .write(CloseRequest { vetoed: false });
}

fn vetoed_this_update(app: &mut App) -> bool {
    app.world
        .resource::<Messages<CloseRequest>>()
        .iter_current_update_messages()
        .any(|m| m.vetoed)
}

#[test]
fn on_close_fires_before_shutdown() {
    let mut app = App::new();
    app.add_plugin(ScriptLuaPlugin::new(
        r#"
function on_close()
    local closed = signal("closed", false)
    closed:set(true)
end
"#,
    ));
    app.tick();

    {
        let mut host = app.world.resource_mut::<LuaHost>();
        assert_ne!(
            host.root_context().get("closed"),
            Some(ScriptValue::Bool(true)),
            "on_close must not fire before a close request"
        );
    }

    write_close_request(&mut app);
    app.tick();

    let mut host = app.world.resource_mut::<LuaHost>();
    assert_eq!(
        host.root_context().get("closed"),
        Some(ScriptValue::Bool(true)),
        "on_close must fire on the veto tick that follows a close request"
    );
}

#[test]
fn on_close_without_veto_lets_close_proceed() {
    let mut app = App::new();
    app.add_plugin(ScriptLuaPlugin::new(
        r#"
function on_close()
    local closed = signal("closed", false)
    closed:set(true)
end
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
    app.add_plugin(ScriptLuaPlugin::new(
        r#"
function on_close()
    return false
end
"#,
    ));
    app.tick();

    write_close_request(&mut app);
    app.tick();
    assert!(
        vetoed_this_update(&mut app),
        "on_close returning false must write CloseRequest {{ vetoed: true }}"
    );

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
    app.add_plugin(ScriptLuaPlugin::new("function on_start() end"));
    app.tick();
    write_close_request(&mut app);
    app.tick();
    assert!(
        !vetoed_this_update(&mut app),
        "a script without on_close must not veto the close"
    );
}
