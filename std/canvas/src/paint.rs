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

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_module::lumen_core::components::Visible;
    use lumen_module::lumen_core::node_ir::Affine2;
    use lumen_module::lumen_core::render_world::Rect;
    use lumen_module::lumen_render_wgpu::vello::peniko::Fill;
    use lumen_module::lumen_render_wgpu::vello::peniko::kurbo::Rect as KurboRect;

    /// Encoding a scene is CPU work; only presenting it needs a device. These
    /// cases drive the painter over a real vello scene with no adapter at
    /// all, which is what lets them run on a machine (and a CI runner) that
    /// has no GPU.
    fn one_filled_square() -> std::sync::Arc<Scene> {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            lumen_module::lumen_render_wgpu::vello::peniko::Color::new([0.0, 1.0, 0.0, 1.0]),
            None,
            &KurboRect::new(0.0, 0.0, 10.0, 10.0),
        );
        std::sync::Arc::new(scene)
    }

    fn leaf(logical: (f32, f32), opacity: f32) -> CanvasLeaf {
        CanvasLeaf {
            scene: one_filled_square(),
            logical,
            opacity,
        }
    }

    fn bounds() -> Rect {
        Rect::new(glam::Vec2::new(8.0, 4.0), glam::Vec2::new(40.0, 20.0))
    }

    /// Run the painter over a fresh scene and hand back what it encoded.
    fn paint_into(leaf: &CanvasLeaf) -> Scene {
        let mut target = Scene::new();
        {
            let mut ctx = NativePaintCtx::new(
                leaf,
                &mut target,
                "lumen.render-wgpu",
                bounds(),
                Affine2::IDENTITY,
                1.0,
                1.0,
            );
            CanvasPainter.paint(&mut ctx);
        }
        target
    }

    #[test]
    fn the_stamp_moves_with_the_drawing_and_with_the_opacity() {
        assert_eq!(leaf_revision(3, 1.0), leaf_revision(3, 1.0));
        assert_ne!(
            leaf_revision(3, 1.0),
            leaf_revision(4, 1.0),
            "a canvas that drew has to repaint"
        );
        assert_ne!(
            leaf_revision(3, 1.0),
            leaf_revision(3, 0.5),
            "and so does one that only faded"
        );
        // Out-of-range opacity is clamped rather than wrapping into the
        // counter's bits.
        assert_eq!(leaf_revision(3, 1.5), leaf_revision(3, 1.0));
        assert_eq!(leaf_revision(3, -1.0), leaf_revision(3, 0.0));
    }

    #[test]
    fn the_painter_appends_the_canvas_scene() {
        let painted = paint_into(&leaf((10.0, 10.0), 1.0));
        assert_eq!(
            painted.encoding().n_paths,
            1,
            "the canvas's one path reached the frame"
        );
        assert_eq!(
            painted.encoding().n_clips,
            0,
            "a fully opaque canvas needs no layer"
        );
    }

    #[test]
    fn a_partly_transparent_canvas_is_painted_through_a_layer() {
        let painted = paint_into(&leaf((10.0, 10.0), 0.5));
        assert!(
            painted.encoding().n_clips > 0,
            "the opacity is applied to the canvas as a whole"
        );
        assert!(
            painted.encoding().n_open_clips == 0,
            "and the layer is closed again"
        );
    }

    #[test]
    fn a_canvas_with_no_drawing_space_is_placed_rather_than_divided_by_zero() {
        // `resize(id, 0, 0)` is a legal call, and the scale it implies is
        // not. The painter falls back to placing the drawing untouched.
        let painted = paint_into(&leaf((0.0, 0.0), 1.0));
        assert_eq!(painted.encoding().n_paths, 1);
    }

    #[test]
    fn a_target_this_painter_does_not_know_is_left_alone() {
        // What a renderer/module vello mismatch looks like from in here: the
        // downcast misses, and the painter reports rather than drawing into
        // something it cannot understand.
        let leaf = leaf((10.0, 10.0), 1.0);
        let mut foreign = 0u32;
        let mut ctx = NativePaintCtx::new(
            &leaf,
            &mut foreign,
            "test.other-backend",
            bounds(),
            Affine2::IDENTITY,
            1.0,
            1.0,
        );
        CanvasPainter.paint(&mut ctx);
        assert_eq!(foreign, 0);
    }

    #[test]
    fn a_payload_from_another_extension_is_not_ours_to_paint() {
        // Two extensions can put leaves in one frame. A painter that read
        // whatever it was handed would paint another module's state.
        let mut target = Scene::new();
        let not_a_canvas = 7u64;
        {
            let mut ctx = NativePaintCtx::new(
                &not_a_canvas,
                &mut target,
                "lumen.render-wgpu",
                bounds(),
                Affine2::IDENTITY,
                1.0,
                1.0,
            );
            CanvasPainter.paint(&mut ctx);
        }
        assert!(target.encoding().is_empty());
    }

    #[test]
    fn extract_publishes_one_leaf_per_canvas_and_retires_it() {
        // The extract half, on a bare app: no renderer, no adapter, no
        // window. What it produces is what a backend would be handed.
        let mut app = lumen_module::lumen_core::app::App::new();
        app.add_extract_fn(extract_canvases);

        let entity = app
            .world
            .spawn((
                Canvas {
                    id: "chart".to_string(),
                    logical: (32.0, 32.0),
                    scene: one_filled_square(),
                    revision: 4,
                },
                Transform {
                    absolute: glam::Vec2::new(8.0, 4.0),
                    size: glam::Vec2::new(40.0, 20.0),
                    baseline_y: None,
                },
                Visible(true),
            ))
            .id();
        app.tick();

        let mut leaves = app.render_world.query::<&ExtractedNative>();
        let extracted: Vec<&ExtractedNative> = leaves.iter(&app.render_world).collect();
        assert_eq!(extracted.len(), 1);
        assert_eq!(&*extracted[0].extension_id, EXTENSION_ID);
        assert_eq!(extracted[0].bounds.size.x, 40.0);
        assert_eq!(extracted[0].revision, leaf_revision(4, 1.0));
        assert!(extracted[0].clip_to_bounds, "a canvas cannot spill");
        let payload = extracted[0]
            .payload
            .downcast_ref::<CanvasLeaf>()
            .expect("the payload is a canvas leaf");
        assert_eq!(payload.logical, (32.0, 32.0));

        // The element goes, and so does its leaf.
        app.world.entity_mut(entity).despawn();
        app.tick();
        let mut leaves = app.render_world.query::<&ExtractedNative>();
        assert_eq!(leaves.iter(&app.render_world).count(), 0);
    }

    #[test]
    fn a_hidden_canvas_is_not_extracted_at_all() {
        let mut app = lumen_module::lumen_core::app::App::new();
        app.add_extract_fn(extract_canvases);
        app.world.spawn((
            Canvas {
                id: "chart".to_string(),
                logical: (32.0, 32.0),
                scene: one_filled_square(),
                revision: 1,
            },
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(10.0, 10.0),
                baseline_y: None,
            },
            Visible(false),
        ));
        app.tick();

        let mut leaves = app.render_world.query::<&ExtractedNative>();
        assert_eq!(
            leaves.iter(&app.render_world).count(),
            0,
            "a hidden canvas costs the renderer nothing"
        );
    }

    #[test]
    fn an_ancestor_opacity_is_folded_into_the_leaf() {
        let mut app = lumen_module::lumen_core::app::App::new();
        app.add_extract_fn(extract_canvases);
        app.world.spawn((
            Canvas {
                id: "chart".to_string(),
                logical: (32.0, 32.0),
                scene: one_filled_square(),
                revision: 2,
            },
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(10.0, 10.0),
                baseline_y: None,
            },
            Visible(true),
            Opacity(0.25),
        ));
        app.tick();

        let mut leaves = app.render_world.query::<&ExtractedNative>();
        let extracted: Vec<&ExtractedNative> = leaves.iter(&app.render_world).collect();
        let payload = extracted[0]
            .payload
            .downcast_ref::<CanvasLeaf>()
            .expect("leaf");
        assert!((payload.opacity - 0.25).abs() < 1e-6);
        assert_eq!(extracted[0].revision, leaf_revision(2, 0.25));
    }
}
