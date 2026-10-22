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
use crate::layers::LayerCache;
use crate::walker::{WalkContext, scene_has_damage, walk_node};
use crate::{PaintTarget, peniko_color};
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use lumen_core::native::NativePainters;
use lumen_core::render_world::{
    SurfaceCapture, SurfaceFrame, is_primary_target, render_viewport, render_window,
    render_window_mut,
};
use lumen_core::traits::FrameRequest;
use lumen_text::{ShaperService, TextShaper};

/// Whether a renderer should paint `target`'s frame.
///
/// A pending screenshot request for the window always paints, because it is
/// answered from the frame [`answer_capture`] reads after painting. So does
/// a target that holds no frame yet (`holds_frame == false`): one just
/// created or resized has no valid pixels. Otherwise the frame paints when
/// the tick reported a change and either the caller forces a full frame or
/// the window's retained tree differs from the last one painted into it
/// ([`scene_has_damage`]). The dirty flag over-approximates, raised by writes
/// that leave the painted tree identical; the diff is what keeps those from
/// costing a frame, and it is what keeps a window whose content did not
/// change from repainting when another window's did.
pub fn wants_frame(
    render_world: &World,
    target: Entity,
    request: FrameRequest,
    holds_frame: bool,
) -> bool {
    capture_requested(render_world, target)
        || !holds_frame
        || (request.dirty && (request.force_full || scene_has_damage(render_world, target)))
}

/// Whether an off-thread screenshot request is waiting on `target`'s frame.
pub fn capture_requested(render_world: &World, target: Entity) -> bool {
    render_world
        .get_resource::<SurfaceCapture>()
        .is_some_and(|c| c.wants(target, is_primary_target(render_world, target)))
}

/// The colour `target`'s frame starts from: its window's clear colour.
pub fn clear_color(render_world: &World, target: Entity) -> peniko::Color {
    peniko_color(
        render_viewport(render_world, target)
            .map(|v| v.clear)
            .unwrap_or_default(),
    )
}

/// Walk `target`'s retained tree into `painter`, then park the tree as the
/// window's previous tree, which the next frame's gate diffs against.
///
/// The caller resets its sink first. The walker scales every leaf from
/// logical to physical pixels by the window's scale factor, paints text
/// through the render world's [`ShaperService`] (skipping it when there is
/// none), dispatches native leaves to the registered [`NativePainters`],
/// replays repeated appearances from `cache` when the sink records fragments,
/// and composites faded groups from the layers kept in `layers` when the sink
/// paints offscreen.
pub fn paint_frame(
    render_world: &mut World,
    target: Entity,
    painter: &mut PaintTarget,
    cache: Option<&mut FragmentCache>,
    mut layers: Option<&mut LayerCache>,
) {
    // Carve a local Arc of the tree so the borrow on the world releases
    // before the shaper is taken mutably.
    let root = render_window(render_world, target).and_then(|w| w.scene.clone());
    let (dpr, size) = render_viewport(render_world, target)
        .map_or((1.0, glam::Vec2::ZERO), |v| (v.scale_factor, v.size));
    let dpr = dpr.max(0.01);
    if let Some(layers) = layers.as_deref_mut() {
        let target = size * dpr;
        layers.begin_frame(dpr, target.x.ceil() as u32, target.y.ceil() as u32);
    }
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
        if let Some(layers) = layers.as_deref_mut() {
            ctx = ctx.with_layer_cache(layers);
        }
        walk_node(&mut ctx, root);
    }
    if let Some(layers) = layers {
        layers.end_frame();
    }
    if let Some(mut window) = render_window_mut(render_world, target) {
        window.previous = root;
    }
}

