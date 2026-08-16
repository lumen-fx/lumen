//! Headless integration tests for the W2 text-editing core:
//! `InputPlugin` (producers) + `lumen_text_edit::TextEditPlugin`
//! (mutator/mirror) wired the same way the lumenc runtime wires them.
//!
//! Covers: click-to-caret placement, press-drag selection, double-click
//! word / triple-click line select, Shift+click extension, Tab-focus
//! select-all, undo/redo round-trips (including typing coalescing),
//! multiline ArrowUp/ArrowDown, caret-blink gating and quiescence, and
//! the placeholder-while-focused extract.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::CaretBlink;
use lumen_core::input::{FocusTracker, KeyPressed};
use lumen_core::prelude::*;
use lumen_core::render_world::AnimationsActive;
use lumen_core::text_model::{TextBuffer, TextCursor};
use lumen_input::InputPlugin;
use lumen_text::TextEditPlugin;

/// Default `TextStyle` size is 16 px; the pointer hit-test estimates a
/// per-grapheme advance of `size_px * 0.55`.
const ADVANCE: f32 = 16.0 * 0.55;

fn app() -> App {
    let mut app = App::new();
    app.add_plugin(InputPlugin::default());
    app.add_plugin(TextEditPlugin);
    app
}

fn spawn_input(app: &mut App, text: &str, multiline: bool) -> Entity {
    let e = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(400.0, 60.0),
                baseline_y: None,
            },
            Visuals {
                fill: Some(Fill::Solid(Color::rgb(0.1, 0.1, 0.1))),
                ..Default::default()
            },
            TextContent(text.to_string()),
            TextInput {
                placeholder: String::new(),
                cursor: 0,
                selection_anchor: None,
                multiline,
            },
            TabIndex(0),
        ))
        .id();
    // One tick so `text_attach_buffer`'s deferred insert lands.
    app.tick();
    assert!(
        app.world.get::<TextBuffer>(e).is_some(),
        "TextEditPlugin must attach a TextBuffer to spawned inputs"
    );
    e
}

fn press_at(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
        });
    // Hover state must exist before the press is dispatched, and the
    // press producers run in the same tick as hit_test - one tick
    // handles move+press just like the real backend delivers them.
    app.tick();
}

fn release_at(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world
        .resource_mut::<Messages<PointerReleased>>()
        .write(PointerReleased {
            position: p,
            button: PointerButton::Primary,
        });
    app.tick();
}

fn move_to(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.tick();
}

fn write_key_mods(app: &mut App, key: Key, modifiers: Modifiers) {
    app.world.resource_mut::<ModifiersState>().0 = modifiers;
    app.world
        .resource_mut::<Messages<KeyPressed>>()
        .write(KeyPressed {
            key,
            modifiers,
            repeat: false,
        });
}

fn write_key(app: &mut App, key: Key) {
    write_key_mods(app, key, Modifiers::default());
}

fn write_char(app: &mut App, s: &str) {
    write_key(app, Key::Character(s.into()));
}

fn ctrl(shift: bool) -> Modifiers {
    Modifiers {
        ctrl: true,
        shift,
        ..Modifiers::default()
    }
}

fn input_state(app: &App, e: Entity) -> (String, usize, Option<usize>) {
    let tc = app.world.get::<TextContent>(e).unwrap();
    let ti = app.world.get::<TextInput>(e).unwrap();
    (tc.0.clone(), ti.cursor, ti.selection_anchor)
}

/// x coordinate that the uniform-advance hit test maps to grapheme
/// column `col`.
fn col_x(col: usize) -> f32 {
    col as f32 * ADVANCE
}

// --- click-to-caret / focus -------------------------------------------------

