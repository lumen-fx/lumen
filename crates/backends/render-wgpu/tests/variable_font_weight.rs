//! Offscreen wgpu+vello capture proving a variable font paints at the
//! instance it was shaped at.
//!
//! cosmic-text pins a variable face's `wght` axis to the authored
//! `font-weight` and reports that instance's advances, so the outlines have
//! to come from the same instance. When they do not, the spacing widens
//! with the weight while every stroke keeps the face's default thickness -
//! the same amount of ink at every weight.
//!
//! Skips itself when the machine has no GPU, and when none of the CSS
//! generic families resolve to a variable face, which leaves no instance to
//! pick between.

use lumen_core::prelude::*;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumen_text::{ShapeOptions, TextShaper};
use lumen_text_cosmic::CosmicShaper;
use std::sync::Arc;

const W: u32 = 400;
const H: u32 = 80;
const TEXT: &str = "Hamburgefonstiv";
const SIZE_PX: f32 = 40.0;
const LIGHT: u16 = 100;
const HEAVY: u16 = 900;
const GENERICS: [&str; 5] = ["sans-serif", "serif", "monospace", "cursive", "fantasy"];

/// White text on black in `family` at `weight`, read back from the
/// offscreen target.
fn render_at(family: &Arc<str>, weight: u16) -> Vec<u8> {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H).with_text_shaper(CosmicShaper::new()));
    {
        let mut vp = app.render_world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::new(8.0, 8.0),
            size: glam::Vec2::new((W - 16) as f32, (H - 16) as f32),
            baseline_y: None,
        },
        TextContent(TEXT.to_string()),
        TextStyle {
            color: Color::rgb(1.0, 1.0, 1.0),
            size_px: SIZE_PX,
            family: Some(family.clone()),
            weight,
            ..Default::default()
        },
    ));
    app.tick();
    let renderer = app.render_world.get_non_send::<WgpuRenderer>().unwrap();
    renderer.read_rgba8().expect("readback")
}

/// Pixels the glyphs cover, which grows with stroke thickness.
fn ink(pixels: &[u8]) -> usize {
    pixels.chunks_exact(4).filter(|px| px[0] > 128).count()
}

/// The instance the shaper picks for `family` at `weight`.
fn instance_at(shaper: &mut CosmicShaper, family: &Arc<str>, weight: u16) -> Vec<i16> {
    let opts = ShapeOptions {
        family: Some(family.clone()),
        weight,
        ..ShapeOptions::default()
    };
    shaper
        .shape(TEXT, SIZE_PX, opts)
        .map(|run| run.segments[0].normalized_coords.clone())
        .unwrap_or_default()
}

/// The first generic family whose two ends shape as different instances.
fn variable_family(shaper: &mut CosmicShaper) -> Option<Arc<str>> {
    GENERICS
        .iter()
        .map(|n| Arc::from(*n))
        .find(|family| instance_at(shaper, family, LIGHT) != instance_at(shaper, family, HEAVY))
}

#[test]
fn a_variable_family_paints_the_weight_it_shaped_at() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut shaper = CosmicShaper::new();
    let Some(family) = variable_family(&mut shaper) else {
        eprintln!("no generic family resolves to a variable face; no instance to pick");
        return;
    };
    let light = ink(&render_at(&family, LIGHT));
    let heavy = ink(&render_at(&family, HEAVY));
    assert!(
        heavy > light * 3 / 2,
        "{family} at weight {HEAVY} must lay down visibly more ink than at \
         weight {LIGHT}: light={light} heavy={heavy}"
    );
}
