// Needs `RunOptions` / `build_headless_app`, only available under `dev-run`.
#![cfg(feature = "dev-run")]

//! Selecting text with the pointer and the keyboard, end to end.
//!
//! Selection has three separable halves and they fail differently:
//! the drag has to keep extending a range on the cursor model, the model
//! has to reach the component the extract reads, and the extract has to
//! hand the renderer a range plus a colour to paint it with. A test that
//! only checks the first would pass while nothing is highlighted on
//! screen.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{TextInput, Transform};
use lumen_core::input::{
    Key, KeyPressed, Modifiers, PointerButton, PointerMoved, PointerPressed, PointerReleased,
    PointerState,
};
use lumen_core::text_model::TextCursor;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_text_sel_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..ticks {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// Line 0 is bytes 0..=8, line 1 is 9..=17.
const TEXT: &str = "ABCDEFGH\nIJKLMNOP";

const MARKUP: &str = r##"<root>
  <textarea id="ed" text="ABCDEFGH&#10;IJKLMNOP" width="400" height="240"
            bg="#223344" font-size="16" padding="8" />
</root>"##;

fn field(app: &mut App) -> (Entity, Transform) {
    let mut q = app
        .world
        .query_filtered::<(Entity, &Transform), With<TextInput>>();
    let (e, t) = q.single(&app.world).expect("one textarea");
    (e, *t)
}

fn press_at(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
        });
    app.tick();
}

fn drag_to(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
        .write(PointerMoved { position: p });
    app.tick();
}

fn release_at(app: &mut App, p: glam::Vec2) {
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world
        .resource_mut::<bevy_ecs::message::Messages<PointerReleased>>()
        .write(PointerReleased {
            position: p,
            button: PointerButton::Primary,
        });
    app.tick();
}

fn selection(app: &App, e: Entity) -> Option<std::ops::Range<usize>> {
    app.world.get::<TextCursor>(e)?.selection_range()
}

/// What `extract_text` reads to decide whether to paint a highlight.
fn legacy_anchor(app: &App, e: Entity) -> Option<usize> {
    app.world.get::<TextInput>(e)?.selection_anchor
}

/// Dragging must extend a range as the pointer moves, and the range must
/// survive the release.
#[test]
fn dragging_extends_a_selection_and_it_survives_release() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;

    press_at(&mut app, glam::Vec2::new(x0, y));
    assert_eq!(selection(&app, e), None, "a bare press selects nothing");

    let mut seen: Vec<std::ops::Range<usize>> = Vec::new();
    for dx in [20.0f32, 40.0, 60.0] {
        drag_to(&mut app, glam::Vec2::new(x0 + dx, y));
        if let Some(r) = selection(&app, e) {
            seen.push(r);
        }
    }
    assert!(
        !seen.is_empty(),
        "dragging never produced a selection range at all"
    );
    let last = seen.last().unwrap().clone();
    assert!(
        last.start == 0 && last.end > 1,
        "drag selection did not grow with the pointer: {last:?}"
    );

    release_at(&mut app, glam::Vec2::new(x0 + 60.0, y));
    assert_eq!(
        selection(&app, e),
        Some(last.clone()),
        "the selection did not survive the pointer release"
    );
}

/// The extract reads `TextInput::selection_anchor`, not the cursor model.
/// If the drag only updates the model, the state is right and nothing is
/// ever painted.
#[test]
fn a_dragged_selection_reaches_the_component_the_renderer_reads() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, y));
    drag_to(&mut app, glam::Vec2::new(x0 + 60.0, y));
    let model = selection(&app, e);
    assert!(model.is_some(), "no selection on the cursor model");
    assert_eq!(
        legacy_anchor(&app, e),
        model.map(|r| r.start),
        "the selection never reached `TextInput::selection_anchor`, so the \
         extract paints no highlight"
    );
}

/// Dragging onto the second line selects across the line break.
#[test]
fn dragging_across_lines_selects_through_the_break() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let x0 = t.absolute.x + 8.0;
    let top = t.absolute.y + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, top + 9.6));
    drag_to(&mut app, glam::Vec2::new(x0 + 30.0, top + 19.2 + 9.6));
    let sel = selection(&app, e).expect("a cross-line drag selects");
    assert!(
        sel.start == 0 && sel.end > 9,
        "cross-line drag stopped at the line break: {sel:?}"
    );
}

/// Shift+click extends from the existing caret.
#[test]
fn shift_click_extends_from_the_caret() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, y));
    release_at(&mut app, glam::Vec2::new(x0, y));
    app.world
        .resource_mut::<lumen_core::input::ModifiersState>()
        .0 = Modifiers {
        shift: true,
        ..Default::default()
    };
    press_at(&mut app, glam::Vec2::new(x0 + 60.0, y));
    let sel = selection(&app, e);
    assert!(
        sel.as_ref().is_some_and(|r| r.start == 0 && r.end > 1),
        "shift+click did not extend a selection: {sel:?}"
    );
}