#[test]
fn press_places_caret_and_focuses() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello world", false);
    press_at(&mut app, glam::Vec2::new(col_x(7), 10.0));

    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!(cursor, 7, "caret lands on the pressed grapheme column");
    assert_eq!(sel, None, "plain press collapses the selection");
    let cur = app.world.get::<TextCursor>(e).unwrap();
    assert_eq!(cur.head.byte, 7, "canonical TextCursor mirrors in lockstep");
    assert!(
        app.world.get::<lumen_core::input::Focused>(e).is_some(),
        "press focuses the input (Qt: focus on press, not release)"
    );
    assert_eq!(app.world.resource::<FocusTracker>().0, Some(e));
}

#[test]
fn press_outside_clears_input_focus() {
    let mut app = app();
    let e = spawn_input(&mut app, "hi", false);
    press_at(&mut app, glam::Vec2::new(5.0, 10.0));
    release_at(&mut app, glam::Vec2::new(5.0, 10.0));
    assert_eq!(app.world.resource::<FocusTracker>().0, Some(e));

    // Press far outside every candidate rect.
    press_at(&mut app, glam::Vec2::new(3000.0, 3000.0));
    assert_eq!(
        app.world.resource::<FocusTracker>().0,
        None,
        "pressing outside clears input focus"
    );
    assert!(app.world.get::<lumen_core::input::Focused>(e).is_none());
}

// --- drag selection ---------------------------------------------------------

#[test]
fn press_drag_extends_selection_with_fixed_anchor() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello world", false);
    press_at(&mut app, glam::Vec2::new(col_x(7), 10.0));
    move_to(&mut app, glam::Vec2::new(col_x(2), 10.0));

    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!(cursor, 2, "drag head follows the pointer");
    assert_eq!(sel, Some(7), "anchor stays at the press position");

    // Dragging past the anchor to the other side keeps the same anchor.
    move_to(&mut app, glam::Vec2::new(col_x(10), 10.0));
    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!(cursor, 10);
    assert_eq!(sel, Some(7), "anchor is preserved across direction flips");
    release_at(&mut app, glam::Vec2::new(col_x(10), 10.0));
    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!((cursor, sel), (10, Some(7)), "release keeps the selection");
}

// --- double / triple click --------------------------------------------------

#[test]
fn double_press_selects_word_triple_selects_line() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello world", false);
    let p = glam::Vec2::new(col_x(7), 10.0);
    press_at(&mut app, p);
    release_at(&mut app, p);
    press_at(&mut app, p);

    let (text, cursor, sel) = input_state(&app, e);
    assert_eq!(sel, Some(6), "double press selects the word start");
    assert_eq!(cursor, 11, "...through the word end");
    assert_eq!(&text[6..11], "world");

    release_at(&mut app, p);
    press_at(&mut app, p);
    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!(
        (sel, cursor),
        (Some(0), 11),
        "triple press selects the whole line"
    );
}

// --- shift+click ------------------------------------------------------------

#[test]
fn shift_press_extends_from_existing_caret() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello world", false);
    press_at(&mut app, glam::Vec2::new(col_x(7), 10.0));
    release_at(&mut app, glam::Vec2::new(col_x(7), 10.0));

    app.world.resource_mut::<ModifiersState>().0 = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    press_at(&mut app, glam::Vec2::new(col_x(2), 10.0));

    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!(cursor, 2, "shift+press moves the head to the press point");
    assert_eq!(sel, Some(7), "...keeping the pre-press caret as the anchor");
}

// --- tab-focus selects all --------------------------------------------------

#[test]
fn tab_focus_selects_all() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello", false);
    write_key(&mut app, Key::Named(NamedKey::Tab));
    app.tick();

    assert_eq!(app.world.resource::<FocusTracker>().0, Some(e));
    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!((sel, cursor), (Some(0), 5), "Tab-focus selects the value");
    let cur = app.world.get::<TextCursor>(e).unwrap();
    assert_eq!((cur.anchor.byte, cur.head.byte), (0, 5));

    // The next typed character replaces the whole value.
    write_char(&mut app, "x");
    app.tick();
    let (text, cursor, sel) = input_state(&app, e);
    assert_eq!(text, "x");
    assert_eq!((cursor, sel), (1, None));
}

