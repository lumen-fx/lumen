//! The windows the app wants, the windows it has, and the `window` script
//! namespace's mirror of the latter. The mirror is process-wide and every app
//! tick may write it, so these run in a binary of their own, one at a time,
//! where no other test's app can tick in between.

use std::sync::Mutex;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::{MessageCursor, Messages};
use bevy_ecs::prelude::*;
use glam::{IVec2, UVec2, Vec2};
use lumen_core::app::App;
use lumen_core::input::{CloseReason, CloseRequest, FocusTracker, Focused, WindowFocused};
use lumen_core::tick::TickStage;
use lumen_core::window::{
    InputBlocked, LastFocus, MonitorInfo, Monitors, QuitPolicy, QuitRequest, Window, WindowClosing,
    WindowOf, WindowPartitions, WindowRequest, WindowRuntime, WindowViewport, primary_viewport,
    primary_viewport_mut, primary_window,
};
use lumen_core::window_backend::{
    CloseAsk, CloseOutcome, CloseRequests, HeadlessDesktop, HeadlessWindowPlugin, WindowCorePlugin,
    run_close_ask, run_close_request, seed_window,
};
use lumen_core::window_state::{
    current, dpr, dpr_of, primary_handle, resolve, set_current, size, size_of, title, title_of,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// An app with a window and no display: the headless runners' setup.
fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugin(WindowCorePlugin);
    app.add_plugin(HeadlessWindowPlugin);
    let primary = primary_window(&mut app.world).expect("a primary window");
    let size = primary_viewport(&app.world).unwrap().size;
    seed_window(&mut app.world, primary, "Start", size);
    app
}

fn wanted(app: &mut App) -> Mut<'_, Window> {
    let window = primary_window(&mut app.world).expect("the app has a window");
    app.world.get_mut::<Window>(window).expect("a Window")
}

fn viewport_size(app: &App) -> Vec2 {
    primary_viewport(&app.world).unwrap().size
}

/// The getters answer the viewport the app runs at, from the first tick on,
/// and follow it when it changes.
#[test]
fn the_getters_follow_the_viewport() {
    let _guard = serial();
    let mut app = App::new();
    {
        let mut viewport = primary_viewport_mut(&mut app.world);
        viewport.size = Vec2::new(320.0, 200.0);
        viewport.scale_factor = 3.0;
    }
    app.tick();
    assert_eq!(size(), (320.0, 200.0));
    assert_eq!(dpr(), 3.0);

    primary_viewport_mut(&mut app.world).size = Vec2::new(800.0, 600.0);
    app.tick();
    assert_eq!(size(), (800.0, 600.0));
}

/// Every app has its primary window from the start, whether or not anything
/// shows it; the window-free half seeds it with what the driver opened and
/// publishes its handle and title, and spawns no second one.
#[test]
fn every_app_has_one_primary_window() {
    let _guard = serial();
    let mut app = headless_app();
    app.tick();
    let window = primary_window(&mut app.world).expect("a primary window");
    assert_eq!(primary_handle(), window.to_bits());
    let w = app.world.get::<Window>(window).unwrap();
    assert_eq!(w.title, "Start");
    assert_eq!(w.size, viewport_size(&app));
    assert_eq!(title(), "Start");
    let primaries = app
        .world
        .query_filtered::<Entity, With<lumen_core::window::PrimaryWindow>>()
        .iter(&app.world)
        .count();
    assert_eq!(primaries, 1);
}

/// With no platform window in between, a changed title and size are what the
/// window has once the tick ran: the viewport, and the mirror the getters
/// read.
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
    assert_eq!(viewport_size(&app), Vec2::new(640.0, 480.0));
}

/// A viewport sized from outside the window, the way a test or a runner
/// sizes it after the app is built, is the size the window has: the first
/// tick keeps it, and a later title change does not undo it.
#[test]
fn a_headless_window_follows_a_viewport_set_from_outside() {
    let _guard = serial();
    let mut app = headless_app();
    primary_viewport_mut(&mut app.world).size = Vec2::new(320.0, 200.0);
    app.tick();
    assert_eq!(viewport_size(&app), Vec2::new(320.0, 200.0));
    assert_eq!(wanted(&mut app).size, Vec2::new(320.0, 200.0));
    wanted(&mut app).title = "Renamed".into();
    app.tick();
    assert_eq!(viewport_size(&app), Vec2::new(320.0, 200.0));
    assert_eq!(title(), "Renamed");
}