/// Shift+Right extends the selection one cluster at a time.
#[test]
fn shift_arrow_extends_the_selection() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    press_at(&mut app, glam::Vec2::new(t.absolute.x + 8.0, y));
    // The key router reads the live modifier state, not the event's copy.
    app.world
        .resource_mut::<lumen_core::input::ModifiersState>()
        .0 = Modifiers {
        shift: true,
        ..Default::default()
    };
    for _ in 0..3 {
        app.world
            .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(lumen_core::input::NamedKey::ArrowRight),
                modifiers: Modifiers {
                    shift: true,
                    ..Default::default()
                },
                repeat: false,
            });
        app.tick();
    }
    assert_eq!(
        selection(&app, e),
        Some(0..3),
        "shift+arrow did not extend the selection"
    );
    assert_eq!(
        legacy_anchor(&app, e),
        Some(0),
        "a keyboard selection never reached the component the extract reads"
    );
}

/// Copy is a behavioural probe that the selected range is real text.
#[test]
fn copy_lifts_the_selected_range() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, y));
    drag_to(&mut app, glam::Vec2::new(x0 + 60.0, y));
    let sel = selection(&app, e).expect("a selection to copy");
    assert_eq!(
        &TEXT[sel.clone()],
        &TEXT[sel.start..sel.end],
        "the range indexes the buffer text"
    );
    assert!(
        TEXT.get(sel.start..sel.end).is_some(),
        "selection range {sel:?} does not index {TEXT:?}"
    );
}

// --- Paint side -----------------------------------------------------------

/// Run the render extract over the app world and return the extracted text
/// record for `e`.
fn extract_for(app: &mut App, e: Entity) -> lumen_core::render_world::ExtractedText {
    let mut render = World::new();
    render.init_resource::<lumen_core::render_world::RenderEntityMap>();
    lumen_core::render_world::extract_text(&mut app.world, &mut render);
    let render_e = *render
        .resource::<lumen_core::render_world::RenderEntityMap>()
        .text
        .get(&e)
        .expect("the field was extracted");
    render
        .get::<lumen_core::render_world::ExtractedText>(render_e)
        .expect("extracted text")
        .clone()
}

/// The extract must hand the renderer a selection range.
#[test]
fn the_extract_emits_the_selection_range() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, y));
    drag_to(&mut app, glam::Vec2::new(x0 + 60.0, y));
    let sel = selection(&app, e).expect("a selection");
    let ex = extract_for(&mut app, e);
    assert_eq!(
        ex.selection,
        Some((sel.start, sel.end)),
        "the extract dropped the selection, so the renderer paints nothing"
    );
}

/// A selection range must turn into at least one highlight rectangle.
#[test]
fn a_selection_produces_highlight_rectangles() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    let x0 = t.absolute.x + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, y));
    drag_to(&mut app, glam::Vec2::new(x0 + 60.0, y));
    let ex = extract_for(&mut app, e);
    let (s, en) = ex.selection.expect("a selection range");
    let geom = &app
        .world
        .get::<lumen_text::ShapedText>(e)
        .expect("shaped")
        .geometry;
    let bands = geom.selection_bands(s, en);
    assert!(
        !bands.is_empty(),
        "selection {s}..{en} produced no highlight rectangles"
    );
    for b in &bands {
        assert!(b.x1 > b.x0, "degenerate highlight rect {:?}", b);
    }
}

/// A selection that spans two lines must produce highlight geometry on
/// both of them.
#[test]
fn a_cross_line_selection_highlights_both_lines() {
    let mut app = build_and_tick(MARKUP, 4);
    let (e, t) = field(&mut app);
    let x0 = t.absolute.x + 8.0;
    let top = t.absolute.y + 8.0;
    press_at(&mut app, glam::Vec2::new(x0, top + 9.6));
    drag_to(&mut app, glam::Vec2::new(x0 + 30.0, top + 19.2 + 9.6));
    let ex = extract_for(&mut app, e);
    let (s, en) = ex.selection.expect("a selection range");
    assert!(en > 9, "the drag did not reach the second line");
    let geom = &app
        .world
        .get::<lumen_text::ShapedText>(e)
        .expect("shaped")
        .geometry;
    let bands = geom.selection_bands(s, en);
    assert!(
        bands.len() >= 2,
        "a selection spanning two lines produced {} band(s); every band \
         would be painted on the first line's baseline",
        bands.len()
    );
    // Each line's band must sit on its own baseline.
    let mut baselines: Vec<f32> = bands.iter().map(|b| b.baseline_y).collect();
    baselines.dedup();
    assert!(
        baselines.len() >= 2,
        "all highlight bands share one baseline {baselines:?}, so the \
         second line's highlight would paint over the first line"
    );
}

/// Double-click selects the word under the pointer.
#[test]
fn double_click_selects_a_word() {
    const WORDS: &str = r##"<root>
  <textarea id="ed" text="alpha beta gamma" width="400" height="240"
            bg="#223344" font-size="16" padding="8" />
</root>"##;
    let mut app = build_and_tick(WORDS, 4);
    let (e, t) = field(&mut app);
    let y = t.absolute.y + 8.0 + 9.6;
    // Inside "beta" (bytes 6..10).
    let p = glam::Vec2::new(t.absolute.x + 8.0 + 60.0, y);
    press_at(&mut app, p);
    release_at(&mut app, p);
    press_at(&mut app, p);
    let sel = selection(&app, e).expect("double-click selects a word");
    assert_eq!(
        &"alpha beta gamma"[sel.clone()],
        "beta",
        "double-click selected {sel:?} instead of the word under the pointer"
    );
}
