//! The window the app wants, the window it has, and the `window` script
//! namespace's mirror of the latter. The mirror is process-wide and every app
//! tick may write it, so these run in a binary of their own, one at a time,
//! where no other test's app can tick in between.

use std::sync::Mutex;

use bevy_ecs::message::{MessageCursor, Messages};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::input::{CloseReason, CloseRequest};
use lumen_core::render_world::Viewport;
use lumen_core::tick::TickStage;
use lumen_core::window::{QuitRequest, Window, WindowRequest, primary_window};
use lumen_core::window_backend::{
    CloseRequests, HeadlessWindowPlugin, WindowCorePlugin, run_close_request, seed_primary_window,
};
use lumen_core::window_state::{dpr, primary_handle, size, title};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// An app with a window and no display: the headless runners' setup.
fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugin(WindowCorePlugin);
    app.add_plugin(HeadlessWindowPlugin);
    let size = app.world.resource::<Viewport>().size;
    seed_primary_window(&mut app.world, "Start", size);
    app
}

fn wanted(app: &mut App) -> Mut<'_, Window> {
    let window = primary_window(&mut app.world).expect("the app has a window");
    app.world.get_mut::<Window>(window).expect("a Window")
}

/// The getters answer the viewport the app runs at, from the first tick on,
/// and follow it when it changes.
#[test]
fn the_getters_follow_the_viewport() {
    let _guard = serial();
    let mut app = App::new();
    {
        let mut viewport = app.world.resource_mut::<Viewport>();
        viewport.size = Vec2::new(320.0, 200.0);
        viewport.scale_factor = 3.0;
    }
    app.tick();
    assert_eq!(size(), (320.0, 200.0));
    assert_eq!(dpr(), 3.0);

    app.world.resource_mut::<Viewport>().size = Vec2::new(800.0, 600.0);
    app.tick();
    assert_eq!(size(), (800.0, 600.0));
}

/// The window-free half gives the app one window entity, seeded with what
/// the driver opened, and publishes its handle and title.
#[test]
fn the_core_plugin_spawns_the_primary_window() {
    let _guard = serial();
    let mut app = headless_app();
    let window = primary_window(&mut app.world).expect("a primary window");
    assert_eq!(primary_handle(), window.to_bits());
    let w = app.world.get::<Window>(window).unwrap();
    assert_eq!(w.title, "Start");
    assert_eq!(w.size, app.world.resource::<Viewport>().size);
    assert_eq!(title(), "Start");
}

/// With no platform window in between, a changed title and size are what the
/// window has once the tick ran: the viewport of both worlds, and the mirror
/// the getters read.
#[test]
fn a_headless_window_takes_its_changes_on_the_tick() {
    let _guard = serial();
    let mut app = headless_app();
    app.tick();
    {
        let mut w = wanted(&mut app);
        w.title = "Docs".into();
        w.size = Vec2::new(640.0, 480.0);
    }
    app.tick();
    assert_eq!(title(), "Docs");
    assert_eq!(size(), (640.0, 480.0));
    assert_eq!(
        app.world.resource::<Viewport>().size,
        Vec2::new(640.0, 480.0)
    );
    assert_eq!(
        app.render_world.resource::<Viewport>().size,
        Vec2::new(640.0, 480.0),
        "an offscreen renderer draws at the new size"
    );
}

/// A viewport sized from outside the window, the way a test or a runner
/// sizes it after the app is built, is the size the window has: the first
/// tick keeps it, and a later title change does not undo it.
#[test]
fn a_headless_window_follows_a_viewport_set_from_outside() {
    let _guard = serial();
    let mut app = headless_app();
    app.world.resource_mut::<Viewport>().size = Vec2::new(320.0, 200.0);
    app.tick();
    assert_eq!(
        app.world.resource::<Viewport>().size,
        Vec2::new(320.0, 200.0)
    );
    assert_eq!(wanted(&mut app).size, Vec2::new(320.0, 200.0));
    wanted(&mut app).title = "Renamed".into();
    app.tick();
    assert_eq!(
        app.world.resource::<Viewport>().size,
        Vec2::new(320.0, 200.0)
    );
    assert_eq!(title(), "Renamed");
}