/// A size no window can have changes nothing.
#[test]
fn a_headless_window_keeps_its_size_for_an_impossible_one() {
    let _guard = serial();
    let mut app = headless_app();
    app.tick();
    let before = viewport_size(&app);
    wanted(&mut app).size = Vec2::new(0.0, -5.0);
    app.tick();
    assert_eq!(viewport_size(&app), before);
}

/// The mirror reports what the window has, not what was asked: with a
/// window backend between the app and the window, nothing has applied the
/// new title yet, so the getter still reads the old one.
#[test]
fn the_mirror_waits_for_the_window() {
    let _guard = serial();
    let mut app = App::new();
    app.add_plugin(WindowCorePlugin);
    let primary = primary_window(&mut app.world).unwrap();
    let size = viewport_size(&app);
    seed_window(&mut app.world, primary, "Opened", size);
    app.tick();
    wanted(&mut app).title = "Asked".into();
    wanted(&mut app).size = Vec2::new(1000.0, 900.0);
    app.tick();
    assert_eq!(title(), "Opened");
    assert_eq!(viewport_size(&app), size);
}

/// Vetoes any close request it sees, recording the windows and reasons.
#[derive(Resource, Default)]
struct Seen(Vec<(Entity, CloseReason)>);

/// Which windows the vetoing hook keeps open; empty keeps every one.
#[derive(Resource, Default)]
struct VetoOnly(Vec<Entity>);

fn veto(
    mut msgs: ResMut<Messages<CloseRequest>>,
    mut cursor: Local<MessageCursor<CloseRequest>>,
    mut seen: ResMut<Seen>,
    only: Res<VetoOnly>,
) {
    let asked: Vec<_> = cursor
        .read(&msgs)
        .filter(|m| !m.vetoed)
        .map(|m| (m.window, m.reason))
        .collect();
    for (window, reason) in asked {
        seen.0.push((window, reason));
        if only.0.is_empty() || only.0.contains(&window) {
            msgs.write(CloseRequest {
                window,
                reason,
                vetoed: true,
            });
        }
    }
}

fn install_veto(app: &mut App, only: Vec<Entity>) {
    app.world.init_resource::<Seen>();
    app.world.insert_resource(VetoOnly(only));
    app.add_systems(TickStage::Systems, veto);
}

/// `window.close()` reaches the driver as a close request, and the veto tick
/// lets a hook keep the window open; with no hook, closing the primary
/// window ends the app.
#[test]
fn a_window_close_runs_the_veto_path() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let mut requests = CloseRequests::default();
    app.tick();
    assert_eq!(requests.take(&app.world), None);

    app.world.write_message(WindowRequest::Close(primary));
    app.tick();
    assert_eq!(
        requests.take(&app.world),
        Some(CloseAsk::Windows(vec![primary]))
    );
    assert_eq!(
        run_close_request(
            &mut app,
            &mut requests,
            Some(primary),
            CloseReason::WindowClose
        ),
        CloseOutcome::Quit,
        "nothing vetoed, so the app ends"
    );

    install_veto(&mut app, Vec::new());
    assert_eq!(
        run_close_request(
            &mut app,
            &mut requests,
            Some(primary),
            CloseReason::WindowClose
        ),
        CloseOutcome::Vetoed,
        "the hook kept the window open"
    );
    assert_eq!(
        app.world.resource::<Seen>().0,
        [(primary, CloseReason::WindowClose)]
    );
}

/// A quit in the same tick as a window close wins: it is the larger request.
#[test]
fn a_quit_outranks_a_window_close() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let mut requests = CloseRequests::default();
    app.world.write_message(WindowRequest::Close(primary));
    app.world.write_message(QuitRequest);
    app.tick();
    assert_eq!(requests.take(&app.world), Some(CloseAsk::Quit));
    assert_eq!(requests.take(&app.world), None, "each request is read once");
}

