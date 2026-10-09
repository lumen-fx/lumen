//! Every wheel step that moves a scroller repaints on the tick that moved it.
//!
//! Issue 416: once the overlay scrollbar was fully shown, a wheel step
//! changed `ScrollOffset` but nothing raised `FrameDirty` (the scrollbar's
//! fade state no longer changed), so the window skipped the frame and the
//! content caught up only when the bar began to fade.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_core::render_world::{FrameDirty, install_extract_pipeline};
use lumen_input::InputPlugin;
use lumen_primitives::scroll::ScrollPlugin;

fn test_app() -> App {
    let mut app = App::new();
    app.world
        .init_resource::<lumen_core::components::A11yScrollIntoViewRequests>();
    install_extract_pipeline(&mut app);
    app.add_plugin(InputPlugin::default());
    app.add_plugin(ScrollPlugin);
    app
}

/// A 200x100 vertical scroller over ten 100px tiles, with `inertia: 0.0` so
/// each wheel step lands at once.
fn spawn_scroller(world: &mut World) -> Entity {
    let container = world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(200.0, 100.0)),
            Style::default(),
            Scroll::vertical().with_inertia(0.0),
            ScrollOffset::default(),
        ))
        .id();
    for i in 0..10 {
        world.spawn((
            Transform::new(
                glam::Vec2::new(0.0, i as f32 * 100.0),
                glam::Vec2::new(200.0, 100.0),
            ),
            Visuals::default(),
            bevy_ecs::hierarchy::ChildOf(container),
        ));
    }
    container
}

/// One wheel step, one tick. Clears `FrameDirty` first, as a window does
/// after presenting, and reports whether the tick raised it again.
fn wheel_step(app: &mut App) -> bool {
    app.world.resource_mut::<FrameDirty>().dirty = false;
    app.world
        .resource_mut::<Messages<MouseWheel>>()
        .write(MouseWheel {
            delta: glam::Vec2::new(0.0, 32.0),
            position: glam::Vec2::new(100.0, 50.0),
            local: None,
        });
    app.tick();
    app.world.resource::<FrameDirty>().dirty
}

#[test]
fn each_wheel_step_raises_frame_dirty() {
    let mut app = test_app();
    let container = spawn_scroller(&mut app.world);
    app.world.resource_mut::<PointerState>().position = Some(glam::Vec2::new(100.0, 50.0));
    app.tick();
    app.tick();

    for step in 1..=4 {
        assert!(
            wheel_step(&mut app),
            "wheel step {step} left the frame clean"
        );
        let offset = app.world.get::<ScrollOffset>(container).unwrap().0.y;
        assert_eq!(
            offset,
            step as f32 * 32.0,
            "wheel step {step} did not scroll"
        );
    }
}

/// An idle scroller does not keep the frame dirty.
#[test]
fn an_idle_scroller_leaves_the_frame_clean() {
    let mut app = test_app();
    spawn_scroller(&mut app.world);
    app.tick();
    app.world.resource_mut::<FrameDirty>().dirty = false;
    app.tick();
    assert!(!app.world.resource::<FrameDirty>().dirty);
}
