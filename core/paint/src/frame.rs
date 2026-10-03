//! The frame logic every renderer shares: whether a frame is worth painting,
//! the walk of the retained scene into a [`PaintTarget`], and the answer to a
//! pending screenshot request.
//!
//! A [`lumen_core::traits::Renderer`] calls these from its
//! `wants_present` and `present`, so every backend and every kind of target
//! gates, paints, and captures the same way. What stays the backend's own is
//! resetting its sink, rasterizing what was painted, and putting the result
//! on its target.

use crate::cache::FragmentCache;
use crate::walker::{WalkContext, scene_has_damage, walk_node};
use crate::{PaintTarget, peniko_color};
use bevy_ecs::world::World;
use lumen_core::native::NativePainters;
use lumen_core::node_ir::{PreviousScene, RetainedScene};
use lumen_core::render_world::{SurfaceCapture, SurfaceFrame, Viewport};
use lumen_core::traits::FrameRequest;
use lumen_text::{ShaperService, TextShaper};

/// Whether a renderer should paint this frame.
///
/// A pending screenshot request always paints, because it is answered from
/// the frame [`answer_capture`] reads after painting. So does a target that
/// holds no frame yet (`holds_frame == false`): one just created or resized
/// has no valid pixels. Otherwise the frame paints when the tick reported a
/// change and either the caller forces a full frame or the retained scene
/// differs from the last painted one ([`scene_has_damage`]). The dirty flag
/// over-approximates, raised by writes that leave the painted tree
/// identical; the diff is what keeps those from costing a frame.
pub fn wants_frame(render_world: &World, request: FrameRequest, holds_frame: bool) -> bool {
    capture_requested(render_world)
        || !holds_frame
        || (request.dirty && (request.force_full || scene_has_damage(render_world)))
}

/// Whether an off-thread screenshot request is waiting on this frame.
pub fn capture_requested(render_world: &World) -> bool {
    render_world
        .get_resource::<SurfaceCapture>()
        .is_some_and(SurfaceCapture::is_requested)
}

/// The colour a frame starts from: the viewport's clear colour.
pub fn clear_color(render_world: &World) -> peniko::Color {
    peniko_color(
        render_world
            .get_resource::<Viewport>()
            .map(|v| v.clear)
            .unwrap_or_default(),
    )
}

/// Walk the render world's retained scene into `painter`, then park the tree
/// as the [`PreviousScene`] the next frame's gate diffs against.
///
/// The caller resets its sink first. The walker scales every leaf from
/// logical to physical pixels by the viewport's scale factor, paints text
/// through the render world's [`ShaperService`] (skipping it when there is
/// none), dispatches native leaves to the registered [`NativePainters`], and
/// replays repeated appearances from `cache` when the sink records fragments.
pub fn paint_frame(
    render_world: &mut World,
    painter: &mut PaintTarget,
    cache: Option<&mut FragmentCache>,
) {
    // Carve a local Arc of the tree so the borrow on the world releases
    // before the shaper is taken mutably.
    let root = render_world
        .get_resource::<RetainedScene>()
        .and_then(|scene| scene.root.clone());
    let dpr = render_world
        .get_resource::<Viewport>()
        .map_or(1.0, |v| v.scale_factor)
        .max(0.01);
    // Cloning shares the table, so this costs one refcount.
    let natives = render_world.get_resource::<NativePainters>().cloned();
    if let Some(root) = root.as_ref() {
        let mut shaper = render_world.get_non_send_mut::<ShaperService>();
        let shaper: Option<&mut dyn TextShaper> = shaper
            .as_deref_mut()
            .map(|s| &mut **s as &mut dyn TextShaper);
        let mut ctx = WalkContext::new_with_dpr(painter, cache, shaper, dpr);
        if let Some(painters) = natives.as_ref() {
            ctx = ctx.with_native_painters(painters);
        }
        walk_node(&mut ctx, root);
    }
    if let Some(mut previous) = render_world.get_resource_mut::<PreviousScene>() {
        previous.root = root;
    }
}

