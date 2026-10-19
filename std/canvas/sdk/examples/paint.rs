//! A small pixel painter: drag on the canvas to paint into a 64x64 buffer
//! shown eight times larger, with image smoothing off so every pixel stays a
//! sharp square. Each dab is one region write; only the tile it lands in is
//! uploaded again.
//!
//! It is an example of the generated `lumenui-canvas` crate. From a Lumen
//! checkout, write that crate outside the checkout and run it against an
//! engine set (see `tools/release/lumenui-engine.py`):
//!
//! ```bash
//! tools/release/lumenui-engine.py wrappers --out /tmp/lumenui-sdk
//! LUMEN_ENGINE_ARTIFACTS=<set> cargo run --manifest-path /tmp/lumenui-sdk/lumenui-canvas/Cargo.toml --example paint
//! ```

use lumenui::prelude::*;
use lumenui_canvas::{Canvas, CanvasPlugin, Context2d, PixelBuffer};

/// The picture, in pixels.
const SIDE: u32 = 64;
/// How large the canvas shows it.
const SHOWN: f64 = 512.0;
/// The brush colour, straight RGBA8.
const INK: [u8; 4] = [0x20, 0x40, 0xd0, 0xff];

#[derive(Resource)]
struct Art {
    pixels: PixelBuffer,
    painting: bool,
    changed: bool,
}

fn main() -> lumenui::Result<()> {
    let pixels = PixelBuffer::new(SIDE, SIDE).expect("a 64x64 buffer is inside the caps");
    for y in 0..SIDE / 8 {
        for x in 0..SIDE / 8 {
            let shade = if (x + y) % 2 == 0 {
                0xeeeeeeff
            } else {
                0xccccccff
            };
            pixels
                .fill_rect(i64::from(x * 8), i64::from(y * 8), 8, 8, shade)
                .expect("the buffer exists");
        }
    }
    // An app names its markup in `lumen.toml` and starts from
    // `lumenui::app!()`; an example has no build script to compile it, so
    // this one gives its one element as text.
    App::from_markup(r#"<root><canvas id="art" width="512" height="512"/></root>"#)
        .window_title("Lumen canvas paint")
        .add_plugin(CanvasPlugin::default())
        .insert_resource(Art {
            pixels,
            painting: false,
            changed: true,
        })
        // In the app's slot of the tick: after the pointer input, before the
        // canvas encodes what was drawn.
        .add_systems(TickStage::Systems, (brush, show).chain())
        .run()
}

/// Paint a 2x2 dab wherever the pointer is while the button is down.
fn brush(
    mut art: ResMut<Art>,
    mut pressed: MessageReader<PointerPressed>,
    mut moved: MessageReader<PointerMoved>,
    mut released: MessageReader<PointerReleased>,
    canvas: Query<&Transform, With<Canvas>>,
) {
    let Ok(place) = canvas.single() else {
        return;
    };
    let dab = |art: &mut Art, at: Vec2| {
        // Window coordinates to buffer pixels, through the box layout gave
        // the canvas.
        let pixel = (at - place.absolute) / place.size * SIDE as f32;
        if pixel.x < 0.0 || pixel.y < 0.0 || pixel.x >= SIDE as f32 || pixel.y >= SIDE as f32 {
            return;
        }
        let (x, y) = (pixel.x as i64, pixel.y as i64);
        art.pixels
            .write_rgba8(x, y, 2, 2, &INK.repeat(4))
            .expect("the buffer exists");
        art.changed = true;
    };
    for press in pressed.read() {
        art.painting = true;
        dab(&mut art, press.position);
    }
    for motion in moved.read() {
        if art.painting {
            dab(&mut art, motion.position);
        }
    }
    if released.read().count() > 0 {
        art.painting = false;
    }
}

/// Show the buffer again after it changed.
fn show(mut art: ResMut<Art>) {
    if !art.changed {
        return;
    }
    art.changed = false;
    let ctx = Context2d::new("art");
    ctx.clear();
    ctx.set_image_smoothing(false);
    ctx.draw_buffer_scaled(&art.pixels, 0.0, 0.0, SHOWN, SHOWN);
}