/// A size no window can have changes nothing.
#[test]
fn a_headless_window_keeps_its_size_for_an_impossible_one() {
    let _guard = serial();
    let mut app = headless_app();
    app.tick();
    let before = app.world.resource::<Viewport>().size;
    wanted(&mut app).size = Vec2::new(0.0, -5.0);
    app.tick();
    assert_eq!(app.world.resource::<Viewport>().size, before);
}

/// The mirror reports what the window has, not what was asked: with a
/// window backend between the app and the window, nothing has applied the
/// new title yet, so the getter still reads the old one.
#[test]
fn the_mirror_waits_for_the_window() {
    let _guard = serial();
    let mut app = App::new();
    app.add_plugin(WindowCorePlugin);
    let size = app.world.resource::<Viewport>().size;
    seed_primary_window(&mut app.world, "Opened", size);
    app.tick();
    wanted(&mut app).title = "Asked".into();
    wanted(&mut app).size = Vec2::new(1000.0, 900.0);
    app.tick();
    assert_eq!(title(), "Opened");
    assert_eq!(super_size(&app), size);
}

fn super_size(app: &App) -> Vec2 {
    app.world.resource::<Viewport>().size
}

/// Vetoes any close request it sees, recording the reasons.
#[derive(Resource, Default)]
struct Seen(Vec<CloseReason>);

fn veto_all(
    mut msgs: ResMut<Messages<CloseRequest>>,
    mut cursor: Local<MessageCursor<CloseRequest>>,
    mut seen: ResMut<Seen>,
) {
    let reasons: Vec<_> = cursor
        .read(&msgs)
        .filter(|m| !m.vetoed)
        .map(|m| m.reason)
        .collect();
    for reason in reasons {
        seen.0.push(reason);
        msgs.write(CloseRequest {
            reason,
            vetoed: true,
        });
    }
}

/// `window.close()` reaches the driver as a close request, and the veto tick
/// lets a hook keep the window open; with no hook the close goes ahead.
#[test]
fn a_window_close_runs_the_veto_path() {
    let _guard = serial();
    let mut app = headless_app();
    let mut requests = CloseRequests::default();
    app.tick();
    assert_eq!(requests.take(&app.world), None);

    app.world.write_message(WindowRequest::Close);
    app.tick();
    assert_eq!(requests.take(&app.world), Some(CloseReason::WindowClose));
    assert!(
        run_close_request(&mut app, &mut requests, CloseReason::WindowClose),
        "nothing vetoed, so the close goes ahead"
    );

    app.world.init_resource::<Seen>();
    app.add_systems(TickStage::Systems, veto_all);
    assert!(
        !run_close_request(&mut app, &mut requests, CloseReason::WindowClose),
        "the hook kept the window open"
    );
    assert_eq!(app.world.resource::<Seen>().0, [CloseReason::WindowClose]);
}

/// A quit in the same tick as a window close wins: it is the larger request.
#[test]
fn a_quit_outranks_a_window_close() {
    let _guard = serial();
    let mut app = headless_app();
    let mut requests = CloseRequests::default();
    app.world.write_message(WindowRequest::Close);
    app.world.write_message(QuitRequest);
    app.tick();
    assert_eq!(requests.take(&app.world), Some(CloseReason::AppQuit));
    assert_eq!(requests.take(&app.world), None, "each request is read once");
}

/// A hook that asks for a close while answering one starts no second round.
#[test]
fn a_close_asked_during_the_veto_tick_is_dropped() {
    let _guard = serial();
    let mut app = headless_app();
    let mut requests = CloseRequests::default();
    app.add_systems(
        TickStage::Systems,
        |closes: MessageReader<CloseRequest>, mut out: MessageWriter<WindowRequest>| {
            if !closes.is_empty() {
                out.write(WindowRequest::Close);
            }
        },
    );
    assert!(run_close_request(
        &mut app,
        &mut requests,
        CloseReason::Platform
    ));
    app.tick();
    assert_eq!(requests.take(&app.world), None);
}
