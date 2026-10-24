// Needs `RunOptions` / `build_headless_app`, only available under `dev-run`.
#![cfg(feature = "dev-run")]

//! `<input type="color">` built at run time, headless, from real sources.
//!
//! A field a script builds is restyled before the color systems first run,
//! and the default skin gives the swatch no `bg`. The restyle took the
//! swatch's paint away, and the field's color never reached it.

use lumen_core::components::{Color, Fill, Visuals};
use lumen_primitives::ColorPart;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r#"<root id="app">
  <column id="host" />
  <script src="main.cdl" />
</root>
"#;

const SCRIPT: &str = r##"import "lumen.cdl";

fn on_ready() {
    get_by_id("host").set_inner_markup("<input type=\"color\" id=\"tint\" value=\"#3366ff\" />");
}

fn main() {}
"##;

#[test]
fn a_color_field_built_at_run_time_shows_its_color() {
    let dir = std::env::temp_dir().join(format!("lumen_color_field_{}", std::process::id()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), MARKUP).unwrap();
    std::fs::write(src.join("main.cdl"), SCRIPT).unwrap();
    // Port 0 keeps parallel test binaries off a shared socket.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..8 {
        app.tick();
    }

    let mut q = app.world.query::<(&ColorPart, Option<&Visuals>)>();
    let swatches: Vec<Option<Color>> = q
        .iter(&app.world)
        .filter(|(part, _)| **part == ColorPart::Swatch)
        .map(|(_, v)| v.and_then(|v| v.fill.as_ref()).and_then(Fill::as_solid))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(swatches.len(), 1, "the script built one field");
    let fill = swatches[0].expect("the swatch paints");
    let want = Color::from_rgba8([0x33, 0x66, 0xff, 0xff]);
    assert!(
        (fill.r - want.r).abs() < 0.01
            && (fill.g - want.g).abs() < 0.01
            && (fill.b - want.b).abs() < 0.01,
        "the swatch shows the field's color: {fill:?}"
    );
}
