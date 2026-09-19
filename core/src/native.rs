//! Native paint seam - the way a plugin draws its own pixels inside the retained scene.
//!
//! A plugin that owns a drawing surface (a chart, a map, a canvas) has content the core
//! primitives cannot describe. The seam gives it an opaque leaf: the plugin contributes an
//! [`ExtractedNative`] from a normal extract fn, registers a [`NativePainter`] for the same
//! `extension_id`, and the render backend calls that painter with its own draw target when the
//! leaf's turn comes in paint order.
//!
//! This is the same shape as `QSGRenderNode` in Qt SceneGraph and `GskGLShaderNode` in GTK 4:
//! an opaque leaf that the scene graph positions, orders, and clips, and whose interior only the
//! backend and the extension understand.
//!
//! ## Contracts
//!
//! - **Bounds enclose the paint.** [`ExtractedNative::bounds`] must cover every pixel the painter
//!   touches. The damage diff repaints exactly that rect, so paint outside it survives as stale
//!   pixels until something else damages the region.
//! - **`revision` is pixel identity.** Two leaves with the same `extension_id`, `bounds`,
//!   `clip_to_bounds`, and `revision` are treated as identical pixels and contribute no damage.
//!   Call [`next_revision`] whenever the content changes, or the frame never repaints. Payload
//!   identity is deliberately not part of the comparison: producers rebuild the `Arc` on every
//!   dirty frame, which would mark the leaf changed every time.
//! - **Painters take `&self`.** One painter serves every leaf carrying its `extension_id`, so
//!   per-frame state goes behind interior mutability.
//! - **`extension_id` names a contract**, covering both the concrete payload type and the API the
//!   painter expects from the backend. A backend with no painter registered for an id skips the
//!   leaf, which is what keeps a scene portable across backends.
//! - **Bounds with no area never repaint on their own.** Damage is a rect, and an empty rect is no
//!   damage, so a leaf that declares zero width or height falls back to whole-viewport damage when
//!   its revision moves. Declare the area you paint.
//!
//! ## Producing leaves
//!
//! [`NativeExtract`] resolves the placement of one entity - paint order, scroll offset, inherited
//! opacity, hidden subtrees - the same way the built-in extractors do, and
//! [`upsert_native_leaves`] carries the leaves across frames under one extension id so two plugins
//! extracting in the same frame cannot evict each other's entities.

use crate::components::{Opacity, Transform};
use crate::node_ir::Affine2;
use crate::render_world::{
    PaintOrder, Rect, RenderEntityMap, aabb_outside, build_parent_map, effective_opacity,
    hidden_entities, paint_order_of, parent_opacities, parent_scroll_clip_rects,
    parent_scroll_offsets,
};
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Resource};
use bevy_ecs::world::World;
use glam::Vec2;
use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Render-world component describing one backend-painted leaf.
///
/// Spawned by a plugin's extract fn like any other `Extracted*` component, so it inherits document
/// paint order via [`crate::render_world::paint_order_of`], enclosing clip brackets, the top-layer
/// band, the [`crate::render_world::clear_extracted`] lifecycle, and the hidden-subtree sweep.
#[derive(Component, Clone)]
pub struct ExtractedNative {
    /// Identifies the extension that owns this leaf. The backend dispatches on it and skips the
    /// leaf when no painter is registered.
    pub extension_id: Arc<str>,
    /// Type-erased draw data. The painter downcasts it to the type its `extension_id` promises.
    pub payload: Arc<dyn Any + Send + Sync>,
    /// Bounding rect in logical window coordinates. Encloses every pixel the painter touches.
    pub bounds: Rect,
    /// Painter-algorithm sort key, from [`crate::render_world::paint_order_of`].
    pub order: PaintOrder,
    /// Content stamp. Equal revisions mean identical pixels; see [`next_revision`].
    pub revision: u64,
    /// When `true`, the backend clips the painter to [`Self::bounds`].
    pub clip_to_bounds: bool,
}

impl fmt::Debug for ExtractedNative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractedNative")
            .field("extension_id", &self.extension_id)
            .field("bounds", &self.bounds)
            .field("order", &self.order)
            .field("revision", &self.revision)
            .field("clip_to_bounds", &self.clip_to_bounds)
            .finish()
    }
}

