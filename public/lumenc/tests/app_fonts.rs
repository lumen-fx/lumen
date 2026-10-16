// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! Fonts an app ships with `@font-face`.
//!
//! The app directory holds a font file the machine has never seen: the test
//! font with a marker appended, which a font parser ignores and no installed
//! font carries. Text naming the rule's family must shape from that file, in
//! the layout world and in the render world, past the `woff2` meant for
//! browsers and a file that is not there.

use lumen_core::prelude::App;
use lumen_text::{ShapeOptions, ShaperService};
use lumenc::{RunOptions, build_headless_app};
use std::sync::Arc;

const MARKER: &[u8] = b"lumen-app-font-marker";

const MARKUP: &str = r#"<root><label id="t">Shipped</label></root>"#;

const CSS: &str = r#"
@font-face {
  font-family: "Pixel Demo";
  src: url("fonts/demo.woff2") format("woff2"), url("fonts/missing.ttf"), url("fonts/demo.ttf");
}
#t { font-family: "Pixel Demo", sans-serif; }
"#;

fn shaped_with_app_font(shaper: &mut ShaperService) -> bool {
    let opts = ShapeOptions {
        family: Some(Arc::from("Pixel Demo")),
        ..ShapeOptions::default()
    };
    let run = shaper.shape("Shipped", 16.0, opts).expect("text shapes");
    run.font_data.ends_with(MARKER)
}

#[test]
fn a_font_face_rule_ships_its_font_to_both_shapers() {
    let dir = std::env::temp_dir().join(format!("lumen-app-fonts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fonts")).expect("create temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    let mut font = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/goldens/fonts/NotoSans-Regular.ttf"
    ))
    .expect("read the test font");
    font.extend_from_slice(MARKER);
    std::fs::write(dir.join("fonts/demo.ttf"), font).expect("write the app font");

    let mut opts = RunOptions::new(&dir).with_markup(MARKUP).with_css(CSS);
    opts.hot_reload = false;
    let (mut app, window): (App, _) = build_headless_app(opts).expect("build headless app");
    // What `run_app` does with the render-side shaper the build hands back.
    let render_shaper = window.text_shaper.expect("a render shaper");
    app.render_world
        .insert_non_send(ShaperService::from(render_shaper));
    app.tick();
    app.tick();

    let mut layout = app
        .world
        .get_non_send_mut::<ShaperService>()
        .expect("layout shaper");
    assert!(
        shaped_with_app_font(&mut layout),
        "layout measures with the app's font"
    );
    let mut render = app
        .render_world
        .get_non_send_mut::<ShaperService>()
        .expect("render shaper");
    assert!(
        shaped_with_app_font(&mut render),
        "the renderer paints with the app's font"
    );

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
