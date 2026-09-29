// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A scroll container's automatic minimum size.
//!
//! In CSS a scroll container's `min-height: auto` / `min-width: auto`
//! resolves to zero on both axes, so a growing scroller takes the
//! space it is given and scrolls the rest. Issue 406 was the gap: a
//! `<scroll grow="1">` floored at its content height, grew past the column
//! it sat in, and had nothing to scroll. Headless: no window, no GPU.

use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenId, Transform};
use lumen_core::prelude::Viewport;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_scroll_min_{}_{}", std::process::id(), {
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
    // The issue's 400x300 window. Layout follows the `Viewport` resource.
    app.world.resource_mut::<Viewport>().size = Vec2::new(400.0, 300.0);
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn size_of(app: &mut App, id: &str) -> Vec2 {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| t.size)
        .unwrap_or_else(|| panic!("no laid-out element with id `{id}`"))
}

/// The issue's layout: fifteen labels in a growing scroller, above a 30px
/// status row, in a 400x300 window. `scroll_attrs` goes on the `<scroll>`.
fn issue_markup(scroll_attrs: &str) -> String {
    let labels: String = (0..15)
        .map(|i| format!(r#"<label text="item {i}"/>"#))
        .collect();
    format!(
        r#"<root>
  <column width="100%" height="100%">
    <column grow="1" height="100%">
      <scroll id="scroller" grow="1" width="100%" {scroll_attrs}>
        <column id="content" gap="10">{labels}</column>
      </scroll>
    </column>
    <row height="30"><label text="status"/></row>
  </column>
</root>"#
    )
}

/// The scroller fills the 300px column it sits in and scrolls its taller
/// content, instead of growing to the content's height.
#[test]
fn a_growing_scroller_stays_at_the_space_it_is_given() {
    let mut app = build(&issue_markup(""));
    let content = size_of(&mut app, "content").y;
    assert!(
        content > 300.0,
        "the content must overflow for this to test anything, got {content}"
    );
    let scroller = size_of(&mut app, "scroller").y;
    assert_eq!(scroller, 300.0, "the scroller grew toward its content");
    // Same answer as the `min-height="0"` workaround the issue names.
    let mut workaround = build(&issue_markup(r#"min-height="0""#));
    assert_eq!(scroller, size_of(&mut workaround, "scroller").y);
}

/// An authored minimum still wins over the zero floor, even past the space
/// the column gives the scroller.
#[test]
fn an_authored_min_height_still_floors_the_scroller() {
    let mut app = build(&issue_markup(r#"min-height="350""#));
    assert_eq!(size_of(&mut app, "scroller").y, 350.0);
}

/// A horizontal scroller in a fixed-width row shrinks on its scrolling axis
/// the same way.
#[test]
fn a_horizontal_scroller_shrinks_in_a_crowded_row() {
    let tiles: String = (0..10)
        .map(|_| r#"<row width="60" height="20" shrink="0"/>"#)
        .collect();
    let mut app = build(&format!(
        r#"<root>
  <row width="400" height="100">
    <scroll id="scroller" scroll="x" grow="1">
      <row id="content">{tiles}</row>
    </scroll>
    <row width="100" height="100" shrink="0"/>
  </row>
</root>"#
    ));
    assert_eq!(size_of(&mut app, "content").x, 600.0);
    assert_eq!(size_of(&mut app, "scroller").x, 300.0);
}

/// A vertical scroller's cross axis loses its content floor too, as in CSS
/// (`overflow-y: scroll` computes `overflow-x` to `auto`) and on the web
/// target: in a crowded row it shrinks below its content's width.
#[test]
fn a_vertical_scroller_shrinks_on_its_cross_axis() {
    let mut app = build(
        r#"<root>
  <row width="400" height="100">
    <scroll id="scroller" grow="1">
      <row id="content" width="350" height="20" shrink="0"/>
    </scroll>
    <row width="100" height="100" shrink="0"/>
  </row>
</root>"#,
    );
    assert_eq!(size_of(&mut app, "content").x, 350.0);
    assert_eq!(size_of(&mut app, "scroller").x, 300.0);
}
