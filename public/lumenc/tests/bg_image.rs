// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature. Gate the whole file so a thin
// (`--no-default-features`) `--all-targets` build compiles it out instead of
// failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Pixels of a `bg: url(...)` background.
//!
//! The image paints as the element's background: over nothing it replaces,
//! under the element's border and children, clipped to its rounded box. A
//! theme swaps it through `var()` and a restyle to a colour takes it away.
//!
//! Skips itself when the machine has no GPU (same convention as
//! `lumen-render-wgpu/tests/smoke.rs`).

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::components::{ImageComponent, LumenClasses, Transform};
use lumen_core::prelude::{App, Color, ColorScheme, LumenId, StyleManager, Viewport};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumen_text_cosmic::CosmicShaper;
use lumenc::{RunOptions, build_headless_app};

const W: u32 = 240;
const H: u32 = 160;

/// A rounded, bordered card whose background is an image a theme can swap,
/// holding one green chip; and an unsized box with a large background image
/// that must not grow to the image's size.
const MARKUP: &str = r#"<root>
  <tile id="card"><tile id="chip"/></tile>
  <row id="strip"><tile id="auto"/></row>
  <row><tile id="vector"/><image id="pic" src="art/day.png"/></row>
</root>"#;

const CSS: &str = r#"
:root { --hero: url("art/day.png"); }
:root.night { --hero: url("art/night.png"); }
#card {
  width: 200px; height: 100px; margin: 10px;
  radius: 30px; border: 4px solid #0000ff;
  bg: var(--hero);
}
:root.flat #card { bg: #00ffff; }
#chip { width: 30px; height: 30px; bg: #00ff00; }
#strip { height: 20px; align: start; }
#auto { height: 20px; bg: url("art/big.png"); bg-fit: contain; }
#vector { width: 20px; height: 20px; bg: url("art/dot.svg"); }
#pic { width: 20px; height: 20px; bg: url("art/night.png"); }
"#;

fn write_png(path: &std::path::Path, size: u32, rgba: [u8; 4]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    image::RgbaImage::from_pixel(size, size, image::Rgba(rgba))
        .save(path)
        .expect("write png");
}

fn boot(dir: &std::path::Path) -> App {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create temp app dir");
    // `[mcp] port = 0` keeps the introspection server off a TCP port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    write_png(&dir.join("art/day.png"), 8, [255, 0, 0, 255]);
    write_png(&dir.join("art/night.png"), 8, [255, 255, 0, 255]);
    write_png(&dir.join("art/big.png"), 64, [255, 0, 255, 255]);
    std::fs::write(
        dir.join("art/dot.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#00ff00"/></svg>"##,
    )
    .expect("write svg");

    let mut opts = RunOptions::new(dir).with_markup(MARKUP).with_css(CSS);
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
    app
}

/// Ticks long enough for the decode workers to answer and the frame to
/// repaint with what they answered.
fn settle(app: &mut App) {
    for _ in 0..40 {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn id_of(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no entity with id `{id}`"))
}

fn frame(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("offscreen renderer present")
        .read_rgba8()
        .expect("framebuffer readback")
}

/// The pixel at `at`, which must be inside the viewport.
fn pixel(pixels: &[u8], at: Vec2) -> [u8; 3] {
    let (x, y) = (at.x as usize, at.y as usize);
    let i = (y * W as usize + x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// Whether `px` is `want` within rasterization tolerance.
fn near(px: [u8; 3], want: [u8; 3]) -> bool {
    px.iter()
        .zip(want)
        .all(|(a, b)| (i16::from(*a) - i16::from(b)).abs() < 40)
}

fn set_root_classes(app: &mut App, classes: &[&str]) {
    let root = app.world.resource::<lumen_scene::spawn::DocumentRoot>().0;
    app.world.entity_mut(root).insert(LumenClasses(
        classes.iter().map(|c| std::sync::Arc::from(*c)).collect(),
    ));
    settle(app);
}

const RED: [u8; 3] = [255, 0, 0];
const YELLOW: [u8; 3] = [255, 255, 0];
const CYAN: [u8; 3] = [0, 255, 255];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const BLACK: [u8; 3] = [0, 0, 0];

#[test]
fn a_background_image_paints_behind_content_inside_its_box() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("lumenc-bg-image-{}", std::process::id()));
    let mut app = boot(&dir);

    let card = id_of(&mut app, "card");
    let chip = id_of(&mut app, "chip");
    let t = *app.world.get::<Transform>(card).expect("card laid out");
    let c = *app.world.get::<Transform>(chip).expect("chip laid out");
    let (o, s) = (t.absolute, t.size);
    let middle = o + s / 2.0 + Vec2::new(20.0, 10.0);
    let chip_middle = c.absolute + c.size / 2.0;
    let corner = o + Vec2::new(2.0, 2.0);
    let top_edge = o + Vec2::new(s.x / 2.0, 1.5);

    let px = frame(&app);
    assert!(
        near(pixel(&px, middle), RED),
        "image fills the card: {:?}",
        pixel(&px, middle)
    );
    assert!(
        near(pixel(&px, chip_middle), GREEN),
        "the child paints over the image"
    );
    assert!(
        near(pixel(&px, top_edge), BLUE),
        "the border paints over the image"
    );
    assert!(
        near(pixel(&px, corner), BLACK),
        "the image is clipped to the rounded corner"
    );

    // A background never sizes its element: the unsized box stays as wide as
    // its (empty) content, not as wide as its 64px image.
    let auto = id_of(&mut app, "auto");
    assert!(
        app.world.get::<lumen_assets::LoadedImage>(auto).is_some(),
        "the image decoded"
    );
    assert!(app.world.get::<ImageComponent>(auto).is_none());
    let size = app.world.get::<Transform>(auto).expect("laid out").size;
    assert_eq!(
        size.x, 0.0,
        "the background image sized its element: {size:?}"
    );

    // An SVG paints as a background the same way a bitmap does.
    let vector = id_of(&mut app, "vector");
    let v = *app.world.get::<Transform>(vector).expect("laid out");
    assert!(
        near(pixel(&px, v.absolute + v.size / 2.0), GREEN),
        "the SVG background: {:?}",
        pixel(&px, v.absolute + v.size / 2.0)
    );

    // An `<image>` keeps its `src` as its one source.
    let pic = id_of(&mut app, "pic");
    assert!(
        app.world
            .get::<lumen_assets::BackgroundImage>(pic)
            .is_none()
    );
    let p = *app.world.get::<Transform>(pic).expect("laid out");
    assert!(
        near(pixel(&px, p.absolute + p.size / 2.0), RED),
        "the image shows its src: {:?}",
        pixel(&px, p.absolute + p.size / 2.0)
    );

    // A theme swaps the image through the custom property it names.
    set_root_classes(&mut app, &["night"]);
    let px = frame(&app);
    assert!(
        near(pixel(&px, middle), YELLOW),
        "the theme's image: {:?}",
        pixel(&px, middle)
    );
    assert!(near(pixel(&px, chip_middle), GREEN));

    // A restyle to a colour takes the image away and paints the colour.
    set_root_classes(&mut app, &["night", "flat"]);
    let px = frame(&app);
    assert!(
        near(pixel(&px, middle), CYAN),
        "the colour replaces the image: {:?}",
        pixel(&px, middle)
    );
    assert!(
        app.world
            .get::<lumen_assets::BackgroundImage>(card)
            .is_none()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