/// A hook that asks for a close while answering one starts no second round.
#[test]
fn a_close_asked_during_the_veto_tick_is_dropped() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let mut requests = CloseRequests::default();
    app.add_systems(
        TickStage::Systems,
        move |closes: MessageReader<CloseRequest>, mut out: MessageWriter<WindowRequest>| {
            if !closes.is_empty() {
                out.write(WindowRequest::Close(primary));
            }
        },
    );
    assert_eq!(
        run_close_request(&mut app, &mut requests, None, CloseReason::Platform),
        CloseOutcome::Quit
    );
    app.tick();
    assert_eq!(requests.take(&app.world), None);
}

/// Seeding a window that no longer exists writes nothing, not even the title
/// the getter reads for it.
#[test]
fn seeding_a_window_that_went_changes_nothing() {
    let _guard = serial();
    let mut app = App::new();
    let ghost = app.world.spawn_empty().id();
    app.world.despawn(ghost);
    lumen_core::window_state::publish_title(None, "Before");
    seed_window(&mut app.world, ghost, "Ghost", Vec2::new(10.0, 10.0));
    assert!(app.world.get::<Window>(ghost).is_none());
    assert_eq!(title_of(0), "Before");
}

/// The headless stand-in carries on when its window is gone: the tick runs
/// and nothing panics.
#[test]
fn a_headless_app_runs_on_without_its_window() {
    let _guard = serial();
    let mut app = headless_app();
    app.tick();
    let window = primary_window(&mut app.world).unwrap();
    app.world.despawn(window);
    app.tick();
    assert_eq!(primary_window(&mut app.world), None);
}

/// A window spawned headless opens at the size it asks for, cascaded onto
/// the virtual desktop, takes focus from the window that had it, and its
/// title and size are what the getters read for its handle.
#[test]
fn a_second_window_opens_headless() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    app.tick();
    let second = app
        .world
        .spawn(Window::new("Second", Vec2::new(300.0, 200.0)))
        .id();
    app.tick();
    app.tick();
    let viewport = app.world.get::<WindowViewport>(second).unwrap();
    assert_eq!(viewport.size, Vec2::new(300.0, 200.0));
    let runtime = app.world.get::<WindowRuntime>(second).unwrap();
    assert!(runtime.focused, "a new window takes focus");
    assert_eq!(runtime.position, Some(IVec2::splat(32)));
    assert_eq!(runtime.monitor.as_deref(), Some("virtual-1"));
    assert!(!app.world.get::<WindowRuntime>(primary).unwrap().focused);
    assert_eq!(title_of(second.to_bits()), "Second");
    assert_eq!(size_of(second.to_bits()), (300.0, 200.0));
    assert_eq!(resolve(second.to_bits()), Some(second.to_bits()));
    assert_eq!(
        resolve(0),
        Some(primary.to_bits()),
        "0 is the current window"
    );

    // The current window is what a handler's getters answer for.
    set_current(Some(second));
    assert_eq!(current(), second.to_bits());
    assert_eq!(title(), "Second");
    set_current(None);
    assert_eq!(title(), "Start");

    // A window that closed is no handle any more.
    app.world.despawn(second);
    app.tick();
    assert_eq!(resolve(second.to_bits()), None);
    set_current(Some(second));
    assert_eq!(
        current(),
        primary.to_bits(),
        "a closed window is not current"
    );
    set_current(None);
}