/// Monotonic content stamps for [`ExtractedNative::revision`].
///
/// Every call returns a value no other call returns, so a producer that stamps its leaf on each
/// content change can never collide with a stale value.
pub fn next_revision() -> u64 {
    static REVISION: AtomicU64 = AtomicU64::new(1);
    REVISION.fetch_add(1, Ordering::Relaxed)
}

/// Where one entity's leaf belongs this frame, as [`NativeExtract`] resolved it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativePlacement {
    /// Bounds in window coordinates, with ancestor scroll offsets already applied.
    pub bounds: Rect,
    /// Paint order for the entity's position in the document.
    pub order: PaintOrder,
    /// Opacity inherited from ancestors, multiplied by the entity's own.
    pub opacity: f32,
}

/// The hierarchy-derived facts a plugin's extract fn needs, computed once per frame.
///
/// A leaf placed by hand from `Transform::absolute` alone ignores everything an ancestor says: it
/// stays pinned while its scroll container scrolls, ignores `opacity`, and keeps painting inside a
/// hidden subtree. Build one of these at the top of the extract fn and ask it to
/// [`place`](Self::place) each entity instead. It reads the same memo the built-in extractors
/// share, so the maps are built once per frame no matter how many extractors ask.
pub struct NativeExtract {
    parents: HashMap<Entity, Entity>,
    depth_cache: HashMap<Entity, u32>,
    hidden: std::collections::HashSet<Entity>,
    scroll: HashMap<Entity, Vec2>,
    opacities: HashMap<Entity, f32>,
    clip: HashMap<Entity, (Vec2, Vec2)>,
}

impl NativeExtract {
    /// Reads the frame's hierarchy, scroll, opacity, visibility, and clip maps from the main world.
    pub fn new(main: &mut World) -> Self {
        let (parents, depth_cache) = build_parent_map(main);
        let hidden = hidden_entities(main, &parents);
        let scroll = parent_scroll_offsets(main, &parents);
        let opacities = parent_opacities(main, &parents);
        let clip = parent_scroll_clip_rects(main, &parents);
        Self {
            parents,
            depth_cache,
            hidden,
            scroll,
            opacities,
            clip,
        }
    }

    /// Resolves where `entity`'s leaf belongs, or `None` when it should not be extracted at all:
    /// hidden by a `Visible(false)` on itself or an ancestor, or scrolled fully out of the nearest
    /// scroll or `overflow: hidden` container.
    pub fn place(
        &mut self,
        entity: Entity,
        transform: &Transform,
        opacity: Option<&Opacity>,
    ) -> Option<NativePlacement> {
        if self.hidden.contains(&entity) {
            return None;
        }
        let offset = self.scroll.get(&entity).copied().unwrap_or(Vec2::ZERO);
        let origin = transform.absolute - offset;
        if let Some(clip_rect) = self.clip.get(&entity)
            && aabb_outside(origin, transform.size, *clip_rect)
        {
            return None;
        }
        Some(NativePlacement {
            bounds: Rect::new(origin, transform.size),
            order: paint_order_of(entity, &self.parents, &mut self.depth_cache),
            opacity: effective_opacity(opacity, &self.opacities, entity).0,
        })
    }
}

