//! Headless integration test for the input pipeline.
//!
//! Drives an App without a window: writes Pointer/MouseInput messages
//! directly into the main world, ticks, asserts on Hovered/Pressed
//! components and ClickEvent messages. No GPU, no display.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_input::InputPlugin;

fn spawn_button(world: &mut World, origin: glam::Vec2, size: glam::Vec2) -> Entity {
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
            TabIndex(0),
        ))
        .id()
}

fn write_pointer_moved(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().position = Some(p);
    world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
}

fn write_pointer_pressed(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().primary_down = true;
    world
        .resource_mut::<Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
        });
}

fn write_pointer_released(world: &mut World, p: glam::Vec2) {
    world.resource_mut::<PointerState>().primary_down = false;
    world
        .resource_mut::<Messages<PointerReleased>>()
        .write(PointerReleased {
            position: p,
            button: PointerButton::Primary,
        });
}

fn click_count(world: &World) -> usize {
    world
        .resource::<Messages<ClickEvent>>()
        .iter_current_update_messages()
        .count()
}

#[test]
fn pointer_over_button_inserts_hovered() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let button = spawn_button(
        &mut app.world,
        glam::Vec2::new(10.0, 10.0),
        glam::Vec2::splat(40.0),
    );

    write_pointer_moved(&mut app.world, glam::Vec2::new(20.0, 20.0));
    app.tick();
    assert!(
        app.world.get::<Hovered>(button).is_some(),
        "button should be Hovered when cursor is inside its AABB"
    );

    write_pointer_moved(&mut app.world, glam::Vec2::new(100.0, 100.0));
    app.tick();
    assert!(
        app.world.get::<Hovered>(button).is_none(),
        "Hovered should be removed when cursor leaves"
    );
}

#[test]
fn press_release_inside_emits_click() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let button = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(50.0),
    );

    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    write_pointer_pressed(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert!(
        app.world.get::<Pressed>(button).is_some(),
        "Pressed should be inserted on PointerPressed against the Hovered entity"
    );

    write_pointer_released(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert!(
        app.world.get::<Pressed>(button).is_none(),
        "Pressed should be removed on PointerReleased"
    );
    assert_eq!(click_count(&app.world), 1, "exactly one ClickEvent");
}

#[test]
fn press_then_leave_does_not_click() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let _button = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(50.0),
    );

    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    write_pointer_pressed(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();

    // Move pointer off the button before release.
    write_pointer_moved(&mut app.world, glam::Vec2::new(200.0, 200.0));
    app.tick();
    write_pointer_released(&mut app.world, glam::Vec2::new(200.0, 200.0));
    app.tick();

    assert_eq!(
        click_count(&app.world),
        0,
        "ClickEvent should NOT fire when release happens outside the press target"
    );
}

#[test]
fn tab_cycles_focus_across_two_tabindex_entities() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let a = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(40.0),
    );
    let b = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::new(50.0, 0.0),
                size: glam::Vec2::splat(40.0),
                baseline_y: None,
            },
            Visuals {
                fill: Some(Fill::Solid(Color::rgb(0.0, 0.0, 1.0))),
                ..Default::default()
            },
            TabIndex(1),
        ))
        .id();

    // First Tab: focus the lowest TabIndex (a).
    app.world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Named(NamedKey::Tab),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    app.tick();
    assert!(
        app.world.get::<Focused>(a).is_some(),
        "first Tab focuses lowest TabIndex"
    );
    assert!(app.world.get::<Focused>(b).is_none());

    // Second Tab: advance to b.
    app.world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Named(NamedKey::Tab),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    app.tick();
    assert!(app.world.get::<Focused>(b).is_some());
    assert!(app.world.get::<Focused>(a).is_none());

    // Shift+Tab: go back to a.
    app.world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Named(NamedKey::Tab),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            repeat: false,
        });
    app.tick();
    assert!(app.world.get::<Focused>(a).is_some());
}

