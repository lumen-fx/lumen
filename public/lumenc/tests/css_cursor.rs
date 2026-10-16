// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! CSS `cursor`, from the stylesheet to the request the window applies.
//!
//! The pointer is moved over each element the way the window backend moves
//! it, and the test reads back the [`CursorRequest`] the window would show:
//! the hovered element's own value, one it inherits, the text field default
//! the user-agent sheet gives, and a `url(...)` image decoded through the
//! asset pipeline with its hotspot, falling back to its keyword while it
//! cannot load.

use glam::Vec2;
use lumen_core::components::Transform;
use lumen_core::input::{CursorKeyword, CursorRequest, PointerMoved, PointerState};
use lumen_core::prelude::{App, LumenId};
use lumenc::{RunOptions, build_headless_app};

const MARKUP: &str = r#"<root>
  <tile id="plain"/>
  <tile id="mover"><tile id="inner"/></tile>
  <input id="field"/>
  <tile id="pen"/>
  <tile id="missing"/>
  <tile id="hidden"/>
</root>"#;

const CSS: &str = r#"
root { padding: 0; gap: 4px; align: start; }
tile { width: 40px; height: 20px; }
#mover { cursor: move; height: 40px; }
#inner { width: 20px; height: 20px; bg: #333333; }
#pen { cursor: url("art/pen.png") 2 3, crosshair; }
#missing { cursor: url("art/nope.png"), not-allowed; }
#hidden { cursor: none; }
"#;

fn boot(dir: &std::path::Path) -> App {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("art")).expect("create temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255]))
        .save(dir.join("art/pen.png"))
        .expect("write png");
    let mut opts = RunOptions::new(dir).with_markup(MARKUP).with_css(CSS);
    opts.hot_reload = false;
    opts.size = (200, 300);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    settle(&mut app);
    app
}

fn settle(app: &mut App) {
    for _ in 0..30 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn center_of(app: &mut App, id: &str) -> Vec2 {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| t.absolute + t.size * 0.5)
        .unwrap_or_else(|| panic!("no entity with id `{id}`"))
}

/// Move the pointer over `id` and hand back what the window is asked for.
fn hover(app: &mut App, id: &str) -> CursorRequest {
    let at = center_of(app, id);
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    settle(app);
    app.world.resource::<CursorRequest>().clone()
}

#[test]
fn the_hovered_element_picks_the_cursor() {
    let dir = std::env::temp_dir().join(format!("lumen-css-cursor-{}", std::process::id()));
    let mut app = boot(&dir);

    let plain = hover(&mut app, "plain");
    assert_eq!(plain.cursor.keyword, CursorKeyword::Auto);
    assert!(plain.cursor.images.is_empty() && plain.image.is_none());

    assert_eq!(hover(&mut app, "mover").cursor.keyword, CursorKeyword::Move);
    assert_eq!(
        hover(&mut app, "inner").cursor.keyword,
        CursorKeyword::Move,
        "a child inherits its parent's cursor"
    );
    assert_eq!(
        hover(&mut app, "field").cursor.keyword,
        CursorKeyword::Text,
        "a text field shows the I-beam from the user-agent sheet"
    );
    assert_eq!(
        hover(&mut app, "hidden").cursor.keyword,
        CursorKeyword::None
    );

    let pen = hover(&mut app, "pen");
    assert_eq!(pen.cursor.keyword, CursorKeyword::Crosshair);
    let image = pen.image.expect("the cursor image decoded");
    assert_eq!((image.width, image.height), (8, 8));
    assert_eq!(image.hotspot, (2, 3));
    assert_eq!(&image.rgba[..4], &[0, 255, 0, 255]);

    let missing = hover(&mut app, "missing");
    assert_eq!(missing.cursor.keyword, CursorKeyword::NotAllowed);
    assert!(
        missing.image.is_none(),
        "an image that cannot load leaves the keyword showing"
    );

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