// --- undo / redo ------------------------------------------------------------

fn focus_directly(app: &mut App, e: Entity) {
    app.world.entity_mut(e).insert(lumen_core::input::Focused);
    app.world.resource_mut::<FocusTracker>().0 = Some(e);
}

#[test]
fn undo_redo_round_trip_with_word_coalescing() {
    let mut app = app();
    let e = spawn_input(&mut app, "", false);
    focus_directly(&mut app, e);

    for ch in ["a", "b"] {
        write_char(&mut app, ch);
        app.tick();
    }
    write_key(&mut app, Key::Named(NamedKey::Space));
    app.tick();
    for ch in ["c", "d"] {
        write_char(&mut app, ch);
        app.tick();
    }
    assert_eq!(input_state(&app, e).0, "ab cd");

    // Typing coalesces into word-ish groups: "ab", " ", "cd".
    write_key_mods(&mut app, Key::Character("z".into()), ctrl(false));
    app.tick();
    assert_eq!(input_state(&app, e).0, "ab ", "first undo pops \"cd\"");
    write_key_mods(&mut app, Key::Character("z".into()), ctrl(false));
    app.tick();
    assert_eq!(input_state(&app, e).0, "ab", "second undo pops the space");
    write_key_mods(&mut app, Key::Character("z".into()), ctrl(false));
    app.tick();
    assert_eq!(input_state(&app, e).0, "", "third undo pops \"ab\"");

    // Ctrl+Shift+Z redoes...
    write_key_mods(&mut app, Key::Character("z".into()), ctrl(true));
    app.tick();
    assert_eq!(input_state(&app, e).0, "ab");
    // ...and Ctrl+Y redoes too.
    write_key_mods(&mut app, Key::Character("y".into()), ctrl(false));
    app.tick();
    assert_eq!(input_state(&app, e).0, "ab ");
    let (_, cursor, _) = input_state(&app, e);
    assert_eq!(cursor, 3, "redo lands the caret after the re-applied text");
}

#[test]
fn undo_restores_deleted_selection() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello world", false);
    // Select "world" by double-press...
    let p = glam::Vec2::new(col_x(7), 10.0);
    press_at(&mut app, p);
    release_at(&mut app, p);
    press_at(&mut app, p);
    release_at(&mut app, p);
    // ...delete it...
    write_key(&mut app, Key::Named(NamedKey::Backspace));
    app.tick();
    assert_eq!(input_state(&app, e).0, "hello ");
    // ...and undo brings it back.
    write_key_mods(&mut app, Key::Character("z".into()), ctrl(false));
    app.tick();
    assert_eq!(input_state(&app, e).0, "hello world");
}

// --- multiline vertical caret movement --------------------------------------

#[test]
fn arrow_up_down_move_across_lines() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello\nworld", true);
    focus_directly(&mut app, e);
    // Place the caret on line 2, column 2 ('r').
    {
        let mut em = app.world.entity_mut(e);
        let mut cur = em.get_mut::<TextCursor>().unwrap();
        let pos = lumen_core::text_model::TextPos::from_byte("hello\nworld", 8);
        cur.head = pos;
        cur.anchor = pos;
    }
    write_key(&mut app, Key::Named(NamedKey::ArrowUp));
    app.tick();
    let (_, cursor, _) = input_state(&app, e);
    assert_eq!(cursor, 2, "ArrowUp keeps the column on the previous line");

    write_key(&mut app, Key::Named(NamedKey::ArrowDown));
    app.tick();
    let (_, cursor, _) = input_state(&app, e);
    assert_eq!(cursor, 8, "ArrowDown returns to the original position");

    // Shift+ArrowUp extends the selection upward.
    write_key_mods(
        &mut app,
        Key::Named(NamedKey::ArrowUp),
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    app.tick();
    let (_, cursor, sel) = input_state(&app, e);
    assert_eq!((cursor, sel), (2, Some(8)));
}

