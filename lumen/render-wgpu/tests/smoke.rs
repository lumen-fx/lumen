//! WGPU + vello smoke test on the render-world model.
//!
//! Skips itself if no wgpu adapter is available.

use lumen_core::prelude::*;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin};

#[test]
fn render_one_rect_and_read_back() {
    // Probe wgpu first; skip if adapter init fails.
    if WgpuRenderer::new_offscreen(64, 64).is_err() {
        eprintln!("skipping: no wgpu adapter available");
        return;
    }

    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(64, 64));

    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(64.0, 64.0);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }

    // Spawn one main-world entity: a 48x48 red square at (8, 8).
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(8.0, 8.0),
            size: glam::Vec2::new(48.0, 48.0),
            baseline_y: None,
        },
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(1.0, 0.0, 0.0))),
            ..Default::default()
        },
    ));

    // Tick: extract → render-world render system runs.
    app.tick();

    let renderer = app
        .render_world
        .get_non_send_resource::<WgpuRenderer>()
        .unwrap();
    let pixels = renderer.read_rgba8().expect("readback");
    assert_eq!(pixels.len(), 64 * 64 * 4);

    let i = (32 * 64 + 32) * 4;
    let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
    assert!(r > 200, "center red channel low: {r}");
    assert!(g < 40, "center green channel high: {g}");
    assert!(b < 40, "center blue channel high: {b}");

    let (cr, cg, cb) = (pixels[0], pixels[1], pixels[2]);
    assert!(
        cr < 40 && cg < 40 && cb < 40,
        "corner not black: ({cr}, {cg}, {cb})"
    );
}
