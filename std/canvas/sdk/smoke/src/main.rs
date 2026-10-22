//! Exercises what a Rust app needs from `lumenui-canvas` alone: a canvas
//! element, a pixel buffer written a region at a time, the buffer drawn
//! zoomed with smoothing off, and the frame rendered headless and read back.
//! Exits non-zero when any of it does not hold.
//!
//! `tools/release/lumenui-engine.py wrappers` gives it a manifest inside the
//! generated `lumenui-canvas` crate, and CI builds and runs it there against
//! the engine set.

use lumenui::prelude::*;
use lumenui_canvas::{CanvasPlugin, Context2d, PixelBuffer};

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
}

/// How many pixels of the frame are exactly `rgb`.
fn count(frame: &Frame, rgb: [u8; 3]) -> usize {
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.pixel(x, y).is_some_and(|p| p[..3] == rgb))
        .count()
}

fn main() {
    let dir = std::env::current_dir().expect("a working directory");
    let side = SIDE * ZOOM;
    let art = PixelBuffer::new(SIDE, SIDE).expect("a buffer");
    art.fill_rect(0, 0, SIDE, SIDE, 0x0000ffff)
        .expect("the buffer exists");
    let frame = App::from_markup(format!(
        r#"<root><canvas id="art" width="{side}" height="{side}"/></root>"#
    ))
    .app_dir(dir)
    .window_size(side + 32, side + 32)
    .add_plugin(CanvasPlugin::default())
    .insert_resource(Art(art))
    .insert_resource(Ticks::default())
    .add_systems(TickStage::Systems, paint)
    .screenshot(HeadlessOptions {
        dpr: 1.0,
        ticks: Some(8),
        ..HeadlessOptions::default()
    });
    let frame = match frame {
        Ok(frame) => frame,
        Err(e) => {
            eprintln!("lumenui-canvas-smoke: the app did not run: {e}");
            std::process::exit(1);
        }
    };

    // Zoomed without smoothing, each written pixel is an exact ZOOM x ZOOM
    // block; smoothing, or a write that never reached the renderer, leaves
    // none of either colour exact.
    let block = (ZOOM * ZOOM) as usize;
    let red = count(&frame, [255, 0, 0]);
    let green = count(&frame, [0, 255, 0]);
    let blue = count(&frame, [0, 0, 255]);
    println!(
        "lumenui-canvas-smoke: {}x{} frame, {red} red, {green} green, {blue} blue pixels",
        frame.width(),
        frame.height()
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
