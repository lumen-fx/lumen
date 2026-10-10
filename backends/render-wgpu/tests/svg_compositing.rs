//! SVG group opacity and masks on the GPU, end to end: a leaf paints an SVG
//! through the paint target, the renderer rasterizes the frame offscreen,
//! and the test reads back the pixels that prove a group fades as one
//! picture and a mask hides what it should.
//!
//! Skips itself when the machine has no GPU: either no wgpu adapter at all,
//! or only a software rasterizer.

use lumen_core::prelude::*;
use lumen_paint::PaintTarget;
use lumen_paint::kurbo::Affine;
use lumen_paint::svg::paint_svg;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;
const EXTENSION: &str = "test.svg";

/// The leaf: an SVG document the painter parses and paints at its bounds.
#[derive(Component)]
struct Drawing(Arc<String>);

struct SvgPainter;

impl NativePainter for SvgPainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(svg) = ctx.payload_as::<String>() else {
            return;
        };
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).expect("valid svg");
        let origin = ctx.bounds.origin;
        let transform = Affine::new(ctx.device_transform().coeffs)
            * Affine::translate((f64::from(origin.x), f64::from(origin.y)));
        let Some(target) = ctx.target_as::<PaintTarget>() else {
            return;
        };
        paint_svg(target.as_mut(), &tree, transform);
    }
}

fn extract_drawings(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query::<(Entity, &Transform, &Drawing)>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform, drawing)| {
            let placed = place.place(e, transform)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: EXTENSION.into(),
                    payload: drawing.0.clone(),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: next_revision(),
                    clip_to_bounds: false,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, EXTENSION, leaves);
}

/// Paint `body` (the inside of a 64 x 64 `<svg>`) over black and read the
/// red channel of each `(x, y)` in `at`.
fn red_at(body: &str, at: &[(u32, u32)]) -> Vec<u8> {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    app.add_extract_fn(extract_drawings);
    app.register_native_painter(EXTENSION, SvgPainter);
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    let svg =
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">{body}</svg>"##);
    app.world.spawn((
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::new(W as f32, H as f32),
            baseline_y: None,
        },
        Drawing(Arc::new(svg)),
    ));
    app.tick();
    let pixels = app
        .render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8()
        .expect("readback");
    at.iter()
        .map(|&(x, y)| pixels[((y * W + x) * 4) as usize])
        .collect()
}

fn near(got: u8, want: u8) -> bool {
    got.abs_diff(want) <= 3
}

/// Half opacity on a group halves what it paints, and where two of its
/// children overlap the result is still half: the group fades as one.
#[test]
fn a_faded_group_fades_as_one_picture() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let red = red_at(
        r##"<g opacity="0.5">
              <rect width="40" height="64" fill="#ff0000"/>
              <rect x="20" width="40" height="64" fill="#ff0000"/>
            </g>"##,
        &[(10, 32), (30, 32), (62, 32)],
    );
    assert!(near(red[0], 128), "one child: {red:?}");
    assert!(near(red[1], 128), "the overlap: {red:?}");
    assert_eq!(red[2], 0, "outside the group: {red:?}");
}

/// A luminance mask shows its content under white and hides it under
/// black.
#[test]
fn a_luminance_mask_shows_under_white_and_hides_under_black() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let red = red_at(
        r##"<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#ffffff"/>
              <rect x="32" width="32" height="64" fill="#000000"/>
            </mask>
            <g mask="url(#m)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "under white: {red:?}");
    assert_eq!(red[1], 0, "under black: {red:?}");
}

/// An alpha mask reads only coverage: opaque black shows the content,
/// where luminance would hide it, and bare mask hides it.
#[test]
fn an_alpha_mask_shows_wherever_the_mask_is_opaque() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let red = red_at(
        r##"<mask id="m" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#000000"/>
            </mask>
            <g mask="url(#m)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "under the opaque mask: {red:?}");
    assert_eq!(red[1], 0, "outside it: {red:?}");
}

/// A mask's own mask applies too: the outer mask shows everything, the
/// inner one only the left half.
#[test]
fn a_mask_on_a_mask_applies_both() {
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("skipping: {why}");
        return;
    }
    let red = red_at(
        r##"<mask id="inner" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="32" height="64" fill="#000000"/>
            </mask>
            <mask id="outer" mask="url(#inner)" maskUnits="userSpaceOnUse" x="0" y="0" width="64" height="64">
              <rect width="64" height="64" fill="#ffffff"/>
            </mask>
            <g mask="url(#outer)"><rect width="64" height="64" fill="#ff0000"/></g>"##,
        &[(16, 32), (48, 32)],
    );
    assert!(near(red[0], 255), "inside both masks: {red:?}");
    assert_eq!(red[1], 0, "outside the inner mask: {red:?}");
}
