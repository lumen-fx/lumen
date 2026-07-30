//! Offscreen wgpu+vello capture proving the text-input selection
//! highlight and caret actually RENDER (author feedback J: "text
//! highlighting doesn't work" / "hard to see where the cursor is").
//!
//! Mirrors Qt `QLineEdit` / Slint `TextInput`: a selection background
//! rect (`QPalette::Highlight` / `selection-background-color`) paints
//! behind the selected glyphs and a solid caret bar marks the cursor.
//! We drive the full headless pipeline (extract → node-IR → vello walker)
//! with distinctive token colors so the two visuals are unambiguous in
//! the readback:
//!   - selection background → pure green
//!   - caret                → pure red
//!
//! Skips itself when no wgpu adapter is available (bare CI container).

use lumen_core::components::TextInputPaint;
use lumen_core::prelude::*;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin};
use lumen_text_cosmic::CosmicShaper;

const W: u32 = 220;
const H: u32 = 60;

fn render_selected_input() -> Option<Vec<u8>> {
    if WgpuRenderer::new_offscreen(W, H).is_err() {
        eprintln!("skipping: no wgpu adapter available");
        return None;
    }
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H).with_text_shaper(CosmicShaper::new()));
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }

    // A focused single-line input with "Hello World"; select "Hello"
    // (bytes 0..5) with the caret at the trailing edge (byte 5).
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(12.0, 12.0),
            size: glam::Vec2::new(196.0, 36.0),
            baseline_y: None,
        },
        TextContent("Hello World".to_string()),
        TextInput {
            cursor: 5,
            selection_anchor: Some(0),
            ..Default::default()
        },
        TextStyle {
            color: Color::rgb(1.0, 1.0, 1.0),
            size_px: 22.0,
            // Distinctive token colors (a skin would source these from
            // `--lumen-selection` / `caret-color`); the test asserts they
            // reach the framebuffer.
            selection_color: Some(Color::rgb(0.0, 1.0, 0.0)),
            ..Default::default()
        },
        TextInputPaint {
            caret_color: Some(Color::rgb(1.0, 0.0, 0.0)),
            ..Default::default()
        },
        Focused,
    ));

    app.tick();

    let renderer = app
        .render_world
        .get_non_send_resource::<WgpuRenderer>()
        .unwrap();
    Some(renderer.read_rgba8().expect("readback"))
}

/// Count pixels matching a dominant-channel predicate.
fn count(pixels: &[u8], pred: impl Fn(u8, u8, u8) -> bool) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|px| pred(px[0], px[1], px[2]))
        .count()
}

#[test]
fn selection_highlight_and_caret_are_visible() {
    let Some(pixels) = render_selected_input() else {
        return;
    };
    assert_eq!(pixels.len(), (W * H * 4) as usize);

    // The caret is font-independent (drawn even when shaping yields no
    // glyphs), so it must always be present: a solid red column.
    let red = count(&pixels, |r, g, b| r > 180 && g < 80 && b < 80);
    assert!(red > 0, "caret (red) did not render: {red} red pixels");

    // The selection highlight needs shaped geometry. If the sandbox has
    // no usable font the run shapes empty and there are no glyphs to
    // select — detect that (no white glyph pixels) and skip only the
    // selection assertion, keeping the font-independent caret check
    // authoritative.
    let white = count(&pixels, |r, g, b| r > 180 && g > 180 && b > 180);
    if white == 0 {
        eprintln!("no glyphs shaped (font-less env); skipping selection-band assertion");
        return;
    }
    let green = count(&pixels, |r, g, b| g > 180 && r < 80 && b < 80);
    assert!(
        green > 0,
        "selection highlight (green) did not render behind the glyphs: {green} green pixels"
    );
}
