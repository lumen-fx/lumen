// Drives the real pipeline (parse -> cascade -> spawn -> layout -> render
// extract), which needs `RunOptions` / `build_headless_app`; lumenc only
// exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! Where a control's label sits inside it.
//!
//! A `<button>` holds its text itself rather than in a child element, so
//! nothing in the entity tree shows whether `justify-content` reached the
//! label. The answer only exists in what the extract hands the renderer,
//! which is what these tests read. Headless: no window, no GPU.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{LumenId, TextAlign};
use lumen_core::render_world::{ExtractedText, RenderEntityMap, extract_text};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_label_place_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string())
        .with_css(css.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// Run the render extract and return what it says about the text of the
/// element with this id.
fn extracted_text(app: &mut App, id: &str) -> ExtractedText {
    let entity = {
        let mut q = app.world.query::<(Entity, &LumenId)>();
        q.iter(&app.world)
            .find(|(_, name)| name.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no element with id `{id}`"))
    };
    let mut render = World::new();
    render.init_resource::<RenderEntityMap>();
    extract_text(&mut app.world, &mut render);
    let drawn = *render
        .resource::<RenderEntityMap>()
        .text
        .get(&entity)
        .unwrap_or_else(|| panic!("`{id}` extracted no text"));
    render
        .get::<ExtractedText>(drawn)
        .expect("extracted text")
        .clone()
}

/// A flex box lays its text out as an item of its own, so centring the
/// contents of a `<button>` is `justify-content`, the same property that
/// centres a child. It used to reach nothing: the label was drawn across
/// the whole content box and only `text-align` moved it.
#[test]
fn justify_content_places_a_button_label() {
    let mut app = build(
        r#"<root><button id="go" width="200" justify="center" text="Go"/></root>"#,
        "",
    );
    assert_eq!(extracted_text(&mut app, "go").align, TextAlign::Center);
}

/// Same through a stylesheet, which is where a skin says it.
#[test]
fn a_css_rule_places_a_button_label_too() {
    let mut app = build(
        r#"<root><button id="go" width="200" text="Go"/></root>"#,
        "button { justify-content: end; }",
    );
    assert_eq!(extracted_text(&mut app, "go").align, TextAlign::End);
}

/// `text-align` places the lines and outranks the distribution: an author
/// who named one gets it.
#[test]
fn an_authored_text_align_wins() {
    let mut app = build(
        r#"<root><button id="go" width="200" justify="center" text-align="end" text="Go"/></root>"#,
        "",
    );
    assert_eq!(extracted_text(&mut app, "go").align, TextAlign::End);
}

/// Under a vertical main axis `justify-content` distributes down the box
/// and says nothing about where a line starts.
#[test]
fn a_column_leaves_the_run_where_it_started() {
    let mut app = build(
        r#"<root><button id="go" width="200" justify="center" text="Go"/></root>"#,
        "button { flex-direction: column; }",
    );
    assert_eq!(extracted_text(&mut app, "go").align, TextAlign::Start);
}

/// The default is untouched: a button nobody aligned still starts its
/// label at the leading edge.
#[test]
fn an_unaligned_button_starts_its_label_at_the_edge() {
    let mut app = build(
        r#"<root><button id="go" width="200" text="Go"/></root>"#,
        "",
    );
    assert_eq!(extracted_text(&mut app, "go").align, TextAlign::Start);
}
