//! Headless regression tests for slider / toggle pointer correctness
//! (Qt-parity Wave 1). Drives the REAL pipeline - `lumen_input::hit_test`
//! -> `dispatch_clicks` -> drag FSM -> control mutators - by writing pointer
//! / key / wheel messages into an [`App`] and ticking, exactly like the
//! window backend would. No GPU, no display.
//!
//! The headline defect: the `<slider>` thumb / `<toggle>` knob children
//! spawn with [`Visuals`], so deepest-child-wins hit-testing routed
//! press / drag / click events at the *child* entity while the control
//! mutators only queried the parent - grabbing the thumb (or clicking
//! the knob) was a permanent dead zone ("click -> release -> click to
//! drag").

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_input::InputPlugin;
use lumen_primitives::controls::THUMB_SIZE;
use lumen_primitives::drag::DragPlugin;
use lumen_primitives::scroll::ScrollPlugin;
use lumen_primitives::{ControlsPlugin, SliderThumb, ToggleKnob};

fn test_app() -> App {
    let mut app = App::new();
    // Normally initialized by the a11y plugin; `ScrollPlugin`'s
    // `apply_a11y_scroll_into_view` consumer requires it.
    app.world
        .init_resource::<lumen_core::components::A11yScrollIntoViewRequests>();
    app.add_plugin(InputPlugin);
    app.add_plugin(ScrollPlugin);
    app.add_plugin(DragPlugin::default());
    app.add_plugin(ControlsPlugin);
    app
}

/// One input-free follow-up tick after the tick that carries the input,
/// mirroring the window backend's self-scheduled follow-up frames. The
/// control mutators are ordered after their producers
/// (`dispatch_clicks` / `update_drag_on_move`), so state lands on the
/// input's own tick; the extra tick only flushes deferred `Commands`
/// (component inserts/removes) so assertions observe them.
fn settle(app: &mut App) {
    app.tick();
    app.tick();
}

/// A 200x24 slider track at the origin with min 0 / max 100, plus the
/// 16x16 thumb child positioned for `value` - the same shape
/// `lumenc::spawn` produces for `<slider>`. Returns `(slider, thumb)`.
fn spawn_slider(world: &mut World, value: f32) -> (Entity, Entity) {
    let track_w = 200.0;
    let slider = world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(track_w, 24.0)),
            Visuals::default(),
            SliderValue {
                value,
                min: 0.0,
                max: 100.0,
                step: None,
            },
            TabIndex(0),
        ))
        .id();
    let left = (value / 100.0) * (track_w - THUMB_SIZE);
    let thumb = world
        .spawn((
            Transform::new(glam::Vec2::new(left, 4.0), glam::Vec2::splat(THUMB_SIZE)),
            Visuals::default(),
            SliderThumb,
            ChildOf(slider),
        ))
        .id();
    (slider, thumb)
}

fn thumb_center(world: &World, thumb: Entity) -> glam::Vec2 {
    let t = world.get::<Transform>(thumb).unwrap();
    t.absolute + t.size / 2.0
}

fn move_pointer(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().position = Some(p);
    world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
}

fn press(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().primary_down = true;
    world
        .resource_mut::<Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
        });
}

fn release(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().primary_down = false;
    world
        .resource_mut::<Messages<PointerReleased>>()
        .write(PointerReleased {
            position: p,
            button: PointerButton::Primary,
        });
}

fn press_key(world: &mut World, key: Key) {
    world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key,
            modifiers: Modifiers::default(),
            repeat: false,
        });
}

fn slider_value(world: &World, e: Entity) -> f32 {
    world.get::<SliderValue>(e).unwrap().value
}

/// The author-reproduced bug: the FIRST press dead-center on the thumb
/// must start a drag - no click-release-click warm-up. The drag events
/// target the thumb child; `set_slider_on_drag` must resolve them to the
/// parent slider.
#[test]
fn first_press_dead_center_on_thumb_starts_drag() {
    let mut app = test_app();
    let (slider, thumb) = spawn_slider(&mut app.world, 30.0);

    // Hover the thumb (deepest child wins the hit-test - that's the
    // trigger of the bug, so assert the precondition).
    let grab = thumb_center(&app.world, thumb);
    move_pointer(&mut app.world, grab);
    app.tick();
    assert!(
        app.world.get::<Hovered>(thumb).is_some(),
        "precondition: the thumb child shadows the track in the hit-test"
    );

    // Press on the thumb, then pull right past the 4 px drag threshold.
    press(&mut app.world, grab);
    app.tick();
    move_pointer(&mut app.world, glam::Vec2::new(140.0, 12.0));
    app.tick(); // threshold crossed -> DragStart
    move_pointer(&mut app.world, glam::Vec2::new(150.0, 12.0));
    settle(&mut app); // first DragMove reaches the slider

    // Pointer maps through the reduced thumb range (`(x - THUMB_SIZE/2) /
    // (width - THUMB_SIZE)`), so x=150 -> (150-8)/184 ~ 0.772 -> ~77.2.
    assert!(
        (slider_value(&app.world, slider) - 77.17).abs() < 1.0,
        "first grab of the thumb must drag the value (got {}, want ~77.2)",
        slider_value(&app.world, slider)
    );

    release(&mut app.world, glam::Vec2::new(150.0, 12.0));
    settle(&mut app);
    assert!(
        (slider_value(&app.world, slider) - 77.17).abs() < 1.0,
        "release keeps the dragged value"
    );
}