/// Answer a pending screenshot request with the frame `read` returns, as
/// tightly packed straight-alpha RGBA8 of `width` x `height`. `read` runs
/// only when a request is pending. The request is cleared whether or not
/// the read succeeded, so a persistent failure cannot wedge the requester.
pub fn answer_capture(
    render_world: &World,
    (width, height): (u32, u32),
    read: impl FnOnce() -> Result<Vec<u8>, String>,
) {
    let Some(capture) = render_world.get_resource::<SurfaceCapture>() else {
        return;
    };
    if !capture.is_requested() {
        return;
    }
    match read() {
        Ok(rgba8) => capture.write(SurfaceFrame {
            width,
            height,
            rgba8,
        }),
        Err(e) => eprintln!("lumen: frame readback failed: {e}"),
    }
    capture.clear_request();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::Color;
    use lumen_core::node_ir::Node;
    use lumen_core::render_world::{Brush, Rect};
    use std::sync::Arc;

    fn render_world() -> World {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        world.insert_resource(RetainedScene::default());
        world.insert_resource(PreviousScene::default());
        world
    }

    fn one_rect() -> Arc<Node> {
        Arc::new(Node::Rect {
            bounds: Rect {
                origin: glam::Vec2::new(4.0, 4.0),
                size: glam::Vec2::new(16.0, 16.0),
            },
            brush: Brush::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            corner: 0.0,
            corners: None,
        })
    }

    fn request(dirty: bool, force_full: bool) -> FrameRequest {
        FrameRequest { dirty, force_full }
    }

    /// A clean tick paints nothing, a dirty tick whose tree is unchanged
    /// paints nothing, and a forced frame or a fresh target paints anyway.
    #[test]
    fn the_gate_follows_dirty_damage_and_the_target() {
        let world = render_world();
        assert!(!wants_frame(&world, request(false, false), true));
        assert!(!wants_frame(&world, request(true, false), true));
        assert!(wants_frame(&world, request(true, true), true));
        assert!(wants_frame(&world, request(false, false), false));
    }

    /// A changed tree paints; once painted and parked, the same tree does
    /// not.
    #[test]
    fn a_changed_tree_paints_once() {
        let mut world = render_world();
        world.resource_mut::<RetainedScene>().root = Some(one_rect());
        assert!(wants_frame(&world, request(true, false), true));

        let mut painter: PaintTarget = Box::<crate::recording::Recording>::default();
        paint_frame(&mut world, &mut painter, None);
        assert!(!wants_frame(&world, request(true, false), true));
    }

    /// A render world missing the scene resources belongs to a
    /// non-standard embed. The diff cannot say anything about it, so it
    /// claims a change rather than keep a frame that may be stale.
    #[test]
    fn a_world_without_scene_resources_always_paints() {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        assert!(wants_frame(&world, request(true, false), true));
        assert!(!capture_requested(&world));
    }

    /// A pending screenshot paints even a clean tick, is answered with what
    /// the read returns, and is cleared whether or not the read worked.
    #[test]
    fn a_capture_paints_and_is_answered_then_cleared() {
        let mut world = render_world();
        let capture = SurfaceCapture::default();
        world.insert_resource(capture.clone());
        answer_capture(&world, (1, 1), || panic!("no request, no read"));

        capture.request();
        assert!(wants_frame(&world, request(false, false), true));
        answer_capture(&world, (1, 1), || Ok(vec![1, 2, 3, 4]));
        assert!(!capture.is_requested());
        let frame = capture.read().expect("answered");
        assert_eq!(
            (frame.width, frame.height, frame.rgba8),
            (1, 1, vec![1, 2, 3, 4])
        );

        capture.request();
        answer_capture(&world, (1, 1), || Err("device lost".into()));
        assert!(!capture.is_requested(), "a failed read still clears");
    }

    /// The clear colour is the viewport's, through the same conversion
    /// every emitter uses.
    #[test]
    fn the_clear_colour_is_the_viewports() {
        let mut world = render_world();
        world.resource_mut::<Viewport>().clear = Color::rgb(1.0, 0.0, 0.0);
        assert_eq!(clear_color(&world), peniko_color(Color::rgb(1.0, 0.0, 0.0)));
    }
}