/// Equal `TabIndex` siblings must cycle in document (markup) order, not in
/// whatever order their `Entity` ids happen to compare - `bevy_ecs` 0.19's
/// `Entity: Ord` is a niche-optimized row-index comparison, not a
/// spawn-order one, so a bare `(TabIndex, Entity)` sort can silently
/// reverse the on-screen tab order. Spawns the entity that should cycle
/// LAST *first* (so it gets the lowest `Entity` id) to prove the sort
/// keys off `DocumentOrder`, not spawn/entity order.
#[test]
fn tab_cycle_with_equal_tabindex_follows_document_order_not_entity_order() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());

    let third = app.world.spawn((TabIndex(0), DocumentOrder(2))).id();
    let first = app.world.spawn((TabIndex(0), DocumentOrder(0))).id();
    let second = app.world.spawn((TabIndex(0), DocumentOrder(1))).id();

    let press_tab = |world: &mut World, shift: bool| {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Tab),
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                repeat: false,
            });
    };

    press_tab(&mut app.world, false);
    app.tick();
    assert!(
        app.world.get::<Focused>(first).is_some(),
        "first Tab focuses DocumentOrder 0 despite it having the highest Entity id"
    );

    press_tab(&mut app.world, false);
    app.tick();
    assert!(
        app.world.get::<Focused>(second).is_some(),
        "second Tab advances to DocumentOrder 1"
    );
    assert!(app.world.get::<Focused>(first).is_none());

    press_tab(&mut app.world, false);
    app.tick();
    assert!(
        app.world.get::<Focused>(third).is_some(),
        "third Tab advances to DocumentOrder 2, spawned first / lowest Entity id"
    );
    assert!(app.world.get::<Focused>(second).is_none());

    // Shift+Tab from DocumentOrder 2 wraps back to DocumentOrder 1.
    press_tab(&mut app.world, true);
    app.tick();
    assert!(app.world.get::<Focused>(second).is_some());
    assert!(app.world.get::<Focused>(third).is_none());
}

// --- Word navigation / word deletion (ctrl+<-/->, ctrl+Backspace/Delete) ------

/// Spawn a `TextInput` + `TextContent` entity and point [`FocusTracker`]
/// at it directly (word-nav / deletion don't require click-to-focus or
/// tab order - `type_into_focused` only checks `FocusTracker`).
fn spawn_focused_input(world: &mut World, text: &str, cursor: usize) -> Entity {
    let e = world
        .spawn((
            TextContent(text.to_string()),
            TextInput {
                placeholder: String::new(),
                cursor,
                selection_anchor: None,
                multiline: false,
            },
        ))
        .id();
    world.resource_mut::<FocusTracker>().0 = Some(e);
    world.entity_mut(e).insert(Focused);
    e
}

fn write_key(world: &mut World, key: Key, ctrl: bool, shift: bool) {
    let modifiers = Modifiers {
        ctrl,
        shift,
        ..Modifiers::default()
    };
    world.resource_mut::<ModifiersState>().0 = modifiers;
    world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key,
            modifiers,
            repeat: false,
        });
}

fn text_of(world: &World, e: Entity) -> String {
    world.get::<TextContent>(e).unwrap().0.clone()
}

fn cursor_of(world: &World, e: Entity) -> usize {
    world.get::<TextInput>(e).unwrap().cursor
}

#[test]
fn ctrl_left_jumps_to_previous_word_start() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let e = spawn_focused_input(&mut app.world, "hello world", 11);

    write_key(&mut app.world, Key::Named(NamedKey::ArrowLeft), true, false);
    app.tick();
    assert_eq!(
        cursor_of(&app.world, e),
        6,
        "ctrl+Left lands at start of \"world\""
    );

    // The space between "hello" and "world" is its own word-boundary
    // segment (unicode-segmentation treats whitespace runs as segments
    // too), so it takes one more ctrl+Left to reach the space's start
    // and a third to reach the very beginning of the text.
    write_key(&mut app.world, Key::Named(NamedKey::ArrowLeft), true, false);
    app.tick();
    assert_eq!(
        cursor_of(&app.world, e),
        5,
        "second ctrl+Left reaches start of the space run"
    );

    write_key(&mut app.world, Key::Named(NamedKey::ArrowLeft), true, false);
    app.tick();
    assert_eq!(
        cursor_of(&app.world, e),
        0,
        "third ctrl+Left reaches start of text"
    );
}

#[test]
fn ctrl_right_jumps_to_next_word_end() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let e = spawn_focused_input(&mut app.world, "hello world", 0);

    write_key(
        &mut app.world,
        Key::Named(NamedKey::ArrowRight),
        true,
        false,
    );
    app.tick();
    assert_eq!(
        cursor_of(&app.world, e),
        5,
        "ctrl+Right lands at end of \"hello\""
    );
}

#[test]
fn ctrl_shift_left_extends_selection_by_word() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let e = spawn_focused_input(&mut app.world, "hello world", 11);

    write_key(&mut app.world, Key::Named(NamedKey::ArrowLeft), true, true);
    app.tick();
    assert_eq!(cursor_of(&app.world, e), 6);
    let sel = app.world.get::<TextInput>(e).unwrap().selection_anchor;
    assert_eq!(
        sel,
        Some(11),
        "shift+ctrl+Left sets the anchor at the start cursor"
    );
}

