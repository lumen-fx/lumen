//! Input across windows: hit testing per window, the hover chain and focus
//! across the one tree, and Tab inside the focused window.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::prelude::*;
use lumen_core::window::{Window, WindowOf, WindowRuntime, primary_entity};
use lumen_input::InputPlugin;

fn boxed(world: &mut World, origin: Vec2, size: Vec2) -> Entity {
    world
        .spawn((
            Transform {
                absolute: origin,
                size,
                baseline_y: None,
            },
            Visuals {
                fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
                ..Default::default()
            },
        ))
        .id()
}

/// A dock in the primary window holding a button, and a panel under the
/// dock that shows in a second window, holding a button at the same
/// window-local place.
struct Scene {
    popout: Entity,
    dock: Entity,
    in_primary: Entity,
    panel: Entity,
    in_popout: Entity,
}

fn scene(app: &mut App) -> Scene {
    let popout = app
        .world
        .spawn(Window::new("Popout", Vec2::new(200.0, 200.0)))
        .id();
    let dock = boxed(&mut app.world, Vec2::ZERO, Vec2::splat(300.0));
    let in_primary = boxed(&mut app.world, Vec2::splat(10.0), Vec2::splat(40.0));
    app.world
        .entity_mut(in_primary)
        .insert((ChildOf(dock), TabIndex(0)));
    let panel = boxed(&mut app.world, Vec2::ZERO, Vec2::splat(200.0));
    app.world
        .entity_mut(panel)
        .insert((ChildOf(dock), WindowOf(popout)));
    let in_popout = boxed(&mut app.world, Vec2::splat(10.0), Vec2::splat(40.0));
    app.world
        .entity_mut(in_popout)
        .insert((ChildOf(panel), TabIndex(0)));
    app.tick();
    Scene {
        popout,
        dock,
        in_primary,
        panel,
        in_popout,
    }
}

fn move_to(world: &mut World, window: Option<Entity>, p: Vec2) {
    let mut state = world.resource_mut::<PointerState>();
    state.position = Some(p);
    state.window = window;
    world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved {
            position: p,
            local: None,
            pointer: Default::default(),
        });
}

fn button(world: &mut World, down: bool, p: Vec2) {
    world.resource_mut::<PointerState>().primary_down = down;
    if down {
        world
            .resource_mut::<Messages<PointerPressed>>()
            .write(PointerPressed {
                position: p,
                button: PointerButton::Primary,
                local: None,
                pointer: Default::default(),
            });
    } else {
        world
            .resource_mut::<Messages<PointerReleased>>()
            .write(PointerReleased {
                position: p,
                button: PointerButton::Primary,
                local: None,
                pointer: Default::default(),
            });
    }
}

fn tab(world: &mut World) {
    world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Named(NamedKey::Tab),
            code: lumen_core::input::KeyCode::Unidentified,
            modifiers: Modifiers::default(),
            repeat: false,
            time_stamp: lumen_core::time::Instant::now(),
        });
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugin(lumen_core::window_backend::WindowCorePlugin);
    app.add_plugin(InputPlugin::default());
    app
}

/// The same point hits a different element depending on the window the
/// pointer is in.
#[test]
fn a_point_hits_the_window_the_pointer_is_in() {
    let mut app = app();
    let s = scene(&mut app);

    move_to(&mut app.world, None, Vec2::splat(20.0));
    app.tick();
    assert!(app.world.get::<Hovered>(s.in_primary).is_some());
    assert!(app.world.get::<Hovered>(s.in_popout).is_none());

    move_to(&mut app.world, Some(s.popout), Vec2::splat(20.0));
    app.tick();
    assert!(app.world.get::<Hovered>(s.in_popout).is_some());
    assert!(app.world.get::<Hovered>(s.in_primary).is_none());
}

/// Hovering inside the pop-out puts the hover chain on its ancestors in the
/// primary window too: one document, one `:hover` chain.
#[test]
fn the_hover_chain_crosses_into_the_dock() {
    let mut app = app();
    let s = scene(&mut app);
    move_to(&mut app.world, Some(s.popout), Vec2::splat(20.0));
    app.tick();
    assert!(app.world.get::<HoverWithin>(s.panel).is_some());
    assert!(
        app.world.get::<HoverWithin>(s.dock).is_some(),
        "the dock is an ancestor in the other window"
    );
}

/// A click in the pop-out lands on the element under the pointer there.
#[test]
fn a_click_in_the_popout_targets_its_element() {
    let mut app = app();
    let s = scene(&mut app);
    let at = Vec2::splat(20.0);
    move_to(&mut app.world, Some(s.popout), at);
    app.tick();
    button(&mut app.world, true, at);
    app.tick();
    assert!(app.world.get::<Pressed>(s.in_popout).is_some());
    button(&mut app.world, false, at);
    app.tick();
    let clicks: Vec<Entity> = app
        .world
        .resource::<Messages<ClickEvent>>()
        .iter_current_update_messages()
        .map(|c| c.entity)
        .collect();
    assert_eq!(clicks, [s.in_popout]);
}

/// A pointer over a window that holds nothing there hits nothing, rather
/// than the element at that point in another window.
#[test]
fn a_point_over_empty_space_in_the_popout_hits_nothing() {
    let mut app = app();
    let s = scene(&mut app);
    move_to(&mut app.world, Some(s.popout), Vec2::splat(250.0));
    app.tick();
    assert!(app.world.get::<Hovered>(s.dock).is_none());
    assert!(app.world.get::<Hovered>(s.in_primary).is_none());
}

/// Tab cycles inside the window with keyboard focus and never walks into
/// another window.
#[test]
fn tab_stays_in_the_focused_window() {
    let mut app = app();
    let s = scene(&mut app);
    let primary = primary_entity(&app.world).unwrap();
    app.world.get_mut::<WindowRuntime>(primary).unwrap().focused = true;
    tab(&mut app.world);
    app.tick();
    assert!(app.world.get::<Focused>(s.in_primary).is_some());
    tab(&mut app.world);
    app.tick();
    assert!(
        app.world.get::<Focused>(s.in_primary).is_some(),
        "the only stop in the primary window keeps focus"
    );
    assert!(app.world.get::<Focused>(s.in_popout).is_none());

    // The pop-out takes keyboard focus with nothing focused in it.
    app.world.get_mut::<WindowRuntime>(primary).unwrap().focused = false;
    app.world
        .get_mut::<WindowRuntime>(s.popout)
        .unwrap()
        .focused = true;
    app.world.entity_mut(s.in_primary).remove::<Focused>();
    app.world.resource_mut::<FocusTracker>().0 = None;
    tab(&mut app.world);
    app.tick();
    assert!(app.world.get::<Focused>(s.in_popout).is_some());
    assert!(app.world.get::<Focused>(s.in_primary).is_none());
}

/// A panel taken back from its window before the window closes is part of
/// the primary window again, hit-testable there.
#[test]
fn a_docked_panel_hit_tests_in_the_primary_window_again() {
    let mut app = app();
    let s = scene(&mut app);
    app.world.entity_mut(s.panel).remove::<WindowOf>();
    app.world.despawn(s.popout);
    app.tick();
    move_to(&mut app.world, None, Vec2::splat(20.0));
    app.tick();
    let hovered: Vec<Entity> = app
        .world
        .query_filtered::<Entity, With<Hovered>>()
        .iter(&app.world)
        .collect();
    assert_eq!(hovered, [s.in_popout], "the deeper of the two boxes wins");
}