/// Windows land on the monitor they ask for and take its scale; with
/// positioning off, as on Wayland, the asked-for place is ignored.
#[test]
fn headless_windows_take_the_scale_of_their_monitor() {
    let _guard = serial();
    let mut app = headless_app();
    app.world.insert_resource(Monitors(vec![
        MonitorInfo {
            name: "left".into(),
            position: IVec2::ZERO,
            size: UVec2::new(1920, 1080),
            work_area: (IVec2::ZERO, UVec2::new(1920, 1080)),
            scale: 1.0,
        },
        MonitorInfo {
            name: "right".into(),
            position: IVec2::new(1920, 0),
            size: UVec2::new(2560, 1440),
            work_area: (IVec2::new(1920, 0), UVec2::new(2560, 1440)),
            scale: 2.0,
        },
    ]));
    let right = app
        .world
        .spawn(Window {
            position: Some(IVec2::new(2000, 100)),
            ..Window::new("Right", Vec2::new(200.0, 100.0))
        })
        .id();
    app.tick();
    let viewport = app.world.get::<WindowViewport>(right).unwrap();
    assert_eq!(viewport.scale_factor, 2.0);
    assert_eq!(
        dpr_of(right.to_bits()),
        2.0,
        "published by the end of the tick"
    );
    assert_eq!(
        app.world
            .get::<WindowRuntime>(right)
            .unwrap()
            .monitor
            .as_deref(),
        Some("right")
    );

    // Moving it back to the left monitor moves its scale with it.
    app.world.get_mut::<Window>(right).unwrap().position = Some(IVec2::new(100, 100));
    app.tick();
    assert_eq!(
        app.world.get::<WindowViewport>(right).unwrap().scale_factor,
        1.0
    );

    // A desktop that places windows itself ignores where one asks to go.
    app.world.insert_resource(HeadlessDesktop {
        positioning: false,
        toplevel_drag: false,
    });
    let placed = app
        .world
        .spawn(Window {
            position: Some(IVec2::new(3000, 500)),
            ..Window::new("Placed", Vec2::new(100.0, 100.0))
        })
        .id();
    app.tick();
    let runtime = app.world.get::<WindowRuntime>(placed).unwrap();
    assert_eq!(
        runtime.position, None,
        "a desktop that places windows does not say where, as Wayland"
    );
    assert_eq!(runtime.monitor.as_deref(), Some("left"));
    // It placed the window, on the left monitor at scale 1, and keeps it
    // there tick after tick.
    assert_eq!(
        app.world
            .get::<WindowViewport>(placed)
            .unwrap()
            .scale_factor,
        1.0
    );
    app.tick();
    assert_eq!(
        app.world.get::<WindowRuntime>(placed).unwrap().position,
        None
    );

    // Maximized and fullscreen are what the window reports at once.
    {
        let mut w = app.world.get_mut::<Window>(placed).unwrap();
        w.maximized = true;
        w.fullscreen = true;
    }
    app.tick();
    let runtime = app.world.get::<WindowRuntime>(placed).unwrap();
    assert!(runtime.maximized && runtime.fullscreen);
    app.world.get_mut::<Window>(placed).unwrap().fullscreen = false;
    app.tick();
    let runtime = app.world.get::<WindowRuntime>(placed).unwrap();
    assert!(runtime.maximized && !runtime.fullscreen);
    app.world.despawn(placed);
    app.world.despawn(right);
    app.tick();
}

/// Closing a secondary window announces it first, so its owner can take its
/// content back; content still in it when it despawns goes with it. The app
/// runs on.
#[test]
fn closing_a_secondary_window_closes_just_it() {
    let _guard = serial();
    let mut app = headless_app();
    let mut requests = CloseRequests::default();
    let root = app.world.spawn_empty().id();
    let popout = app.world.spawn(Window::default()).id();
    let kept = app.world.spawn((ChildOf(root), WindowOf(popout))).id();
    let dropped_window = app.world.spawn(Window::default()).id();
    let dropped = app
        .world
        .spawn((ChildOf(root), WindowOf(dropped_window)))
        .id();
    // Whatever opened the pop-out takes its panel back on `WindowClosing`.
    app.add_systems(
        TickStage::Systems,
        move |mut closing: MessageReader<WindowClosing>, mut commands: Commands| {
            for event in closing.read() {
                if event.window == popout {
                    commands.entity(kept).remove::<WindowOf>();
                }
            }
        },
    );
    app.tick();
    assert!(!app.world.resource::<WindowPartitions>().is_single());

    app.world.write_message(WindowRequest::Close(popout));
    app.world
        .write_message(WindowRequest::Close(dropped_window));
    app.tick();
    let ask = requests.take(&app.world).expect("closes asked");
    assert_eq!(
        run_close_ask(&mut app, &mut requests, ask),
        CloseOutcome::Closed
    );
    assert!(app.world.get_entity(popout).is_err(), "the pop-out closed");
    assert!(
        app.world.get_entity(kept).is_ok(),
        "its panel moved out first"
    );
    assert!(app.world.get::<WindowOf>(kept).is_none());
    assert!(
        app.world.get_entity(dropped).is_err(),
        "unclaimed content went with it"
    );
    assert!(primary_window(&mut app.world).is_some(), "the app runs on");
    app.tick();
    assert!(app.world.resource::<WindowPartitions>().is_single());
}