#[test]
fn ctrl_backspace_deletes_previous_word() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let e = spawn_focused_input(&mut app.world, "hello world", 11);

    write_key(&mut app.world, Key::Named(NamedKey::Backspace), true, false);
    app.tick();
    assert_eq!(text_of(&app.world, e), "hello ");
    assert_eq!(cursor_of(&app.world, e), 6);
}

#[test]
fn ctrl_delete_deletes_next_word() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let e = spawn_focused_input(&mut app.world, "hello world", 0);

    write_key(&mut app.world, Key::Named(NamedKey::Delete), true, false);
    app.tick();
    assert_eq!(text_of(&app.world, e), " world");
    assert_eq!(cursor_of(&app.world, e), 0);
}

#[test]
fn ctrl_left_respects_multibyte_char_boundaries() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    // "h\u{e9}llo w\u{f6}rld" - the accented letters are each 2 bytes.
    let text = "h\u{e9}llo w\u{f6}rld";
    let e = spawn_focused_input(&mut app.world, text, text.len());

    write_key(&mut app.world, Key::Named(NamedKey::ArrowLeft), true, false);
    app.tick();
    let world_start = text.rfind(' ').unwrap() + 1;
    assert_eq!(cursor_of(&app.world, e), world_start);
    assert!(
        text.is_char_boundary(cursor_of(&app.world, e)),
        "cursor must land on a char boundary"
    );
}

#[test]
fn disabled_entity_neither_presses_nor_clicks() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let button = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(50.0),
    );
    app.world
        .entity_mut(button)
        .insert(lumen_core::components::Disabled);

    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    write_pointer_pressed(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert!(
        app.world.get::<Pressed>(button).is_none(),
        "Disabled entity must not gain Pressed"
    );

    write_pointer_released(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert_eq!(
        click_count(&app.world),
        0,
        "Disabled entity must not emit ClickEvent"
    );
}

#[test]
fn tab_cycle_skips_disabled_entities() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let a = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(40.0),
    );
    let b = spawn_button(
        &mut app.world,
        glam::Vec2::new(50.0, 0.0),
        glam::Vec2::splat(40.0),
    );
    let c = spawn_button(
        &mut app.world,
        glam::Vec2::new(100.0, 0.0),
        glam::Vec2::splat(40.0),
    );
    // Distinct tab indexes pin the cycle order a -> b -> c.
    app.world.entity_mut(b).insert(TabIndex(1));
    app.world.entity_mut(c).insert(TabIndex(2));
    app.world
        .entity_mut(b)
        .insert(lumen_core::components::Disabled);

    let press_tab = |app: &mut App| {
        app.world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Tab),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        app.tick();
    };

    press_tab(&mut app);
    assert!(app.world.get::<Focused>(a).is_some(), "first Tab focuses a");

    press_tab(&mut app);
    assert!(
        app.world.get::<Focused>(b).is_none(),
        "disabled b must be skipped"
    );
    assert!(
        app.world.get::<Focused>(c).is_some(),
        "second Tab lands on c, skipping disabled b"
    );
}

// --- Same-tick press+release (MCP simulate / synthetic click) ----------------

/// Counts every [`ClickEvent`] delivered across all ticks, exactly like a
/// real handler-dispatch system (`dispatch_clicks_and_doubles`) would.
#[derive(Resource, Default)]
struct ClickTally(usize);

fn tally_clicks(mut tally: ResMut<ClickTally>, mut clicks: MessageReader<ClickEvent>) {
    for _ in clicks.read() {
        tally.0 += 1;
    }
}

