//! Getting a surface's frame onto the screen.
//!
//! The extract hands the render world a leaf per surface: where the element
//! sits, how the frame fits it, and the texture id its frames are presented
//! under. The painter draws that id through the render backend's texture
//! seam, which shows the frame beneath the window's frame in place. A frame
//! arriving changes the texture, not the leaf, so the window's other content
//! is not painted again for it.

use crate::api::SurfaceElement;
use crate::session::Shared;
use lumen_module::lumen_core::components::{ImageFit, ImageRendering, Transform};
use lumen_module::lumen_core::native::{
    ExtractedNative, NativeExtract, NativePaintCtx, NativePainter, upsert_native_leaves,
};
use lumen_module::lumen_core::prelude::*;
use lumen_module::lumen_paint::kurbo::Affine;
use lumen_module::lumen_paint::peniko::{ImageQuality, ImageSampler};
use lumen_module::lumen_paint::{BackendImage, PaintTarget, TextureId, TextureRefusal};
use std::sync::Arc;

/// Names this module's leaves and its painter.
pub const EXTENSION_ID: &str = "lumen.surface";

/// One surface, as the render world sees it.
pub struct SurfaceLeaf {
    /// The id its frames are presented under.
    pub texture: TextureId,
    /// The size of the frame on screen, in texture pixels; `None` before the
    /// first frame.
    pub frame_size: Option<(u32, u32)>,
    /// How the frame fits the element.
    pub fit: ImageFit,
    /// Sample the nearest texel.
    pub nearest: bool,
    /// The element's own opacity, ancestors outside a fading group folded in.
    pub opacity: f32,
    /// The session, to report a refused draw to.
    pub shared: Arc<Shared>,
}

/// Where the frame lands in an element's box: its origin and drawn size, in
/// logical pixels, for a frame of `frame` texture pixels shown at `scale`
/// device pixels per logical pixel in a box of `size` at `origin`.
pub fn frame_rect(
    frame: (u32, u32),
    scale: f32,
    origin: glam::Vec2,
    size: glam::Vec2,
    fit: ImageFit,
) -> [f32; 4] {
    let scale = f64::from(scale.max(0.01));
    let ((dx, dy), (w, h)) = lumen_module::lumen_paint::fit_box(
        (f64::from(frame.0) / scale, f64::from(frame.1) / scale),
        (f64::from(size.x), f64::from(size.y)),
        fit,
    );
    [
        origin.x + dx as f32,
        origin.y + dy as f32,
        w as f32,
        h as f32,
    ]
}

/// Publish one leaf per surface element, and record where each was placed.
pub fn extract_surfaces(main: &mut World, render: &mut World) {
    let mut place = NativeExtract::new(main);
    let mut query = main.query::<(
        Entity,
        &Transform,
        &SurfaceElement,
        Option<&ImageFit>,
        Option<&ImageRendering>,
    )>();
    let mut leaves = Vec::new();
    for (entity, transform, element, fit, rendering) in query.iter(main) {
        let Some(placed) = place.place(entity, transform) else {
            element.shared.set_placement(None);
            continue;
        };
        let b = placed.bounds;
        element
            .shared
            .set_placement(Some([b.origin.x, b.origin.y, b.size.x, b.size.y]));
        let frame_size = element.shared.frame().map(|f| f.size);
        let fit = fit.copied().unwrap_or(ImageFit::Contain);
        let nearest = rendering.is_some_and(|r| r.is_nearest());
        let revision = revision(frame_size, fit, nearest, placed.opacity);
        leaves.push((
            entity,
            ExtractedNative {
                extension_id: EXTENSION_ID.into(),
                payload: Arc::new(SurfaceLeaf {
                    texture: element.texture,
                    frame_size,
                    fit,
                    nearest,
                    opacity: placed.opacity,
                    shared: element.shared.clone(),
                }),
                bounds: placed.bounds,
                order: placed.order,
                revision,
                // The frame is clipped to the element: a frame that covers
                // the box spills past it otherwise.
                clip_to_bounds: true,
            },
        ));
    }
    upsert_native_leaves(render, EXTENSION_ID, leaves);
}

