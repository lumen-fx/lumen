// Drives the REAL pipeline (parse -> spawn -> layout -> shaping producer ->
// input) rather than hand-built geometry, so the vertical text origin is
// whatever the app actually renders with. Needs `lumenc::spawn` /
// `RunOptions` / `build_app`, which only exist under `dev-run`.
#![cfg(feature = "dev-run")]

//! Vertical text-geometry regression tests for multiline fields.
//!
//! The unit tests in `lumen-input` build a `TextGeometry` by hand and feed
//! it a `TextBlockOrigin` directly. That cannot catch a producer that
//! publishes the wrong origin, or a consumer that adds an offset the
//! producer already applied, because nothing in the test ever runs the
//! producer. These tests spawn a real `<textarea>`, let the layout plugin
//! shape it, and assert that a pointer press lands the caret on the line
//! the user clicked.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{TextBlockOrigin, TextInput, Transform};
use lumen_core::input::{PointerButton, PointerMoved, PointerPressed, PointerState};
use lumen_core::text_model::TextCursor;
use lumenc::RunOptions;
use lumenc::run::build_app;

/// Build the full app from inline markup and tick it, exactly like the
/// `run_pipeline` integration tests do.
fn build_and_tick(markup: &str, ticks: u32) -> App {
    let dir = std::env::temp_dir().join(format!(
        "lumenc_text_geom_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _winit) = build_app(opts).expect("build_app");
    app.add_plugin(lumen_window_winit::WinitPlugin);
    for _ in 0..ticks {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// The text the markup below puts in the field: two 4-byte lines, so a
/// byte offset identifies its line unambiguously. Line 0 is `0..=4`,
/// line 1 is `5..=9`.
const TWO_LINE_TEXT: &str = "AAAA\nBBBB";

const TWO_LINE: &str = r##"<root>
  <textarea id="ed" text="AAAA&#10;BBBB" width="400" height="240"
            bg="#223344" font-size="16" padding="8" />
</root>"##;

fn textarea_of(app: &mut App) -> (Entity, Transform) {
    let mut q = app
        .world
        .query_filtered::<(Entity, &Transform), With<TextInput>>();
    let (e, t) = q.single(&app.world).expect("one textarea");
    (e, *t)
}

/// Press at `(x, y)` in window coordinates and settle a tick.
fn click_at(app: &mut App, x: f32, y: f32) {
    let p = glam::Vec2::new(x, y);
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
    app.world.resource_mut::<PointerState>().primary_down = false;
}

fn caret_byte(app: &App, e: Entity) -> usize {
    app.world.get::<TextCursor>(e).expect("cursor").head.byte
}

/// The producer must publish an origin for a real spawned textarea. If it
/// does not, every consumer silently runs on its own fallback and the two
/// can drift apart without any test noticing.
#[test]
fn the_producer_publishes_an_origin_for_a_real_textarea() {
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    assert!(t.size.y > 100.0, "the box got its authored height: {t:?}");
    let origin = app
        .world
        .get::<TextBlockOrigin>(e)
        .expect("shaping producer published TextBlockOrigin");
    assert_eq!(
        origin.top, 0.0,
        "a textarea is a stacked block, so its first line starts at the \
         inner box top (got {})",
        origin.top
    );
}

/// Clicking lower must not move the caret to an earlier byte. This is the
/// owner-reported "on the second line caret goes up when clicking down".
#[test]
fn clicking_lower_never_moves_the_caret_to_an_earlier_line() {
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    let line_h = 16.0 * 1.2;
    let x = t.absolute.x + 10.0;
    // Sample down the field: first line band, second line band, and well
    // below the text (which must clamp to the last line, not wrap around).
    let top = t.absolute.y + 8.0;
    let mut last = 0usize;
    for (i, dy) in [line_h * 0.5, line_h * 1.5, line_h * 3.0]
        .into_iter()
        .enumerate()
    {
        click_at(&mut app, x, top + dy);
        let byte = caret_byte(&app, e);
        assert!(
            byte >= last,
            "click {i} at dy={dy} moved the caret backwards: {byte} < {last}"
        );
        last = byte;
    }
}

/// The concrete line mapping: a press inside the first line band resolves
/// to a byte on line 0, a press inside the second band to a byte on line 1.
#[test]
fn a_press_resolves_to_the_line_under_the_pointer() {
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    let line_h = 16.0 * 1.2;
    let x = t.absolute.x + 10.0;
    let top = t.absolute.y + 8.0;

    click_at(&mut app, x, top + line_h * 0.5);
    let first = caret_byte(&app, e);
    assert!(
        first <= 4,
        "press on the first line resolved to byte {first}, which is on line 1"
    );

    click_at(&mut app, x, top + line_h * 1.5);
    let second = caret_byte(&app, e);
    assert!(
        second >= 5,
        "press on the second line resolved to byte {second}, which is on line 0"
    );
}

/// The drawn caret and the edit byte must agree: after clicking on the
/// second line, the caret rect must sit in the second line's band. When the
/// shaped run's byte offsets aliased across lines, the caret drew on line
/// one while the edit applied on line two, which is the owner-reported
/// "deleting is not where the caret is".
#[test]
fn the_caret_rect_follows_the_clicked_line() {
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    let line_h = 16.0 * 1.2;
    let x = t.absolute.x + 10.0;
    let top = t.absolute.y + 8.0;

    click_at(&mut app, x, top + line_h * 1.5);
    let byte = caret_byte(&app, e);
    let geom = &app
        .world
        .get::<lumen_text::ShapedText>(e)
        .expect("shaped")
        .geometry;
    // `CaretRect::top` is measured from the FIRST line's baseline, so the
    // absolute value is not the interesting part: the caret for a byte on
    // line two must sit exactly one line height below the caret for a byte
    // on line one.
    let first = geom.byte_to_caret(0).top;
    let second = geom.byte_to_caret(byte).top;
    assert!(
        (second - first - line_h).abs() < 0.5,
        "caret for byte {byte} drew {} below the line-one caret, expected \
         one line height ({line_h})",
        second - first
    );
}

/// Typing after a click inserts at the clicked point, not at a byte that
/// aliased onto an earlier line.
#[test]
fn typing_after_a_click_inserts_at_the_clicked_point() {
    use lumen_core::input::{Key, KeyPressed, Modifiers};
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    let line_h = 16.0 * 1.2;
    // Click at the very start of the second line.
    click_at(
        &mut app,
        t.absolute.x + 9.0,
        t.absolute.y + 8.0 + line_h * 1.5,
    );
    app.world
        .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Character("Z".into()),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    app.tick();
    let text = app
        .world
        .get::<lumen_core::text_model::TextBuffer>(e)
        .expect("buffer")
        .to_string();
    assert_eq!(
        text, "AAAA\nZBBBB",
        "the typed character landed away from the clicked caret"
    );
}

/// Backspace removes the character immediately before the caret the user
/// can see, on whichever line that is.
#[test]
fn backspace_after_a_click_deletes_at_the_clicked_point() {
    use lumen_core::input::{Key, KeyPressed, Modifiers, NamedKey};
    let mut app = build_and_tick(TWO_LINE, 4);
    let (e, t) = textarea_of(&mut app);
    let line_h = 16.0 * 1.2;
    // Click between the first and second 'B' on line two.
    let x = t.absolute.x + 8.0 + 16.0;
    click_at(&mut app, x, t.absolute.y + 8.0 + line_h * 1.5);
    let before = caret_byte(&app, e);
    assert!(
        before >= 5,
        "clicked byte {before} is not on the second line"
    );
    app.world
        .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
        .write(KeyPressed {
            key: Key::Named(NamedKey::Backspace),
            modifiers: Modifiers::default(),
            repeat: false,
        });
    app.tick();
    let text = app
        .world
        .get::<lumen_core::text_model::TextBuffer>(e)
        .expect("buffer")
        .to_string();
    assert!(
        text.starts_with("AAAA\n") && text.len() == TWO_LINE_TEXT.len() - 1,
        "backspace on line two edited elsewhere: {text:?}"
    );
}