/// Carries one extension's leaves into the render world, reusing each entity's render-world entity
/// across frames and despawning the ones that went away.
///
/// The lifecycle is scoped to `extension_id`, so several plugins extracting in the same frame keep
/// their own sets: a plugin only ever retires leaves it produced. Pass the id the leaves carry.
pub fn upsert_native_leaves<I>(render: &mut World, extension_id: &str, leaves: I)
where
    I: IntoIterator<Item = (Entity, ExtractedNative)>,
{
    let mut map = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().native);
    let mut prior: HashMap<Entity, Entity> = HashMap::new();
    map.retain(|(id, main_e), render_e| {
        if &**id == extension_id {
            prior.insert(*main_e, *render_e);
            false
        } else {
            true
        }
    });

    let id: Arc<str> = Arc::from(extension_id);
    for (main_e, leaf) in leaves {
        let reuse = prior
            .remove(&main_e)
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(leaf);
                re
            }
            None => render.spawn(leaf).id(),
        };
        map.insert((id.clone(), main_e), render_e);
    }
    // Whatever is left in `prior` had a leaf last frame and none now.
    for (_, render_e) in prior {
        if let Ok(em) = render.get_entity_mut(render_e) {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().native = map;
}

/// What a backend hands a [`NativePainter`] for one leaf.
///
/// Both the payload and the draw target are type-erased, which is what keeps this crate free of
/// backend dependencies. A painter downcasts the payload to the type its `extension_id` promises
/// and the target to the type the backend named in [`Self::backend_id`]; a painter that does not
/// recognise the backend draws nothing.
pub struct NativePaintCtx<'a> {
    payload: &'a (dyn Any + Send + Sync),
    target: &'a mut dyn Any,
    /// Identifies the backend that owns [`Self::target`], for example `"lumen.render-wgpu"`.
    pub backend_id: &'static str,
    /// The leaf's bounds in logical window coordinates.
    pub bounds: Rect,
    /// Transform accumulated from ancestor transform nodes, in logical coordinates. The tree
    /// builder emits no transform nodes today, so this is the identity for now; compose through
    /// [`Self::device_transform`] rather than assuming it.
    pub transform: Affine2,
    /// Device pixel ratio between logical coordinates and the target's pixels.
    pub dpr: f32,
    /// Alpha multiplier accumulated from ancestor opacity nodes.
    ///
    /// The painter is the only thing that applies it. A bounds clip composites nothing, so asking
    /// to be clipped never changes a leaf's alpha. The tree builder emits no opacity nodes today,
    /// so this is `1.0` for now; the way to honour CSS `opacity` is to fold
    /// [`NativePlacement::opacity`] into the payload at extract, which is what the built-in
    /// extractors do with their own colours.
    pub opacity: f32,
}

impl<'a> NativePaintCtx<'a> {
    /// Builds a context around one leaf's payload and the backend's draw target.
    pub fn new(
        payload: &'a (dyn Any + Send + Sync),
        target: &'a mut dyn Any,
        backend_id: &'static str,
        bounds: Rect,
        transform: Affine2,
        dpr: f32,
        opacity: f32,
    ) -> Self {
        Self {
            payload,
            target,
            backend_id,
            bounds,
            transform,
            dpr,
            opacity,
        }
    }

    /// Borrows the payload as `T`, or `None` when the leaf carries something else.
    pub fn payload_as<T: Any>(&self) -> Option<&'a T> {
        self.payload.downcast_ref::<T>()
    }

    /// Borrows the draw target as `T`, or `None` when this backend's target is something else.
    pub fn target_as<T: Any>(&mut self) -> Option<&mut T> {
        self.target.downcast_mut::<T>()
    }

    /// The transform from logical leaf coordinates to target pixels: the ancestor transform
    /// followed by the device-pixel scale.
    pub fn device_transform(&self) -> Affine2 {
        Affine2::scale(self.dpr as f64) * self.transform
    }
}

/// Paints one [`ExtractedNative`] leaf onto a backend draw target.
///
/// A single painter serves every leaf carrying its `extension_id`, so it takes `&self` and keeps
/// any per-frame state behind interior mutability.
pub trait NativePainter: Send + Sync + 'static {
    /// Draws the leaf described by `ctx`.
    fn paint(&self, ctx: &mut NativePaintCtx<'_>);
}

/// Render-world registry mapping an `extension_id` to the painter that draws it.
///
/// Cloning shares the table; registering on a clone copies it first, so a backend can take a cheap
/// snapshot of the registry before it starts a frame.
#[derive(Resource, Clone, Default)]
pub struct NativePainters {
    painters: Arc<HashMap<Arc<str>, Arc<dyn NativePainter>>>,
}

impl NativePainters {
    /// Registers `painter` for `extension_id`, replacing any painter already registered for it.
    pub fn register<P: NativePainter>(&mut self, extension_id: impl Into<Arc<str>>, painter: P) {
        Arc::make_mut(&mut self.painters).insert(extension_id.into(), Arc::new(painter));
    }

    /// Returns the painter registered for `extension_id`, or `None`.
    pub fn get(&self, extension_id: &str) -> Option<&Arc<dyn NativePainter>> {
        self.painters.get(extension_id)
    }

    /// Number of registered painters.
    pub fn len(&self) -> usize {
        self.painters.len()
    }