/// A quit asks every window, the primary one first; a veto from any one
/// cancels the whole quit.
#[test]
fn a_quit_asks_every_window_and_one_veto_cancels_it() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let second = app.world.spawn(Window::default()).id();
    install_veto(&mut app, vec![second]);
    let mut requests = CloseRequests::default();
    assert_eq!(
        run_close_ask(&mut app, &mut requests, CloseAsk::Quit),
        CloseOutcome::Vetoed
    );
    assert_eq!(
        app.world.resource::<Seen>().0,
        [
            (primary, CloseReason::AppQuit),
            (second, CloseReason::AppQuit)
        ]
    );
    app.world.resource_mut::<VetoOnly>().0 = vec![Entity::PLACEHOLDER];
    assert_eq!(
        run_close_ask(&mut app, &mut requests, CloseAsk::Quit),
        CloseOutcome::Quit
    );
}

/// With `[app] quit = "explicit"`, closing the primary window hides every
/// window and the app runs on; a quit still ends it, and so does a signal.
#[test]
fn the_explicit_quit_policy_hides_instead_of_quitting() {
    let _guard = serial();
    let mut app = headless_app();
    app.world.insert_resource(QuitPolicy::Explicit);
    let primary = primary_window(&mut app.world).unwrap();
    let second = app.world.spawn(Window::default()).id();
    let mut requests = CloseRequests::default();
    assert_eq!(
        run_close_request(
            &mut app,
            &mut requests,
            Some(primary),
            CloseReason::Platform
        ),
        CloseOutcome::Hidden
    );
    assert!(!app.world.get::<Window>(primary).unwrap().visible);
    assert!(!app.world.get::<Window>(second).unwrap().visible);
    assert_eq!(
        run_close_request(
            &mut app,
            &mut requests,
            Some(primary),
            CloseReason::Shutdown
        ),
        CloseOutcome::Quit,
        "a signal is not the user closing the window"
    );
    assert_eq!(
        run_close_ask(&mut app, &mut requests, CloseAsk::Quit),
        CloseOutcome::Quit
    );
}

/// Focus leaving a window remembers the element it was on; coming back
/// restores it. With one window nothing takes focus from the app's own
/// element, so this is bookkeeping for apps that show content in several.
#[test]
fn a_window_gets_its_focus_back() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let popout = app.world.spawn(Window::default()).id();
    app.world.spawn(WindowOf(popout));
    let field = app.world.spawn(Focused).id();
    app.world.resource_mut::<FocusTracker>().0 = Some(field);
    app.tick();
    app.world.write_message(WindowFocused {
        window: primary,
        focused: false,
    });
    app.tick();
    assert_eq!(
        app.world.get::<LastFocus>(primary),
        Some(&LastFocus(Some(field)))
    );
    // Something else took focus meanwhile.
    let other = app.world.spawn_empty().id();
    app.world.entity_mut(field).remove::<Focused>();
    app.world.resource_mut::<FocusTracker>().0 = Some(other);
    app.world.write_message(WindowFocused {
        window: primary,
        focused: true,
    });
    app.tick();
    assert_eq!(app.world.resource::<FocusTracker>().0, Some(field));
    assert!(app.world.get::<Focused>(field).is_some());
}

/// Focusing an element in a window that does not have keyboard focus asks
/// for that window to be raised.
#[test]
fn focusing_an_element_in_another_window_raises_it() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let popout = app.world.spawn(Window::default()).id();
    let panel = app.world.spawn(WindowOf(popout)).id();
    let field = app.world.spawn(ChildOf(panel)).id();
    app.tick();
    app.tick();
    // The primary window has focus back.
    app.world.get_mut::<WindowRuntime>(popout).unwrap().focused = false;
    app.world.get_mut::<WindowRuntime>(primary).unwrap().focused = true;
    app.world.resource_mut::<FocusTracker>().0 = Some(field);
    app.tick();
    let raised: Vec<WindowRequest> = app
        .world
        .resource::<Messages<WindowRequest>>()
        .iter_current_update_messages()
        .copied()
        .collect();
    assert!(raised.contains(&WindowRequest::Focus(popout)), "{raised:?}");
}

