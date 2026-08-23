// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead of
// failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Pixels behind `overflow: hidden`.
//!
//! An `overflow: hidden` box clips to where it is drawn. Inside a scrolled
//! container that is not where it was laid out, and issue 136 was the gap: the
//! clip rect kept the raw layout origin while everything under it moved with
//! the scroll, so scrolling a card into view painted an empty box.
//!
//! Skips itself when the machine has no GPU (same convention as
//! `lumen-render-wgpu/tests/smoke.rs`).

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::input::ScrollOffset;
use lumen_core::prelude::{App, Color, ColorScheme, LumenId, StyleManager, Viewport};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumen_text_cosmic::CosmicShaper;
use lumenc::{RunOptions, build_headless_app};

/// 200x100 viewport onto 400px of scrollable content.
const W: u32 = 200;
const H: u32 = 100;

/// A card 200px down the scroll content, absolutely positioned across its
/// container and clipping its own overflow, holding one red tile.
const MARKUP: &str = r#"<root>
  <scroll id="scroller">
    <column id="content">
      <tile id="spacer"/>
      <tile id="card"><tile id="tile"/></tile>
    </column>
  </scroll>
</root>"#;

const CSS: &str = "\
#scroller { width: 200px; height: 100px; } \
#content { width: 200px; height: 400px; } \
#spacer { width: 10px; height: 200px; } \
#card { position: absolute; inset: 200px 0 140px 0; overflow: hidden; } \
#tile { width: 50px; height: 50px; background: #ff0000; }";

fn boot(name: &str) -> App {
    let dir = std::env::temp_dir().join(format!(
        "lumenc-overflow-clip-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp app dir");
    // `[mcp] port = 0` keeps the introspection server off a TCP port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");

    let mut opts = RunOptions::new(&dir).with_markup(MARKUP).with_css(CSS);
    opts.hot_reload = false;
    opts.size = (W, H);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.add_plugin(WgpuRendererPlugin::new(W, H).with_text_shaper(CosmicShaper::new()));
    for vp in [
        &mut *app.world.resource_mut::<Viewport>(),
        &mut *app.render_world.resource_mut::<Viewport>(),
    ] {
        vp.size = Vec2::new(W as f32, H as f32);
        vp.scale_factor = 1.0;
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    // Pin the scheme so the OS light/dark preference cannot reach the cascade.
    app.world
        .resource_mut::<StyleManager>()
        .set_scheme(ColorScheme::ForceDark);
    settle(&mut app);
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn settle(app: &mut App) {
    for _ in 0..12 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

fn id_of(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no entity with id `{id}`"))
}

/// Count of red pixels in the captured frame.
fn red_pixels(app: &App) -> usize {
    let pixels = app
        .render_world
        .get_non_send::<WgpuRenderer>()
        .expect("offscreen renderer present")
        .read_rgba8()
        .expect("framebuffer readback");
    pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 150 && px[1] < 90 && px[2] < 90)
        .count()
}

/// The card sits below the fold until the container scrolls it into view;
/// once it is on screen its content has to paint (issue 136 painted nothing).
#[test]
fn scrolled_overflow_hidden_card_paints_its_content() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = boot("scrolled");
    assert_eq!(
        red_pixels(&app),
        0,
        "the card starts 200px below a 100px viewport"
    );

    let scroller = id_of(&mut app, "scroller");
    app.world
        .get_mut::<ScrollOffset>(scroller)
        .expect("scroll container carries an offset")
        .0 = Vec2::new(0.0, 200.0);
    settle(&mut app);

    // The tile is 50x50 and lands fully inside both the card and the viewport.
    assert_eq!(
        red_pixels(&app),
        2500,
        "the scrolled-in card paints its whole tile"
    );
}

/// The other half of the same rect: content that overflows the card is still
/// cut off, so the fix moves the clip rather than dropping it.
#[test]
fn overflow_hidden_card_still_clips_what_leaves_it() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = boot("clips");
    let tile = id_of(&mut app, "tile");
    // 200px tall inside a 60px card: only the top 60px may paint.
    app.world
        .entity_mut(tile)
        .get_mut::<lumen_core::components::Style>()
        .expect("tile carries a style")
        .height = lumen_core::components::Length::Px(200.0);
    app.world
        .entity_mut(tile)
        .insert(lumen_core::components::DirtyLayout);
    let scroller = id_of(&mut app, "scroller");
    app.world
        .get_mut::<ScrollOffset>(scroller)
        .expect("scroll container carries an offset")
        .0 = Vec2::new(0.0, 200.0);
    settle(&mut app);

    assert_eq!(
        red_pixels(&app),
        50 * 60,
        "the card clips the tile at its own bottom edge"
    );
}
