//! W6 T3 regression: scrolling under a STATIONARY cursor must re-resolve
//! `Hovered` on the same tick the offset changes (Qt behaviour: content
//! moving under the pointer re-evaluates the hovered widget).
//!
//! The defect: `accumulate_wheel` / `integrate_scroll` / `scroll_on_keys`
//! all mutate `ScrollOffset` in `TickStage::Systems` with no ordering
//! edge against `lumen_input::hit_test` (a `ScrollOffset` reader). The
//! executor serialized the conflicting pair in an arbitrary order, so
//! when the last offset mutation of a scroll landed after that tick's
//! hit-test, the hover marker kept pointing at the pre-scroll entity
//! until an unrelated tick re-ran it. `ScrollPlugin` now orders every
//! offset mutator `.before(hit_test)`.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_input::InputPlugin;
use lumen_primitives::scroll::ScrollPlugin;

fn test_app() -> App {
    let mut app = App::new();
    app.world
        .init_resource::<lumen_core::components::A11yScrollIntoViewRequests>();
    app.add_plugin(InputPlugin);
    app.add_plugin(ScrollPlugin);
    app
}

/// Vertical scroller (200x100 viewport, 2x100-tall tiles -> max offset
/// 100) with `inertia: 0.0` so the wheel applies instantly - the test
/// pins the same-tick contract, not the momentum integrator.
fn spawn_scroller(world: &mut World) -> (Entity, Entity, Entity) {
    let container = world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(200.0, 100.0)),
            Style::default(),
            Scroll::vertical().with_inertia(0.0),
            ScrollOffset::default(),
        ))
        .id();
    let tile1 = world
        .spawn((
            Transform::new(glam::Vec2::new(0.0, 0.0), glam::Vec2::new(200.0, 100.0)),
            Visuals::default(),
            bevy_ecs::hierarchy::ChildOf(container),
        ))
        .id();
    let tile2 = world
        .spawn((
            Transform::new(glam::Vec2::new(0.0, 100.0), glam::Vec2::new(200.0, 100.0)),
            Visuals::default(),
            bevy_ecs::hierarchy::ChildOf(container),
        ))
        .id();
    (container, tile1, tile2)
}

#[test]
fn scroll_under_stationary_cursor_rehovers_same_tick() {
    let mut app = test_app();
    let (container, tile1, tile2) = spawn_scroller(&mut app.world);

    // Stationary cursor over the middle of the viewport.
    app.world.resource_mut::<PointerState>().position = Some(glam::Vec2::new(100.0, 50.0));
    app.tick();
    app.tick(); // flush deferred Hovered commands
    assert!(
        app.world.get::<Hovered>(tile1).is_some(),
        "pre-scroll: cursor sits on tile 1"
    );
    assert!(app.world.get::<Hovered>(tile2).is_none());

    // Wheel-down 120 px with the cursor unmoved. Offset jumps to 120
    // (unclamped until A11ySync), shifting tile 2's visual rect to
    // -20..80 - the cursor at y=50 is now over tile 2.
    app.world
        .resource_mut::<Messages<MouseWheel>>()
        .write(MouseWheel {
            delta: glam::Vec2::new(0.0, -120.0),
            position: glam::Vec2::new(100.0, 50.0),
        });
    app.tick(); // wheel consumed; hit_test runs AFTER the offset mutation
    app.tick(); // flush deferred Hovered commands

    let offset = app.world.get::<ScrollOffset>(container).unwrap().0;
    assert!(
        offset.y >= 100.0 - 0.5,
        "wheel scrolled the container (clamped to content extent), got {offset:?}"
    );
    assert!(
        app.world.get::<Hovered>(tile1).is_none(),
        "stale hover: tile 1 moved out from under the stationary cursor"
    );
    assert!(
        app.world.get::<Hovered>(tile2).is_some(),
        "content moved under the cursor - hover must re-resolve to tile 2"
    );
}