/// Answer a pending screenshot request for `target` with the frame `read`
/// returns, as tightly packed straight-alpha RGBA8 of `width` x `height`.
/// `read` runs only when a request for this window is pending. The request
/// is cleared whether or not the read succeeded, so a persistent failure
/// cannot wedge the requester.
pub fn answer_capture(
    render_world: &World,
    target: Entity,
    (width, height): (u32, u32),
    read: impl FnOnce() -> Result<Vec<u8>, String>,
) {
    let Some(capture) = render_world.get_resource::<SurfaceCapture>() else {
        return;
    };
    if !capture.wants(target, is_primary_target(render_world, target)) {
        return;
    }
    match read() {
        Ok(rgba8) => capture.write(SurfaceFrame {
            window: target,
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
    use lumen_core::render_world::{Brush, Rect, spawn_render_window};
    use lumen_core::window::WindowViewport;
    use std::sync::Arc;

    fn window() -> Entity {
        Entity::from_raw_u32(3).expect("an entity id")
    }

    fn render_world() -> World {
        let mut world = World::new();
        spawn_render_window(&mut world, window(), WindowViewport::default(), true);
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
        assert!(!wants_frame(&world, window(), request(false, false), true));
        assert!(!wants_frame(&world, window(), request(true, false), true));
        assert!(wants_frame(&world, window(), request(true, true), true));
        assert!(wants_frame(&world, window(), request(false, false), false));
    }

    /// A changed tree paints; once painted and parked, the same tree does
    /// not.
    #[test]
    fn a_changed_tree_paints_once() {
        let mut world = render_world();
        render_window_mut(&mut world, window()).unwrap().scene = Some(one_rect());
        assert!(wants_frame(&world, window(), request(true, false), true));

        let mut painter: PaintTarget = Box::<crate::recording::Recording>::default();
        paint_frame(&mut world, window(), &mut painter, None, None);
        assert!(!wants_frame(&world, window(), request(true, false), true));
    }

    /// A target the render world has no window for belongs to a
    /// non-standard embed. The diff cannot say anything about it, so it
    /// claims a change rather than keep a frame that may be stale.
    #[test]
    fn a_target_without_a_window_always_paints() {
        let world = World::new();
        assert!(wants_frame(&world, window(), request(true, false), true));
        assert!(!capture_requested(&world, window()));
    }

    /// A pending screenshot paints even a clean tick, is answered with what
    /// the read returns, and is cleared whether or not the read worked.
    #[test]
    fn a_capture_paints_and_is_answered_then_cleared() {
        let mut world = render_world();
        let other = Entity::from_raw_u32(9).expect("an entity id");
        spawn_render_window(&mut world, other, WindowViewport::default(), false);
        let capture = SurfaceCapture::default();
        world.insert_resource(capture.clone());
        answer_capture(&world, window(), (1, 1), || panic!("no request, no read"));

        capture.request();
        assert!(wants_frame(&world, window(), request(false, false), true));
        assert!(
            !capture_requested(&world, other),
            "a request for the primary window leaves the others alone"
        );
        answer_capture(&world, other, (1, 1), || panic!("not this window"));
        answer_capture(&world, window(), (1, 1), || Ok(vec![1, 2, 3, 4]));
        assert!(!capture.is_requested());
        let frame = capture.read().expect("answered");
        assert_eq!(
            (frame.width, frame.height, frame.rgba8),
            (1, 1, vec![1, 2, 3, 4])
        );

        capture.request_window(Some(other));
        assert!(capture_requested(&world, other));
        assert!(!capture_requested(&world, window()));
        answer_capture(&world, other, (1, 1), || Err("device lost".into()));
        assert!(!capture.is_requested(), "a failed read still clears");
    }

    /// The clear colour is the window's, through the same conversion
    /// every emitter uses.
    #[test]
    fn the_clear_colour_is_the_windows() {
        let mut world = World::new();
        spawn_render_window(
            &mut world,
            window(),
            WindowViewport {
                clear: Color::rgb(1.0, 0.0, 0.0),
                ..WindowViewport::default()
            },
            true,
        );
        assert_eq!(
            clear_color(&world, window()),
            peniko_color(Color::rgb(1.0, 0.0, 0.0))
        );
    }
}