/// A click (press + release, no drag) landing on the thumb must reach the
/// slider, not vanish on the child entity. Clicking *off* the thumb's
/// centre proves routing: a dead-centre click now legitimately leaves the
/// value unchanged (see `press_dead_center_on_thumb_leaves_value_unchanged`),
/// so we click a few px right of centre and assert the value moved.
#[test]
fn click_on_thumb_routes_to_slider() {
    let mut app = test_app();
    let (slider, thumb) = spawn_slider(&mut app.world, 0.0);
    // Thumb at value 0 sits at x 0..16; click x=12 (still on the thumb,
    // right of its centre x=8).
    let at = thumb_center(&app.world, thumb) + glam::Vec2::new(4.0, 0.0);
    move_pointer(&mut app.world, at);
    app.tick();
    press(&mut app.world, at);
    app.tick();
    release(&mut app.world, at);
    settle(&mut app);
    // frac = (12 - 8) / 184 ~ 0.0217 -> value ~2.17: the click reached the
    // slider and mapped through the reduced thumb range.
    assert!(
        (slider_value(&app.world, slider) - 2.17).abs() < 0.5,
        "click on the thumb must jump-to-position on the parent slider \
         (got {})",
        slider_value(&app.world, slider)
    );
}

/// The `<toggle>` knob covers most of the control's face; clicking it
/// dead-center must flip the toggle.
#[test]
fn click_dead_center_on_toggle_knob_flips_toggle() {
    let mut app = test_app();
    let toggle = app
        .world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(48.0, 24.0)),
            Visuals::default(),
            Toggleable { checked: false },
        ))
        .id();
    let knob = app
        .world
        .spawn((
            Transform::new(glam::Vec2::new(4.0, 4.0), glam::Vec2::splat(16.0)),
            Visuals::default(),
            ToggleKnob,
            ChildOf(toggle),
        ))
        .id();

    let at = glam::Vec2::new(12.0, 12.0); // knob center
    move_pointer(&mut app.world, at);
    app.tick();
    assert!(
        app.world.get::<Hovered>(knob).is_some(),
        "precondition: the knob shadows the track in the hit-test"
    );
    press(&mut app.world, at);
    app.tick();
    release(&mut app.world, at);
    settle(&mut app);
    assert!(
        app.world.get::<Toggleable>(toggle).unwrap().checked,
        "click dead-center on the knob must flip the toggle"
    );
}

/// Escape mid-drag cancels: value restored to its pre-drag snapshot, the
/// drag ends, and the eventual release commits nothing.
#[test]
fn escape_cancels_slider_drag_and_restores_value() {
    let mut app = test_app();
    let (slider, thumb) = spawn_slider(&mut app.world, 30.0);

    let grab = thumb_center(&app.world, thumb);
    move_pointer(&mut app.world, grab);
    app.tick();
    press(&mut app.world, grab);
    app.tick();
    move_pointer(&mut app.world, glam::Vec2::new(100.0, 12.0));
    app.tick(); // threshold crossed -> DragStart (snapshots value 30)
    move_pointer(&mut app.world, glam::Vec2::new(140.0, 12.0));
    settle(&mut app); // DragMove -> (140-8)/184 ~ 0.717 -> value ~71.7
    assert!(
        (slider_value(&app.world, slider) - 71.74).abs() < 1.0,
        "drag reached ~71.7 before the cancel (got {})",
        slider_value(&app.world, slider)
    );

    press_key(&mut app.world, Key::Named(NamedKey::Escape));
    settle(&mut app);
    assert_eq!(
        slider_value(&app.world, slider),
        30.0,
        "Escape restores the pre-drag value"
    );

    // Still holding the button: further motion must not resume the drag...
    move_pointer(&mut app.world, glam::Vec2::new(180.0, 12.0));
    settle(&mut app);
    assert_eq!(slider_value(&app.world, slider), 30.0);

    // ...and the release must not emit a click commit.
    release(&mut app.world, glam::Vec2::new(180.0, 12.0));
    settle(&mut app);
    assert_eq!(
        slider_value(&app.world, slider),
        30.0,
        "release after a cancelled drag commits nothing"
    );
}