/// The stamp that says the leaf paints the same: equal stamps at equal
/// geometry are equal pixels in the window's frame. A new frame of the same
/// size is not a change here; the texture carries it.
fn revision(frame: Option<(u32, u32)>, fit: ImageFit, nearest: bool, opacity: f32) -> u64 {
    let (w, h) = frame.map_or((0, 0), |(w, h)| {
        (u64::from(w) & 0xffff, u64::from(h) & 0xffff)
    });
    let fit = match fit {
        ImageFit::Fill => 0,
        ImageFit::Contain => 1,
        ImageFit::Cover => 2,
        ImageFit::None => 3,
        ImageFit::ScaleDown => 4,
    };
    let opacity = u64::from((opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
    (w << 48)
        | (h << 32)
        | (u64::from(frame.is_some()) << 31)
        | (fit << 28)
        | (u64::from(nearest) << 27)
        | (opacity << 16)
}

/// Draws a surface's frame.
pub struct SurfacePainter;

impl NativePainter for SurfacePainter {
    fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
        let Some(leaf) = ctx.payload_as::<SurfaceLeaf>() else {
            return;
        };
        let Some(frame) = leaf.frame_size else {
            // Before the first frame the element shows its own background.
            return;
        };
        let bounds = ctx.bounds;
        let device = Affine::new(ctx.device_transform().coeffs);
        let [x, y, w, h] = frame_rect(frame, ctx.dpr, bounds.origin, bounds.size, leaf.fit);
        let Some(target) = ctx.target_as::<PaintTarget>() else {
            return;
        };
        if leaf.opacity < 1.0 {
            leaf.shared.report_refusal(Some((
                "opacity",
                "a surface shows its frame opaque; its opacity, or an ancestor's, is below 1, \
                 so the frame is not drawn"
                    .into(),
            )));
            return;
        }
        let transform = device
            * Affine::translate((f64::from(x), f64::from(y)))
            * Affine::scale_non_uniform(
                f64::from(w) / f64::from(frame.0.max(1)),
                f64::from(h) / f64::from(frame.1.max(1)),
            );
        let quality = if leaf.nearest {
            ImageQuality::Low
        } else {
            ImageQuality::Medium
        };
        let drawn = target.draw_texture(
            &BackendImage {
                id: leaf.texture,
                width: frame.0,
                height: frame.1,
                fallback: None,
            },
            ImageSampler::default().with_quality(quality),
            transform,
        );
        // A renderer without textures (the CPU backend) leaves the surface
        // unsupported, which the element reports by its state; anything else
        // refused is an effect the frame cannot be shown under.
        leaf.shared.report_refusal(
            drawn
                .err()
                .filter(|r| *r != TextureRefusal::Unavailable)
                .map(|refusal| {
                    (
                        refusal.as_str(),
                        format!("the surface's frame is not drawn: {refusal}"),
                    )
                }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame fits the box the way `object-fit` says.
    #[test]
    fn frames_fit_the_box() {
        let origin = glam::Vec2::new(10.0, 20.0);
        let size = glam::Vec2::new(200.0, 100.0);
        assert_eq!(
            frame_rect((400, 400), 1.0, origin, size, ImageFit::Contain),
            [60.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(
            frame_rect((400, 400), 1.0, origin, size, ImageFit::Fill),
            [10.0, 20.0, 200.0, 100.0]
        );
        assert_eq!(
            frame_rect((100, 50), 2.0, origin, size, ImageFit::None),
            [10.0, 20.0, 50.0, 25.0],
            "a frame at its own size is its texture pixels at the scale"
        );
    }

    /// A new frame of the same size paints the same leaf; a new size, a new
    /// fit or a fade does not.
    #[test]
    fn the_stamp_moves_with_what_the_leaf_paints() {
        let r = revision(Some((640, 480)), ImageFit::Contain, false, 1.0);
        assert_eq!(r, revision(Some((640, 480)), ImageFit::Contain, false, 1.0));
        assert_ne!(r, revision(Some((641, 480)), ImageFit::Contain, false, 1.0));
        assert_ne!(r, revision(Some((640, 480)), ImageFit::Cover, false, 1.0));
        assert_ne!(r, revision(Some((640, 480)), ImageFit::Contain, true, 1.0));
        assert_ne!(r, revision(Some((640, 480)), ImageFit::Contain, false, 0.5));
        assert_ne!(r, revision(None, ImageFit::Contain, false, 1.0));
    }
}
