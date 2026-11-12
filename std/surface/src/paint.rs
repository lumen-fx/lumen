//! Getting a surface's frame onto the screen.
//!
//! The extract hands the render world a leaf per surface: where the element
//! sits, how the frame fits it, and the texture id its frames are presented
//! under. The painter draws that id through the render backend's texture
//! seam, which shows the frame beneath the window's frame in place. A frame
//! arriving changes the texture, not the leaf, so the window's other content
//! is not painted again for it.

use crate::api::SurfaceElement;
use crate::session::{Placement, Shared};
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

/// The device pixels per logical pixel of the window `entity` shows in; 1
/// with no window.
fn scale_of(world: &World, entity: Entity) -> f32 {
    world
        .get_resource::<lumen_module::lumen_core::window::WindowPartitions>()
        .and_then(|p| p.window_of(entity))
        .and_then(|w| world.get::<lumen_module::lumen_core::window::WindowViewport>(w))
        .map_or(1.0, |v| v.scale_factor)
}

/// Where a window position lands in the frame, in texture pixels: undoes
/// where the element was drawn, how the frame fits it, and the device pixel
/// ratio. `None` before the first frame or while the element is not drawn.
/// Outside the frame's drawn rect the answer is outside `0..size` (negative,
/// or past the frame's width or height), which the caller checks.
pub(crate) fn to_texture(shared: &Shared, position: glam::Vec2) -> Option<glam::Vec2> {
    map_point(shared.frame().map(|f| f.size), shared.placement(), position)
}

/// [`to_texture`] over a frame size and a placement.
fn map_point(
    frame: Option<(u32, u32)>,
    placement: Option<Placement>,
    position: glam::Vec2,
) -> Option<glam::Vec2> {
    let frame = frame?;
    let placement = placement?;
    let [fx, fy, fw, fh] = frame_rect_of(frame, &placement);
    if fw <= 0.0 || fh <= 0.0 {
        return None;
    }
    Some(glam::Vec2::new(
        (position.x - fx) * frame.0 as f32 / fw,
        (position.y - fy) * frame.1 as f32 / fh,
    ))
}

/// The ratio from logical motion to texture pixels; the device pixel ratio
/// before the first frame.
pub(crate) fn motion_scale(shared: &Shared) -> glam::Vec2 {
    let (Some(frame), Some(placement)) = (shared.frame().map(|f| f.size), shared.placement())
    else {
        return glam::Vec2::splat(shared.placement().map_or(1.0, |p| p.scale));
    };
    let [_, _, fw, fh] = frame_rect_of(frame, &placement);
    glam::Vec2::new(
        frame.0 as f32 / fw.max(0.001),
        frame.1 as f32 / fh.max(0.001),
    )
}

/// [`frame_rect`] for a recorded placement.
fn frame_rect_of(frame: (u32, u32), p: &Placement) -> [f32; 4] {
    let [x, y, w, h] = p.rect;
    frame_rect(
        frame,
        p.scale,
        glam::Vec2::new(x, y),
        glam::Vec2::new(w, h),
        p.fit,
    )
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
        let fit = fit.copied().unwrap_or(ImageFit::Contain);
        // `place` has already taken the scroll offsets above the element
        // off its box, so this is where it shows.
        element.shared.set_placement(Some(Placement {
            rect: [b.origin.x, b.origin.y, b.size.x, b.size.y],
            fit,
            scale: scale_of(main, entity),
        }));
        let frame_size = element.shared.frame().map(|f| f.size);
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
    fn mapping_undoes_the_box_the_fit_and_the_pixel_ratio() {
        // A 200 x 100 box at (100, 50) showing a 400 x 400 frame.
        let at = |fit, scale| {
            Some(Placement {
                rect: [100.0, 50.0, 200.0, 100.0],
                fit,
                scale,
            })
        };
        let map =
            |fit, scale, x, y| map_point(Some((400, 400)), at(fit, scale), glam::Vec2::new(x, y));
        // Contain: the frame shows 100 x 100, centred at (150, 50).
        for scale in [1.0, 2.0] {
            assert_eq!(
                map(ImageFit::Contain, scale, 150.0, 50.0),
                Some(glam::Vec2::ZERO)
            );
            assert_eq!(
                map(ImageFit::Contain, scale, 160.0, 60.0),
                Some(glam::Vec2::new(40.0, 40.0))
            );
        }
        // Cover: the frame shows 200 x 200 from (100, 0), cropped to the box.
        for scale in [1.0, 2.0] {
            assert_eq!(
                map(ImageFit::Cover, scale, 110.0, 60.0),
                Some(glam::Vec2::new(20.0, 120.0))
            );
        }
        // Unscaled: the device pixel ratio sets the frame's drawn size, 400
        // logical pixels at a ratio of 1 and 200 at 2, from the box's corner.
        assert_eq!(
            map(ImageFit::None, 1.0, 110.0, 60.0),
            Some(glam::Vec2::new(10.0, 10.0))
        );
        assert_eq!(
            map(ImageFit::None, 2.0, 110.0, 60.0),
            Some(glam::Vec2::new(20.0, 20.0))
        );
        // No frame yet, or not drawn: nothing to map into.
        assert_eq!(
            map_point(None, at(ImageFit::Contain, 1.0), glam::Vec2::ZERO),
            None
        );
        assert_eq!(map_point(Some((400, 400)), None, glam::Vec2::ZERO), None);
    }

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
