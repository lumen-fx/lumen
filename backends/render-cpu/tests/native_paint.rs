//! A plugin's native painter on the CPU backend, end to end: the same
//! painter that draws on the GPU, through the same paint target.

use lumen_core::prelude::*;
use lumen_paint::kurbo::{Affine, Rect as KurboRect};
use lumen_paint::peniko::color::{AlphaColor, Srgb};
use lumen_paint::peniko::{BlendMode, Fill as PaintFill};
use lumen_paint::{PaintTarget, Shape};
use lumen_render_cpu::{BACKEND_ID, CpuRenderer, CpuRendererPlugin};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;
const EXTENSION: &str = "test.solid";

/// Fills its bounds green, then opens a layer and leaves it open: the shape
/// of a painter that returned early out of its own drawing.
struct GreenThenStray;

impl NativePainter for GreenThenStray {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        assert_eq!(ctx.backend_id, BACKEND_ID);
        let bounds = ctx.bounds;
        let transform = Affine::new(ctx.device_transform().coeffs);
        let Some(target) = ctx.target_as::<PaintTarget>() else {
            panic!("the CPU backend hands painters a PaintTarget");
        };
        target.fill(
            PaintFill::NonZero,
            transform,
            AlphaColor::<Srgb>::from_rgba8(0, 255, 0, 255).into(),
            None,
            &Shape::Rect(KurboRect::new(
                f64::from(bounds.origin.x),
                f64::from(bounds.origin.y),
                f64::from(bounds.origin.x + bounds.size.x),
                f64::from(bounds.origin.y + bounds.size.y),
            )),
        );
        target.push_layer(
            PaintFill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &Shape::Rect(KurboRect::new(0.0, 0.0, 1.0, 1.0)),
        );
    }
}

#[derive(Component)]
struct Solid;

fn extract_solids(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut q = main.query_filtered::<(Entity, &Transform), With<Solid>>();
    let leaves: Vec<(Entity, ExtractedNative)> = q
        .iter(main)
        .filter_map(|(e, transform)| {
            let placed = place.place(e, transform, None)?;
            Some((
                e,
                ExtractedNative {
                    extension_id: EXTENSION.into(),
                    payload: Arc::new(()),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: 1,
                    clip_to_bounds: false,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, EXTENSION, leaves);
}

/// The painter's pixels land, and the layer it left open is closed by the
/// walker before it can clip the rest of the frame: a box painted after the
/// leaf still shows.
#[test]
fn a_native_painter_draws_on_the_cpu_and_cannot_unbalance_the_frame() {
    let mut app = App::new();
    app.add_plugin(CpuRendererPlugin::new(W, H));
    app.add_extract_fn(extract_solids);
    app.register_native_painter(EXTENSION, GreenThenStray);
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(W as f32, H as f32);
        vp.clear = Color::rgb(0.0, 0.0, 0.0);
    }
    let at = |x: f32, y: f32, w: f32, h: f32| Transform {
        absolute: glam::Vec2::new(x, y),
        size: glam::Vec2::new(w, h),
        baseline_y: None,
    };
    app.world.spawn((at(8.0, 8.0, 16.0, 16.0), Solid));
    app.world.spawn((
        at(40.0, 40.0, 16.0, 16.0),
        Visuals {
            fill: Some(Fill::Solid(Color::rgb(0.0, 0.0, 1.0))),
            ..Default::default()
        },
    ));
    app.tick();

    let pixels = app.render_world.non_send::<CpuRenderer>().read_rgba8();
    let pixel = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (pixels[i], pixels[i + 1], pixels[i + 2])
    };
    assert_eq!(pixel(16, 16), (0, 255, 0), "the painter's fill");
    assert_eq!(pixel(48, 48), (0, 0, 255), "the box after the leaf");
    assert_eq!(pixel(2, 60), (0, 0, 0), "nothing else");
}