/// Regression: a press and its release delivered on the SAME tick - the
/// shape produced by the MCP `drain_simulate_queue` path, which injects a
/// PointerPressed and PointerReleased from one drain - must produce exactly
/// one ClickEvent and leave NO stuck `Pressed` marker, even across the
/// self-scheduled follow-up frames the window backend runs after a click.
///
/// Before the fix, `dispatch_clicks` inserted `Pressed` via deferred
/// `Commands` in the press loop and then read the (still-empty) `pressed`
/// query in the release loop of the same run: the click was swallowed (0
/// deliveries) and `Pressed` leaked, which downstream compounded into
/// wrong-element / burst deliveries.
#[test]
fn same_tick_press_release_emits_exactly_one_click() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    app.world.init_resource::<ClickTally>();
    app.add_systems(
        lumen_core::tick::TickStage::Systems,
        tally_clicks.after(lumen_input::dispatch_clicks),
    );
    let button = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(50.0),
    );

    // Establish hover, as a prior CursorMoved / simulate PointerMove would.
    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();

    // Press AND release in ONE tick (the simulate drain shape).
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<Messages<PointerPressed>>()
        .write(PointerPressed {
            position: glam::Vec2::new(25.0, 25.0),
            button: PointerButton::Primary,
        });
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world
        .resource_mut::<Messages<PointerReleased>>()
        .write(PointerReleased {
            position: glam::Vec2::new(25.0, 25.0),
            button: PointerButton::Primary,
        });
    app.tick();

    // Self-scheduled follow-up frames (press-tint fade keeps the loop
    // ticking after a click). No new input - must not re-deliver.
    for _ in 0..15 {
        app.tick();
    }

    assert_eq!(
        app.world.resource::<ClickTally>().0,
        1,
        "one same-tick press+release must yield exactly one ClickEvent"
    );
    assert!(
        app.world.get::<Pressed>(button).is_none(),
        "same-tick press+release must not leak a stuck `Pressed` marker"
    );
}

/// Four independent same-tick clicks must deliver exactly four ClickEvents -
/// the headless analogue of the "4 clicks on the counter -> 1,2,3,4" live
/// check. Interleaves self-scheduled follow-up ticks between clicks.
#[test]
fn four_same_tick_clicks_deliver_four_times() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    app.world.init_resource::<ClickTally>();
    app.add_systems(
        lumen_core::tick::TickStage::Systems,
        tally_clicks.after(lumen_input::dispatch_clicks),
    );
    let _button = spawn_button(
        &mut app.world,
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::splat(50.0),
    );
    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();

    for _ in 0..4 {
        app.world.resource_mut::<PointerState>().primary_down = true;
        app.world
            .resource_mut::<Messages<PointerPressed>>()
            .write(PointerPressed {
                position: glam::Vec2::new(25.0, 25.0),
                button: PointerButton::Primary,
            });
        app.world.resource_mut::<PointerState>().primary_down = false;
        app.world
            .resource_mut::<Messages<PointerReleased>>()
            .write(PointerReleased {
                position: glam::Vec2::new(25.0, 25.0),
                button: PointerButton::Primary,
            });
        app.tick();
        // Idle follow-up frames between clicks.
        for _ in 0..3 {
            app.tick();
        }
    }

    assert_eq!(
        app.world.resource::<ClickTally>().0,
        4,
        "four separate same-tick clicks must yield exactly four ClickEvents"
    );
}

/// A `<button>` with no `bg` anywhere (no markup attr, no CSS rule, no
/// skin) spawns without [`Visuals`]. Hit-testing must still route the
/// pointer to it: participation is decided by the layout rect plus
/// interactivity, never by whether the element painted a background.
/// Deleting an app's stylesheet used to make every button inert.
#[test]
fn button_without_visuals_is_a_click_target() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let button = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::splat(50.0),
                baseline_y: None,
            },
            TabIndex(0),
        ))
        .id();
    assert!(
        app.world.get::<Visuals>(button).is_none(),
        "the fixture must stay bg-less for the test to mean anything"
    );

    write_pointer_moved(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert!(
        app.world.get::<Hovered>(button).is_some(),
        "an unpainted button must hover at its own rect"
    );

    write_pointer_pressed(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    write_pointer_released(&mut app.world, glam::Vec2::new(25.0, 25.0));
    app.tick();
    assert_eq!(
        click_count(&app.world),
        1,
        "an unpainted button must deliver a ClickEvent at its own rect"
    );
}

/// A bg-less focusable nested inside a painted container wins the hit
/// over the container: deepest-candidate-wins is unchanged, the
/// focusable simply joined the candidate set.
#[test]
fn nested_bg_less_focusable_wins_over_its_painted_parent() {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    let panel = spawn_button(
        &mut app.world,
        glam::Vec2::ZERO,
        glam::Vec2::new(200.0, 100.0),
    );
    let inner = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::new(20.0, 20.0),
                size: glam::Vec2::splat(40.0),
                baseline_y: None,
            },
            TabIndex(0),
            ChildOf(panel),
        ))
        .id();

    write_pointer_moved(&mut app.world, glam::Vec2::new(30.0, 30.0));
    app.tick();
    assert!(
        app.world.get::<Hovered>(inner).is_some(),
        "the deeper unpainted focusable takes the hit"
    );
    assert!(app.world.get::<Hovered>(panel).is_none());
}
