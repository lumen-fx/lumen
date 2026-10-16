//! Exercises what a Rust app needs from `lumenui-canvas` alone: a canvas
//! element, a pixel buffer written a region at a time, the buffer drawn
//! zoomed with smoothing off, and the frame rendered headless and read back.
//! Exits non-zero when any of it does not hold.
//!
//! The SDK's `App` has no rendered headless run yet, so this drives the
//! runtime's own (`lumenui::runtime`), which is reachable through `lumenui`
//! like everything else.
//!
//! `tools/release/lumenui-engine.py wrappers` gives it a manifest inside the
//! generated `lumenui-canvas` crate, and CI builds and runs it there against
//! the engine set.

use std::sync::OnceLock;

use lumenui::lumen_core::render_world::SurfaceCapture;
use lumenui::prelude::*;
use lumenui::runtime::{HeadlessOptions, RunOptions, run_app_headless_rendered};
use lumenui_canvas::{CanvasPlugin, CanvasSet, Context2d, PixelBuffer};

/// The frame the renderer wrote, read after the run.
static CAPTURE: OnceLock<SurfaceCapture> = OnceLock::new();

/// Buffer pixels, and how many times each covers the screen.
const SIDE: u32 = 8;
const ZOOM: u32 = 8;

#[derive(Resource)]
struct Art(PixelBuffer);

#[derive(Resource, Default)]
struct Ticks(u32);

fn paint(art: Res<Art>, mut ticks: ResMut<Ticks>) {
    ticks.0 += 1;
    if ticks.0 == 2 {
        // One call writes a red and a green pixel side by side, after the
        // buffer has already been drawn once.
        art.0
            .write_rgba8(2, 2, 2, 1, &[255, 0, 0, 255, 0, 255, 0, 255])
            .expect("the buffer exists");
    }
    let ctx = Context2d::new("art");
    ctx.clear();
    ctx.set_image_smoothing(false);
    let side = f64::from(SIDE * ZOOM);
    ctx.draw_buffer_scaled(&art.0, 0.0, 0.0, side, side);
    if ticks.0 == 4
        && let Some(capture) = CAPTURE.get()
    {
        capture.request();
    }
}

/// How many pixels of the frame are exactly `rgb`.
fn count(rgba: &[u8], rgb: [u8; 3]) -> usize {
    rgba.chunks_exact(4).filter(|p| p[..3] == rgb).count()
}

fn main() {
    let dir = std::env::current_dir().expect("a working directory");
    let side = SIDE * ZOOM;
    let mut opts = RunOptions::new(dir)
        .with_parser(lumenui::lumenc::default_parser())
        .with_markup(format!(
            r#"<root><canvas id="art" width="{side}" height="{side}"/></root>"#
        ))
        .with_plugin(CanvasPlugin::default())
        .with_app_hook(|app| {
            let art = PixelBuffer::new(SIDE, SIDE).expect("a buffer");
            art.fill_rect(0, 0, SIDE, SIDE, 0x0000ffff)
                .expect("the buffer exists");
            app.world.insert_resource(Art(art));
            // The renderer answers a capture handle in its world; with no
            // introspection server running, nothing else puts one there.
            let capture = CAPTURE.get_or_init(SurfaceCapture::default);
            app.render_world.insert_resource(capture.clone());
            app.world.insert_resource(Ticks::default());
            app.add_systems(TickStage::Systems, paint.before(CanvasSet::Encode));
        });
    opts.size = (side + 32, side + 32);
    opts.hot_reload = false;
    let result = run_app_headless_rendered(
        opts,
        HeadlessOptions {
            dpr: 1.0,
            ticks: Some(8),
        },
    );
    if let Err(e) = result {
        eprintln!("lumenui-canvas-smoke: the app did not run: {e}");
        std::process::exit(1);
    }

    let Some(frame) = CAPTURE.get().and_then(SurfaceCapture::read) else {
        eprintln!("lumenui-canvas-smoke: the renderer answered no capture");
        std::process::exit(1);
    };
    // Zoomed without smoothing, each written pixel is an exact ZOOM x ZOOM
    // block; smoothing, or a write that never reached the renderer, leaves
    // none of either colour exact.
    let block = (ZOOM * ZOOM) as usize;
    let red = count(&frame.rgba8, [255, 0, 0]);
    let green = count(&frame.rgba8, [0, 255, 0]);
    let blue = count(&frame.rgba8, [0, 0, 255]);
    println!(
        "lumenui-canvas-smoke: {}x{} frame, {red} red, {green} green, {blue} blue pixels",
        frame.width, frame.height
    );
    if red != block || green != block || blue != block * (SIDE * SIDE - 2) as usize {
        eprintln!(
            "lumenui-canvas-smoke: expected {block} red, {block} green and {} blue",
            block * (SIDE * SIDE - 2) as usize
        );
        std::process::exit(1);
    }
    println!("lumenui-canvas-smoke: ok");
}
