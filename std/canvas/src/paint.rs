//! Getting a canvas onto the screen.
//!
//! Two halves of the engine's native-paint seam, and nothing else. The
//! extract runs on the main world and hands the render world a leaf per
//! canvas: where the element ended up after layout, how opaque it is, and the
//! encoded scene. The painter runs on the render world and appends that scene
//! into the frame the renderer is building.
//!
//! The engine knows none of this is a canvas. It knows there is a leaf under
//! an extension id and a painter registered for that id, which is the whole
//! contract.

use lumen_module::lumen_core::components::{Opacity, Transform};
use lumen_module::lumen_core::native::{
    ExtractedNative, NativeExtract, NativePaintCtx, NativePainter, upsert_native_leaves,
};
use lumen_module::lumen_core::prelude::*;
use lumen_module::lumen_render_wgpu::vello::Scene;
use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::Affine;

use crate::plugin::Canvas;

/// Names this module's leaves and its painter. Scoped to the crate, so two
/// extensions cannot collide over an entity.
pub const EXTENSION_ID: &str = "lumen.canvas";

/// One canvas, as the render world sees it.
pub struct CanvasLeaf {
    /// The encoded drawing. Cloning is an `Arc` bump; the scene itself is
    /// shared with the store until the next encode writes into it.
    pub scene: std::sync::Arc<Scene>,
    /// The drawing space the scene was encoded in. The painter scales this
    /// onto the element's box, which is how CSS resizes a canvas without
    /// re-encoding it.
    pub logical: (f32, f32),
    /// The element's opacity, ancestors folded in.
    pub opacity: f32,
}

/// Publish one leaf per canvas element.
///
/// The scene comes off the component the module's own system keeps current,
/// not out of the store: an extract has no business taking a process-wide
/// lock in the middle of a frame.
pub fn extract_canvases(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut query = main.query::<(Entity, &Transform, &Canvas, Option<&Opacity>)>();
    let leaves: Vec<(Entity, ExtractedNative)> = query
        .iter(main)
        .filter_map(|(entity, transform, canvas, opacity)| {
            let placed = place.place(entity, transform, opacity)?;
            Some((
                entity,
                ExtractedNative {
                    extension_id: EXTENSION_ID.into(),
                    payload: std::sync::Arc::new(CanvasLeaf {
                        scene: canvas.scene.clone(),
                        logical: canvas.logical,
                        opacity: placed.opacity,
                    }),
                    bounds: placed.bounds,
                    order: placed.order,
                    revision: leaf_revision(canvas.revision, placed.opacity),
                    // A canvas draws in its own coordinate space and has no
                    // idea how big its box is, so anything it drew past the
                    // edge is clipped rather than spilling onto a sibling.
                    clip_to_bounds: true,
                },
            ))
        })
        .collect();
    upsert_native_leaves(render, EXTENSION_ID, leaves);
}

/// The stamp the seam compares leaves by: equal revision at equal geometry
/// has to mean equal pixels.
///
/// A canvas's own revision moves when it is drawn on, and that is not the
/// only thing that changes its pixels: the opacity it inherits is folded into
/// the payload, and an ancestor fading in changes nothing else about the
/// leaf. Left out, a fade over a canvas that is not being drawn on compares
/// equal, contributes no damage, and keeps the alpha it had when it last
/// drew. The two are packed rather than hashed so different pixels can never
/// land on the same stamp; a canvas would have to be drawn on 2^48 times for
/// the counter to reach the opacity's bits.
fn leaf_revision(revision: u64, opacity: f32) -> u64 {
    let quantized = u64::from((opacity.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16);
    (revision << 16) | quantized
}

/// Appends a canvas's scene into the frame.
pub struct CanvasPainter;

impl NativePainter for CanvasPainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(leaf) = ctx.payload_as::<CanvasLeaf>() else {
            return;
        };
        let bounds = ctx.bounds;
        let device = Affine::new(ctx.device_transform().coeffs);
        // The drawing space onto the box: a canvas declared 300x150 inside a
        // 600x300 element draws at twice the size, which is what the HTML
        // canvas does and what lets CSS resize one for free.
        let scale_x = if leaf.logical.0 > 0.0 {
            f64::from(bounds.size.x / leaf.logical.0)
        } else {
            1.0
        };
        let scale_y = if leaf.logical.1 > 0.0 {
            f64::from(bounds.size.y / leaf.logical.1)
        } else {
            1.0
        };
        let transform = device
            * Affine::translate((f64::from(bounds.origin.x), f64::from(bounds.origin.y)))
            * Affine::scale_non_uniform(scale_x, scale_y);
        let opacity = leaf.opacity.clamp(0.0, 1.0);
        let scene = leaf.scene.clone();

        let Some(target) = ctx.target_as::<Scene>() else {
            // The downcast fails when this module and the renderer hold
            // different builds of vello, which the SDK's `paint` re-export
            // exists to prevent. Say so once rather than drawing nothing in
            // silence.
            report_backend_mismatch(ctx.backend_id);
            return;
        };
        if opacity < 1.0 {
            target.push_layer(
                lumen_module::lumen_render_wgpu::vello::peniko::Fill::NonZero,
                lumen_module::lumen_render_wgpu::vello::peniko::BlendMode::default(),
                opacity,
                device,
                &lumen_module::lumen_render_wgpu::vello::peniko::kurbo::Rect::new(
                    f64::from(bounds.origin.x),
                    f64::from(bounds.origin.y),
                    f64::from(bounds.origin.x + bounds.size.x),
                    f64::from(bounds.origin.y + bounds.size.y),
                ),
            );
        }
        target.append(&scene, Some(transform));
        if opacity < 1.0 {
            target.pop_layer();
        }
    }
}

/// Report a target this painter cannot draw into, once for the run.
fn report_backend_mismatch(backend_id: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        lumen_module::lumen_core::warn_line!(
            "lumen-canvas: the '{backend_id}' renderer handed this module a draw target it \
             does not recognize, so nothing is drawn. The module and the engine were built \
             against different versions of the renderer."
        );
    }
}
