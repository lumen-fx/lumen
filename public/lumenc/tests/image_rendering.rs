// Boots a real app through `build_headless_app` / `RunOptions`, which lumenc
// only exposes under the `dev-run` feature.
#![cfg(feature = "dev-run")]

//! Pixels of CSS `image-rendering`.
//!
//! A two-pixel image (red, then blue) scaled up twentyfold: smooth by
//! default, so the columns either side of the seam blend; hard-edged under
//! `pixelated`, on an `<image>` and on a `bg: url(...)` that inherits the
//! value from its parent. A restyle that drops the rule brings the blend
//! back. Runs on the CPU renderer everywhere and on the GPU renderer where
//! the machine has one.

use glam::Vec2;
use lumen_core::components::{LumenClasses, Transform};
use lumen_core::prelude::{App, Color, ColorScheme, LumenId, StyleManager};
use lumen_render_cpu::{CpuRenderer, CpuRendererPlugin};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumenc::{RunOptions, build_headless_app};

const W: u32 = 120;
const H: u32 = 90;

const MARKUP: &str = r#"<root>
  <image id="smooth" src="art/two.png"/>
  <image id="pixelated" src="art/two.png"/>
  <row id="parent"><tile id="bg"/></row>
</root>"#;

const CSS: &str = r#"
root { padding: 0; gap: 10px; align: start; }
#smooth, #pixelated { width: 40px; height: 20px; }
:root.sharp #pixelated { image-rendering: pixelated; }
:root.sharp #parent { image-rendering: crisp-edges; }
#bg { width: 40px; height: 20px; bg: url("art/two.png"); bg-fit: fill; }
"#;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Cpu,
    Gpu,
}

fn boot(dir: &std::path::Path, backend: Backend) -> App {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("art")).expect("create temp app dir");
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    let mut two = image::RgbaImage::new(2, 1);
    two.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    two.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
    two.save(dir.join("art/two.png")).expect("write png");

    let mut opts = RunOptions::new(dir).with_markup(MARKUP).with_css(CSS);
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
    set_root_classes(&mut app, &["sharp"]);
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

/// The two columns either side of the seam in the middle of `id`'s box.
fn seam(app: &mut App, id: &str) -> ([u8; 3], [u8; 3]) {
    let at = origin_of(app, id) + Vec2::new(19.0, 10.0);
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
    let px = |x: usize, y: usize| {
        let i = (y * W as usize + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    let (x, y) = (at.x as usize, at.y as usize);
    (px(x, y), px(x + 1, y))
}

/// A pixel that took colour from both halves of the image.
fn blended(px: [u8; 3]) -> bool {
    px[0] > 20 && px[2] > 20
}

#[test]
fn image_rendering_picks_how_a_scaled_image_is_sampled() {
    check(Backend::Cpu);
    match gpu_unavailable_reason() {
        None => check(Backend::Gpu),
        Some(why) => eprintln!("skipping the GPU half: {why}"),
    }
}

fn check(backend: Backend) {
    let dir = std::env::temp_dir().join(format!(
        "lumen-image-rendering-{backend:?}-{}",
        std::process::id()
    ));
    let mut app = boot(&dir, backend);

    let (left, right) = seam(&mut app, "smooth");
    assert!(
        blended(left) && blended(right),
        "auto smooths across the seam on {backend:?}: {left:?} {right:?}"
    );
    for id in ["pixelated", "bg"] {
        let (left, right) = seam(&mut app, id);
        assert_eq!(
            (left, right),
            ([255, 0, 0], [0, 0, 255]),
            "`{id}` samples the nearest pixel on {backend:?}"
        );
    }

    // A restyle that drops the rule takes the element back to smooth.
    set_root_classes(&mut app, &[]);
    for id in ["pixelated", "bg"] {
        let (left, right) = seam(&mut app, id);
        assert!(
            blended(left) && blended(right),
            "`{id}` smooths again once the rule is gone on {backend:?}: {left:?} {right:?}"
        );
    }

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