/// A blocked window says which window blocks it.
#[test]
fn a_blocked_window_names_its_blocker() {
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    let dialog = app.world.spawn(Window::default()).id();
    app.world
        .entity_mut(primary)
        .insert(InputBlocked { by: dialog });
    assert_eq!(
        app.world.get::<InputBlocked>(primary),
        Some(&InputBlocked { by: dialog })
    );
}

/// The surface drag events the app received since `cursor` last looked.
fn drag_events(
    app: &App,
    cursor: &mut MessageCursor<lumen_core::surface_drag::SurfaceDragEvent>,
) -> Vec<lumen_core::surface_drag::SurfaceDragEvent> {
    let messages = app
        .world
        .resource::<Messages<lumen_core::surface_drag::SurfaceDragEvent>>();
    cursor.read(messages).copied().collect()
}

/// Move the pointer to `at` in `window`'s logical pixels, as a backend
/// reports a move, then tick.
fn pointer_to(app: &mut App, window: Entity, at: Vec2) {
    let mut state = app.world.resource_mut::<lumen_core::input::PointerState>();
    state.window = Some(window);
    state.position = Some(at);
    app.world.write_message(lumen_core::input::PointerMoved {
        position: at,
        ..Default::default()
    });
    app.tick();
}

/// A drag across windows on the virtual desktop: the stand-in reports the
/// window under the pointer in its own coordinates, moves a following
/// window with the pointer, and reports the drop; on a desktop that does
/// not position windows no window follows unless toplevel drag is on.
#[test]
fn the_headless_desktop_carries_a_drag_across_windows() {
    use lumen_core::surface_drag::{SurfaceDragEvent, SurfaceDragRequest, SurfaceDragSupport};
    let _guard = serial();
    let mut app = headless_app();
    let primary = primary_window(&mut app.world).unwrap();
    primary_viewport_mut(&mut app.world).size = Vec2::new(400.0, 300.0);
    app.tick();
    let other = app
        .world
        .spawn(Window {
            position: Some(IVec2::new(600, 0)),
            ..Window::new("Other", Vec2::new(200.0, 200.0))
        })
        .id();
    let follower = app
        .world
        .spawn(Window {
            position: Some(IVec2::new(1200, 700)),
            ..Window::new("Follower", Vec2::new(100.0, 100.0))
        })
        .id();
    app.tick();
    assert_eq!(
        app.world.get::<WindowRuntime>(primary).unwrap().position,
        Some(IVec2::ZERO)
    );
    assert!(app.world.resource::<SurfaceDragSupport>().follow);
    let mut cursor = MessageCursor::default();
    // No drag yet: moves report nothing.
    pointer_to(&mut app, primary, Vec2::new(10.0, 10.0));
    assert!(drag_events(&app, &mut cursor).is_empty());

    app.world.write_message(SurfaceDragRequest::Begin {
        source: primary,
        follow: None,
        grab_offset: Vec2::new(5.0, 5.0),
    });
    app.tick();
    // Over its own window, then past its edge over the other window, then
    // over nothing: the source window keeps the pointer's moves.
    let mut seen = Vec::new();
    for at in [(50.0, 60.0), (650.0, 20.0), (500.0, 900.0)] {
        pointer_to(&mut app, primary, Vec2::from(at));
        seen.extend(drag_events(&app, &mut cursor));
    }
    assert_eq!(
        seen,
        vec![
            SurfaceDragEvent::Over {
                window: Some(primary),
                position: Vec2::new(50.0, 60.0)
            },
            SurfaceDragEvent::Over {
                window: Some(other),
                position: Vec2::new(50.0, 20.0)
            },
            SurfaceDragEvent::Over {
                window: None,
                position: Vec2::new(500.0, 900.0)
            },
        ]
    );
    // A window that follows trails the pointer by the grab offset, and is
    // never the window the pointer is over.
    app.world
        .write_message(SurfaceDragRequest::Follow(Some(follower)));
    app.tick();
    pointer_to(&mut app, primary, Vec2::new(700.0, 50.0));
    assert_eq!(
        app.world.get::<WindowRuntime>(follower).unwrap().position,
        Some(IVec2::new(695, 45))
    );
    assert_eq!(
        app.world.get::<Window>(follower).unwrap().position,
        Some(IVec2::new(695, 45)),
        "what the app asks follows, so a later change does not move it back"
    );
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Over {
            window: Some(other),
            position: Vec2::new(100.0, 50.0)
        }]
    );
    // The release drops there and ends the drag.
    app.world.write_message(lumen_core::input::PointerReleased {
        position: Vec2::new(700.0, 50.0),
        ..Default::default()
    });
    app.tick();
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Dropped {
            window: Some(other),
            position: Vec2::new(100.0, 50.0)
        }]
    );
    pointer_to(&mut app, primary, Vec2::new(20.0, 20.0));
    assert!(
        drag_events(&app, &mut cursor).is_empty(),
        "the drag is over"
    );

    // A pointer the platform takes away cancels; an end request stops it.
    app.world.write_message(SurfaceDragRequest::Begin {
        source: primary,
        follow: None,
        grab_offset: Vec2::ZERO,
    });
    app.tick();
    app.world
        .write_message(lumen_core::input::PointerCancelled::default());
    app.tick();
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Cancelled]
    );
    app.world.write_message(SurfaceDragRequest::Begin {
        source: primary,
        follow: None,
        grab_offset: Vec2::ZERO,
    });
    app.tick();
    app.world.write_message(SurfaceDragRequest::End);
    app.tick();
    pointer_to(&mut app, primary, Vec2::new(30.0, 30.0));
    assert!(drag_events(&app, &mut cursor).is_empty());

    // As Wayland without toplevel drag: the stand-in places every window
    // itself and reports no position, but still knows which window the
    // pointer is over; none follows, and the stand-in says so.
    app.world.insert_resource(HeadlessDesktop {
        positioning: false,
        toplevel_drag: false,
    });
    app.world.despawn(other);
    app.world.despawn(follower);
    app.tick();
    assert!(!app.world.resource::<SurfaceDragSupport>().follow);
    let spawned = app
        .world
        .spawn(Window::new("Late", Vec2::new(100.0, 100.0)))
        .id();
    app.tick();
    app.world.write_message(SurfaceDragRequest::Begin {
        source: primary,
        follow: Some(spawned),
        grab_offset: Vec2::new(10.0, 10.0),
    });
    app.tick();
    pointer_to(&mut app, primary, Vec2::new(100.0, 100.0));
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Over {
            window: Some(spawned),
            position: Vec2::new(68.0, 68.0)
        }],
        "the newest window lies on top, where the stand-in cascaded it"
    );
    assert_eq!(
        app.world.get::<WindowRuntime>(spawned).unwrap().position,
        None
    );
    app.world.write_message(SurfaceDragRequest::End);
    app.tick();
    // With toplevel drag on, a window follows again, though the desktop
    // still reports no position for it.
    app.world.insert_resource(HeadlessDesktop {
        positioning: false,
        toplevel_drag: true,
    });
    app.tick();
    assert!(app.world.resource::<SurfaceDragSupport>().follow);
    app.world.write_message(SurfaceDragRequest::Begin {
        source: primary,
        follow: Some(spawned),
        grab_offset: Vec2::new(10.0, 10.0),
    });
    app.tick();
    pointer_to(&mut app, primary, Vec2::new(300.0, 250.0));
    assert_eq!(
        app.world.get::<WindowRuntime>(spawned).unwrap().position,
        None
    );
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Over {
            window: Some(primary),
            position: Vec2::new(300.0, 250.0)
        }],
        "the window following is never the one under the pointer"
    );
    // It went where the pointer took it: once it stops following, the
    // pointer just inside its corner is over it.
    app.world.write_message(SurfaceDragRequest::Follow(None));
    app.tick();
    pointer_to(&mut app, primary, Vec2::new(295.0, 245.0));
    assert_eq!(
        drag_events(&app, &mut cursor),
        vec![SurfaceDragEvent::Over {
            window: Some(spawned),
            position: Vec2::new(5.0, 5.0)
        }]
    );
    app.world.write_message(SurfaceDragRequest::End);
    app.tick();
}
