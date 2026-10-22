// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! Pixels of CSS `mask-image`.
//!
//! A two-pixel mask (opaque, then clear) contained and centered in a square
//! box: the box's background shows through the opaque half only, and only
//! inside the strip the mask covers. A child paints through the same mask.
//! A restyle that drops the rule shows the whole box again. Runs on the CPU
//! renderer everywhere and on the GPU renderer where the machine has one.

use glam::Vec2;
use lumen_core::components::{LumenClasses, Transform};
use lumen_core::prelude::{App, Color, ColorScheme, LumenId, StyleManager};
use lumen_core::render_world::ExtractedMaskGroup;
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumenc::{RunOptions, build_headless_app};

const W: u32 = 120;
const H: u32 = 60;

const MARKUP: &str = r#"<root>
  <row id="bar">
    <tile id="tinted"/>
    <tile id="parent"><tile id="kid"/></tile>
  </row>
</root>"#;

const CSS: &str = r#"
root { padding: 0; align: start; }
#bar { gap: 10px; align: start; }
#tinted, #parent { width: 40px; height: 40px; bg: #ff0000; }
#kid { width: 20px; height: 40px; bg: #00ff00; }
:root.masked #tinted, :root.masked #parent {
  mask-image: url("art/mask.png");
  mask-size: contain;
  mask-repeat: no-repeat;
  mask-position: center;
}
"#;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Cpu,
    Gpu,
}

fn boot(dir: &std::path::Path, backend: Backend, markup: &str, css: &str) -> App {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("art")).expect("create temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    let mut mask = image::RgbaImage::new(2, 1);
    mask.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
    mask.put_pixel(1, 0, image::Rgba([255, 255, 255, 0]));
    mask.save(dir.join("art/mask.png")).expect("write png");

    let mut opts = RunOptions::new(dir).with_markup(markup).with_css(css);
    opts.hot_reload = false;
    opts.size = (W, H);
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    match backend {
        Backend::Cpu => app.add_plugin(CpuRendererPlugin::new(W, H)),
        Backend::Gpu => app.add_plugin(WgpuRendererPlugin::new(W, H)),
    };
    {
        let vp = &mut *lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = Vec2::new(W as f32, H as f32);
        vp.scale_factor = 1.0;
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    app.world
        .resource_mut::<StyleManager>()
        .set_scheme(ColorScheme::ForceDark);
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

fn set_root_classes(app: &mut App, classes: &[&str]) {
    let root = app.world.resource::<lumen_scene::spawn::DocumentRoot>().0;
    app.world.entity_mut(root).insert(LumenClasses(
        classes.iter().map(|c| std::sync::Arc::from(*c)).collect(),
    ));
    settle(app);
}

fn origin_of(app: &mut App, id: &str) -> Vec2 {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| t.absolute)
        .unwrap_or_else(|| panic!("no entity with id `{id}`"))
}

/// The pixel at `(x, y)` inside `id`'s box.
fn pixel(app: &mut App, id: &str, x: f32, y: f32) -> [u8; 3] {
    let at = origin_of(app, id) + Vec2::new(x, y);
    let pixels = match app.render_world.get_non_send::<CpuRenderer>() {
        Some(cpu) => cpu
            .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window")),
        None => app
            .render_world
            .get_non_send::<WgpuRenderer>()
            .expect("offscreen renderer present")
            .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
            .expect("framebuffer readback"),
    };
    let i = (at.y as usize * W as usize + at.x as usize) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// Within a few steps of `want` on every channel: the mask is a two-pixel
/// image scaled up, so its opaque half samples just under full alpha.
fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 12)
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLACK: [u8; 3] = [0, 0, 0];

#[test]
fn mask_image_paints_an_element_through_the_image_alpha() {
    check(Backend::Cpu);
    match gpu_unavailable_reason() {
        None => check(Backend::Gpu),
        Some(why) => eprintln!("skipping the GPU half: {why}"),
    }
    without_masks_nothing_is_extracted();
}

fn check(backend: Backend) {
    let dir =
        std::env::temp_dir().join(format!("lumen-css-mask-{backend:?}-{}", std::process::id()));
    let mut app = boot(&dir, backend, MARKUP, CSS);
    set_root_classes(&mut app, &["masked"]);

    // Contained, the 2x1 mask is a 40x20 strip across the middle of the
    // 40x40 box, its left half opaque.
    for (x, y, want, what) in [
        (10.0, 20.0, RED, "the opaque half shows the background"),
        (30.0, 20.0, BLACK, "the clear half hides it"),
        (10.0, 4.0, BLACK, "above the strip nothing shows"),
        (10.0, 36.0, BLACK, "below the strip nothing shows"),
    ] {
        let got = pixel(&mut app, "tinted", x, y);
        assert!(near(got, want), "{what} on {backend:?}: {got:?}");
    }
    // The child paints through its parent's mask.
    for (x, y, want) in [(10.0, 20.0, GREEN), (10.0, 4.0, BLACK), (30.0, 20.0, BLACK)] {
        let got = pixel(&mut app, "parent", x, y);
        assert!(
            near(got, want),
            "child at ({x}, {y}) on {backend:?}: {got:?}"
        );
    }

    // A restyle that drops the rule shows the whole box again.
    set_root_classes(&mut app, &[]);
    assert_eq!(pixel(&mut app, "tinted", 30.0, 4.0), RED, "{backend:?}");
    assert_eq!(pixel(&mut app, "parent", 30.0, 4.0), RED, "{backend:?}");

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An app whose rules never put a mask on an element extracts no mask group
/// and paints as before.
fn without_masks_nothing_is_extracted() {
    let dir = std::env::temp_dir().join(format!("lumen-css-mask-none-{}", std::process::id()));
    let mut app = boot(&dir, Backend::Cpu, MARKUP, CSS);
    settle(&mut app);
    let groups = app
        .render_world
        .query::<&ExtractedMaskGroup>()
        .iter(&app.render_world)
        .count();
    assert_eq!(groups, 0);
    assert_eq!(pixel(&mut app, "tinted", 30.0, 4.0), RED);
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A mask image scaled up under `image-rendering: pixelated`, here inherited
/// from the row, samples the nearest pixel as an `<image>` does: the 2x1 mask
/// stretched over a 40x40 box keeps a hard edge down the middle rather than a
/// blended ramp.
#[test]
fn a_pixelated_mask_keeps_its_hard_edges() {
    const CSS: &str = r#"
root { padding: 0; align: start; }
#bar { gap: 10px; align: start; image-rendering: pixelated; }
#tinted, #parent { width: 40px; height: 40px; bg: #ff0000; }
#kid { width: 20px; height: 40px; bg: #00ff00; }
#tinted {
  mask-image: url("art/mask.png");
  mask-size: 40px 40px;
  mask-repeat: no-repeat;
}
"#;
    for backend in [Backend::Cpu, Backend::Gpu] {
        if let (Backend::Gpu, Some(why)) = (backend, gpu_unavailable_reason()) {
            eprintln!("skipping the GPU half: {why}");
            continue;
        }
        let dir = std::env::temp_dir().join(format!(
            "lumen-css-mask-pixelated-{backend:?}-{}",
            std::process::id()
        ));
        let mut app = boot(&dir, backend, MARKUP, CSS);
        settle(&mut app);
        for (x, want) in [(18.5, RED), (19.5, RED), (20.5, BLACK), (21.5, BLACK)] {
            let got = pixel(&mut app, "tinted", x, 20.0);
            assert_eq!(got, want, "x = {x} on {backend:?}");
        }
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