/// One wheel notch over a hovered slider = one step, and the wheel is
/// consumed - an ancestor scroll container must not move.
#[test]
fn wheel_over_slider_steps_value_and_does_not_scroll_ancestor() {
    let mut app = test_app();
    // 400x400 scroller with 1000-px content: plenty of scroll range.
    let scroller = app
        .world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(400.0, 400.0)),
            Scroll::vertical().with_inertia(0.0),
            ScrollOffset::default(),
        ))
        .id();
    let content = app
        .world
        .spawn((
            Transform::new(glam::Vec2::ZERO, glam::Vec2::new(400.0, 1000.0)),
            ChildOf(scroller),
        ))
        .id();
    let slider = app
        .world
        .spawn((
            Transform::new(glam::Vec2::new(10.0, 10.0), glam::Vec2::new(200.0, 24.0)),
            Visuals::default(),
            SliderValue {
                value: 50.0,
                min: 0.0,
                max: 100.0,
                step: None,
            },
            ChildOf(content),
        ))
        .id();

    move_pointer(&mut app.world, glam::Vec2::new(110.0, 22.0));
    app.tick();
    assert!(app.world.get::<Hovered>(slider).is_some());

    // One wheel notch up (the backend normalizes one line-detent to
    // WHEEL_NOTCH_PX logical pixels).
    app.world
        .resource_mut::<Messages<MouseWheel>>()
        .write(MouseWheel {
            delta: glam::Vec2::new(0.0, lumen_primitives::WHEEL_NOTCH_PX),
            position: glam::Vec2::new(110.0, 22.0),
        });
    app.tick();

    assert!(
        (slider_value(&app.world, slider) - 51.0).abs() < f32::EPSILON,
        "one notch = one step: 50 -> 51 (got {})",
        slider_value(&app.world, slider)
    );
    assert_eq!(
        app.world.get::<ScrollOffset>(scroller).unwrap().0,
        glam::Vec2::ZERO,
        "the slider consumes the wheel; the ancestor scroller must not move"
    );

    // Wheel-down with an authored step: one notch = one authored step.
    app.world.get_mut::<SliderValue>(slider).unwrap().step = Some(5.0);
    app.world
        .resource_mut::<Messages<MouseWheel>>()
        .write(MouseWheel {
            delta: glam::Vec2::new(0.0, -lumen_primitives::WHEEL_NOTCH_PX),
            position: glam::Vec2::new(110.0, 22.0),
        });
    app.tick();
    assert!(
        (slider_value(&app.world, slider) - 46.0).abs() < f32::EPSILON,
        "authored step=\"5\": 51 -> 46 (got {})",
        slider_value(&app.world, slider)
    );
}

/// Pressing (and releasing, no drag) exactly on the thumb must leave the
/// value unchanged. The pointer maps through the SAME reduced range the
/// thumb is drawn over, so a dead-centre grab is value-neutral - before
/// the fix the click mapped over the full track width while the thumb was
/// drawn over `width - THUMB_SIZE`, so grabbing the thumb nudged the value
/// by up to `THUMB_SIZE/2` worth of range (the "volume slider jumps when
/// you grab it" bug).
#[test]
fn press_dead_center_on_thumb_leaves_value_unchanged() {
    let mut app = test_app();
    // value 50 -> thumb centre at x = 8 + 0.5*184 = 100.
    let (slider, thumb) = spawn_slider(&mut app.world, 50.0);
    let at = thumb_center(&app.world, thumb);
    assert!(
        (at.x - 100.0).abs() < 0.01,
        "precondition: thumb centre sits at x~100 (got {})",
        at.x
    );

    move_pointer(&mut app.world, at);
    app.tick();
    press(&mut app.world, at);
    app.tick();
    release(&mut app.world, at);
    settle(&mut app);

    assert!(
        (slider_value(&app.world, slider) - 50.0).abs() < 0.01,
        "grabbing the thumb dead-centre must not nudge the value (got {})",
        slider_value(&app.world, slider)
    );
}

/// Space / Enter on a Tab-focused slider must leave its value untouched
/// (it used to synthesize a zero-position click -> value = min).
#[test]
fn space_on_focused_slider_leaves_value_unchanged() {
    let mut app = test_app();
    let (slider, _thumb) = spawn_slider(&mut app.world, 42.0);

    // Tab-focus the slider (it carries TabIndex like the real spawn).
    press_key(&mut app.world, Key::Named(NamedKey::Tab));
    app.tick();
    assert!(app.world.get::<Focused>(slider).is_some());

    press_key(&mut app.world, Key::Named(NamedKey::Space));
    app.tick();
    press_key(&mut app.world, Key::Named(NamedKey::Enter));
    app.tick();
    assert_eq!(
        slider_value(&app.world, slider),
        42.0,
        "Space/Enter must not click-activate a slider"
    );

    // Arrows still work (keyboard path unaffected by the exemption).
    press_key(&mut app.world, Key::Named(NamedKey::ArrowRight));
    app.tick();
    assert_eq!(slider_value(&app.world, slider), 43.0);
}