    /// Returns `true` when no painter is registered.
    pub fn is_empty(&self) -> bool {
        self.painters.is_empty()
    }
}

impl fmt::Debug for NativePainters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativePainters")
            .field("registered", &self.painters.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Recorder {
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl NativePainter for Recorder {
        fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
            let label = ctx
                .payload_as::<String>()
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            self.seen.lock().expect("recorder lock").push(label);
        }
    }

    struct Noop;

    impl NativePainter for Noop {
        fn paint(&self, _ctx: &mut NativePaintCtx<'_>) {}
    }

    fn ctx_over<'a>(
        payload: &'a (dyn Any + Send + Sync),
        target: &'a mut dyn Any,
    ) -> NativePaintCtx<'a> {
        NativePaintCtx::new(
            payload,
            target,
            "test.backend",
            Rect::new(glam::Vec2::ZERO, glam::Vec2::new(10.0, 10.0)),
            Affine2::IDENTITY,
            1.0,
            1.0,
        )
    }

    /// A painter is found by the id it was registered under, and registering the
    /// same id again replaces it - so a plugin reinstalled at runtime does not
    /// leave the old painter drawing.
    #[test]
    fn the_last_painter_registered_for_an_id_is_the_one_that_paints() {
        let mut registry = NativePainters::default();
        assert!(registry.is_empty());

        registry.register("demo.native", Noop);
        registry.register("demo.other", Noop);
        assert_eq!(registry.len(), 2);
        assert!(registry.get("demo.native").is_some());
        assert!(registry.get("demo.missing").is_none());

        let recorder = Recorder::default();
        registry.register("demo.native", recorder.clone());
        assert_eq!(registry.len(), 2, "re-registering replaces, not appends");

        let payload: Arc<dyn Any + Send + Sync> = Arc::new("hello".to_string());
        let mut target = ();
        let painter = registry.get("demo.native").expect("painter").clone();
        painter.paint(&mut ctx_over(payload.as_ref(), &mut target));

        assert_eq!(recorder.seen.lock().expect("lock").as_slice(), ["hello"]);
    }

    /// A backend snapshots the registry for the frame it is about to paint.
    /// Registering after the snapshot must not reach into it.
    #[test]
    fn a_snapshot_of_the_registry_does_not_see_later_registrations() {
        let mut registry = NativePainters::default();
        registry.register("demo.native", Noop);

        let snapshot = registry.clone();
        registry.register("demo.late", Noop);

        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.get("demo.late").is_none());
        assert!(registry.get("demo.late").is_some());
    }

    /// The painter maps its own logical coordinates to target pixels through
    /// `device_transform`: ancestor placement first, device scale second.
    #[test]
    fn device_transform_scales_the_ancestor_transform() {
        let payload: Arc<dyn Any + Send + Sync> = Arc::new(7u32);
        let mut target = ();
        let mut ctx = ctx_over(payload.as_ref(), &mut target);
        ctx.transform = Affine2::translate(10.0, 20.0);
        ctx.dpr = 2.0;

        assert_eq!(
            ctx.device_transform().coeffs,
            [2.0, 0.0, 0.0, 2.0, 20.0, 40.0]
        );

        ctx.dpr = 1.0;
        assert_eq!(ctx.device_transform().coeffs, ctx.transform.coeffs);
    }

    /// Downcasting is how a painter checks that a leaf really carries the payload
    /// its `extension_id` promises; a mismatch reads as absent rather than
    /// misinterpreting the bytes.
    #[test]
    fn a_payload_of_the_wrong_type_reads_as_absent() {
        let payload: Arc<dyn Any + Send + Sync> = Arc::new(7u32);
        let mut target = 5i64;
        let mut ctx = ctx_over(payload.as_ref(), &mut target);

        assert_eq!(ctx.payload_as::<u32>(), Some(&7));
        assert!(ctx.payload_as::<String>().is_none());
        assert_eq!(ctx.target_as::<i64>(), Some(&mut 5));
        assert!(ctx.target_as::<String>().is_none());
    }

    /// Revisions are what tell the damage diff that pixels changed, so two of
    /// them are never equal.
    #[test]
    fn revisions_never_repeat() {
        let a = next_revision();
        let b = next_revision();
        let c = next_revision();
        assert!(a < b && b < c);
    }
}