#[test]
fn multiline_press_resolves_line_from_y() {
    let mut app = app();
    let e = spawn_input(&mut app, "hello\nworld", true);
    // Line height = 16 * 1.2 = 19.2 -> y = 25 is line 2. Column 2.
    press_at(&mut app, glam::Vec2::new(col_x(2), 25.0));
    let (_, cursor, _) = input_state(&app, e);
    assert_eq!(cursor, 8, "press on line 2 places the caret on line 2");
}

// --- caret blink ------------------------------------------------------------

#[test]
fn caret_blink_toggles_only_while_focused() {
    let mut app = app();
    let e = spawn_input(&mut app, "hi", false);

    // Unfocused: no blink wakeups at all (idle quiescence).
    app.tick();
    assert!(
        !app.world.resource::<AnimationsActive>().get(),
        "no input focused => blink must not raise AnimationsActive"
    );
    assert!(app.world.resource::<CaretBlink>().visible);

    press_at(&mut app, glam::Vec2::new(5.0, 10.0));
    assert!(
        app.world.resource::<AnimationsActive>().get(),
        "focused input => blink keeps the loop ticking"
    );
    assert!(
        app.world.resource::<CaretBlink>().visible,
        "fresh focus starts in the visible phase"
    );

    // Age the phase past one period -> hidden half.
    let period = app.world.resource::<CaretBlink>().period;
    app.world.resource_mut::<CaretBlink>().phase =
        std::time::Instant::now() - period - std::time::Duration::from_millis(10);
    app.tick();
    assert!(
        !app.world.resource::<CaretBlink>().visible,
        "one period elapsed => caret hidden"
    );

    // Any edit resets the phase to visible.
    focus_directly(&mut app, e);
    write_char(&mut app, "x");
    app.tick();
    assert!(
        app.world.resource::<CaretBlink>().visible,
        "an edit resets the blink phase to visible"
    );

    // Unfocus => visible again and fully quiescent.
    press_at(&mut app, glam::Vec2::new(3000.0, 3000.0));
    app.tick();
    assert!(!app.world.resource::<AnimationsActive>().get());
    assert!(app.world.resource::<CaretBlink>().visible);
}

// --- extract: placeholder + blink gate --------------------------------------

#[test]
fn placeholder_shows_while_focused_and_caret_respects_blink() {
    let mut app = app();
    let e = app
        .world
        .spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(400.0, 30.0),
                baseline_y: None,
            },
            Visuals {
                fill: Some(Fill::Solid(Color::rgb(0.1, 0.1, 0.1))),
                ..Default::default()
            },
            TextContent(String::new()),
            TextInput {
                placeholder: "type here".to_string(),
                cursor: 0,
                selection_anchor: None,
                multiline: false,
            },
            TabIndex(0),
        ))
        .id();
    app.tick();
    focus_directly(&mut app, e);
    app.tick();

    let extracted: Vec<ExtractedText> = {
        let mut q = app.render_world.query::<&ExtractedText>();
        q.iter(&app.render_world).cloned().collect()
    };
    assert_eq!(extracted.len(), 1);
    assert_eq!(
        extracted[0].text, "type here",
        "placeholder stays visible while the empty input is focused"
    );
    assert_eq!(
        extracted[0].caret,
        Some(0),
        "caret renders at offset 0 over the placeholder"
    );

    // Blink-hidden half withholds the caret from the extract. Age the
    // phase (the blink system recomputes `visible` from it each tick).
    let period = app.world.resource::<CaretBlink>().period;
    app.world.resource_mut::<CaretBlink>().phase =
        std::time::Instant::now() - period - std::time::Duration::from_millis(10);
    app.tick();
    let extracted: Vec<ExtractedText> = {
        let mut q = app.render_world.query::<&ExtractedText>();
        q.iter(&app.render_world).cloned().collect()
    };
    assert_eq!(extracted[0].caret, None, "hidden blink phase => no caret");
}
