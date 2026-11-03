// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A wrapping label's automatic minimum width in a row.
//!
//! In CSS a flex item's automatic minimum width is its min-content width,
//! which for text that wraps is its longest word. So a `grow="1"` label
//! beside a button on a narrow row shrinks to what the button leaves and
//! wraps, directly or inside a `<column grow="1">`. Issue 609 was the gap:
//! the label kept its one-line width and pushed the button off the row.
//! Headless: no window, no GPU.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextStyle, Transform};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const SENTENCE: &str = "A long description of a mod that keeps going well past the width of \
                        the card it sits on, which is what the search list shows";

fn build(markup: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_wrap_min_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let mut opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(800.0, 400.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn tf(app: &mut App, id: &str) -> Transform {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("no laid-out element with id `{id}`"))
}

fn text_size(app: &mut App, id: &str) -> f32 {
    let mut q = app.world.query::<(&LumenId, &TextStyle)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| t.size_px)
        .unwrap_or_else(|| panic!("no text on `{id}`"))
}

/// The label and button share the 380px row: the button ends inside it,
/// right after the label, and the label wraps onto more lines than one.
fn assert_fits(app: &mut App, item: &str) {
    let row = tf(app, "row");
    let item = tf(app, item);
    let label = tf(app, "label");
    let button = tf(app, "add");
    assert_eq!(row.size.x, 380.0);
    let row_end = row.absolute.x + row.size.x;
    let button_end = button.absolute.x + button.size.x;
    assert!(
        button_end <= row_end + 0.5,
        "the button ends at {button_end}, past the row's end at {row_end}"
    );
    assert!(
        (item.absolute.x + item.size.x - button.absolute.x).abs() < 0.5,
        "the button follows the label's box"
    );
    assert!(button.size.x > 0.0, "the button keeps its own width");
    // One line of text is about 1.2 font sizes tall; two are about 2.4.
    let font_px = text_size(app, "label");
    assert!(
        label.size.y > 1.8 * font_px,
        "the label wraps (height {} at {font_px}px text)",
        label.size.y
    );
}

#[test]
fn a_wrapping_label_shrinks_beside_a_button() {
    let mut app = build(&format!(
        r#"<root>
  <row id="row" width="380px" align="start">
    <label id="label" grow="1" wrap="word" text="{SENTENCE}"/>
    <button id="add" text="Add"/>
  </row>
</root>"#
    ));
    assert_fits(&mut app, "label");
}

#[test]
fn a_wrapping_label_in_a_growing_column_shrinks_beside_a_button() {
    let mut app = build(&format!(
        r#"<root>
  <row id="row" width="380px" align="start">
    <column id="col" grow="1">
      <label id="label" wrap="word" text="{SENTENCE}"/>
    </column>
    <button id="add" text="Add"/>
  </row>
</root>"#
    ));
    assert_fits(&mut app, "col");
}
