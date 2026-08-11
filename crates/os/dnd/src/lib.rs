//! Drag-and-drop abstractions for Lumen.
//!
//! Reuses the [`MimePayload`] type from [`lumen-os-mime`] so a single
//! payload travels across both the clipboard and the DnD pipeline -
//! mirrors `QMimeData` / `GdkContentProvider` (per os-integration audit
//! section 104-110, section 469).
//!
//! In-app drag from a [`DragSource`] entity to a `lumen-core::DropTarget`
//! is now wired end to end: [`begin_in_app_drag`] opens an [`ActiveDrag`]
//! session on the drag-gesture threshold (fed by
//! `lumen-primitives::drag`), [`track_drop_hover`] follows the pointer
//! marking the hovered target with `DropHovered`, and
//! [`finish_in_app_drag`] hit-tests the release point and emits
//! [`DropAccepted`]. The inbound file-drop path continues to work
//! unchanged via `lumen-core::FileDropped` / `PendingFileDrops`
//! ([`translate_file_drops_to_payload`]). Cross-window drag-source via
//! the platform (`NSDraggingSession`, `DoDragDrop`, `wl_data_source`/Xdnd)
//! is deferred - winit exposes no source API today.
//!
//! Mirrors: the source-publishes-mime -> target-negotiates-effect ->
//! drop-carries-source+data shape of `QDrag`/`QMimeData`/`dropEvent`, and
//! the HTML5 `dragstart`/`dragenter`/`drop` + `dataTransfer` event model
//! (so the markup `draggable` / `drop-target` / `on-drop` surface maps
//! 1:1 onto real DOM DnD when transpiled).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::{DropHovered, DropTarget, Transform};
use lumen_core::input::{DragEndEvent, DragStartEvent, FileDropped, PointerState, ScrollOffset};
use lumen_os_mime::{MimeKind, MimePayload};

pub use lumen_os_mime as mime;

/// Marker / data component for an entity that can be the *source* of a
/// drag-and-drop. Authored by the markup layer (`<draggable mime=...>`
/// follow-up) or imperatively by the app.
///
/// The payload is built eagerly - for lazy construction (GTK
/// `DragSource::prepare`), wrap a closure inside a `Custom` blob and
/// resolve at drag-start time. v1 ships eager.
#[derive(Component, Clone, Debug)]
pub struct DragSource {
    /// The payload exported when a drag begins on this entity.
    pub payload: MimePayload,
    /// Allowed effects. v1 only carries the data - backends choose the
    /// cursor.
    pub effects: DropEffectSet,
}

impl DragSource {
    /// Construct a drag source from any value convertible into a
    /// [`MimePayload`].
    pub fn new(payload: impl Into<MimePayload>) -> Self {
        Self {
            payload: payload.into(),
            effects: DropEffectSet::COPY,
        }
    }

    /// Builder: limit the effects this source advertises.
    pub fn with_effects(mut self, effects: DropEffectSet) -> Self {
        self.effects = effects;
        self
    }
}

/// Bitset of drop effects a source advertises or a target accepts.
///
/// Mirrors `Qt::DropAction` / `NSDragOperation` / `Gdk::DragAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct DropEffectSet(pub u8);

impl DropEffectSet {
    /// Empty set - drop is rejected.
    pub const NONE: Self = DropEffectSet(0);
    /// Copy the data, leaving the source intact.
    pub const COPY: Self = DropEffectSet(1 << 0);
    /// Move the data - source should delete after a successful drop.
    pub const MOVE: Self = DropEffectSet(1 << 1);
    /// Link / reference the data without copying.
    pub const LINK: Self = DropEffectSet(1 << 2);
    /// Any of the above.
    pub const ANY: Self = DropEffectSet(0b111);

    /// True when this set contains every bit in `other`.
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Intersection with another set.
    pub fn intersect(self, other: Self) -> Self {
        DropEffectSet(self.0 & other.0)
    }

    /// True when no bits are set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// MIME-type filter declared on a [`lumen_core::DropTarget`] (extended
/// by this crate). Drops are rejected when none of the configured
/// kinds match the source payload.
///
/// Authored separately from the marker component so the existing
/// `lumen-core::DropTarget` stays untouched; entities can carry both.
#[derive(Component, Clone, Debug, Default)]
pub struct DropAccept {
    /// MIME kinds this target accepts. Empty means "accept any".
    pub kinds: Vec<MimeKind>,
    /// Effects this target advertises. Empty (`NONE`) rejects.
    pub effects: DropEffectSet,
}

impl DropAccept {
    /// Accept any MIME - semantics today's `DropTarget` already has.
    pub fn any() -> Self {
        Self {
            kinds: Vec::new(),
            effects: DropEffectSet::COPY,
        }
    }

    /// Accept only the given MIME kinds.
    pub fn only(kinds: impl IntoIterator<Item = MimeKind>) -> Self {
        Self {
            kinds: kinds.into_iter().collect(),
            effects: DropEffectSet::COPY,
        }
    }

    /// Builder: set the allowed effects.
    pub fn with_effects(mut self, effects: DropEffectSet) -> Self {
        self.effects = effects;
        self
    }

    /// True when this accept-set permits the supplied payload.
    pub fn accepts(&self, payload: &MimePayload) -> bool {
        if self.kinds.is_empty() {
            return true;
        }
        self.kinds.iter().any(|k| payload.has(k))
    }
}

/// Emitted when a drag starts on a [`DragSource`] entity (in-app
/// gesture). Cross-window drag-source via the platform is deferred -
/// see crate docs.
#[derive(Message, Clone, Debug)]
pub struct DragStarted {
    /// Source entity carrying [`DragSource`].
    pub source: Entity,
    /// Payload the source published.
    pub payload: MimePayload,
}

/// Emitted when the user releases the drag on a hovered drop target
/// (in-app gesture).
#[derive(Message, Clone, Debug)]
pub struct DropAccepted {
    /// Source entity, when the drop originated in-app.
    pub source: Option<Entity>,
    /// Target entity receiving the drop.
    pub target: Entity,
    /// Payload delivered.
    pub payload: MimePayload,
    /// Effect chosen by the target.
    pub effect: DropEffect,
}

/// Single resolved drop effect (one bit of [`DropEffectSet`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DropEffect {
    /// Drop rejected - for parity with `Qt::IgnoreAction`.
    #[default]
    None,
    /// Copy data.
    Copy,
    /// Move data.
    Move,
    /// Link / reference data.
    Link,
}

impl DropEffect {
    /// Pick a single effect from a set, preferring `Copy` -> `Move` ->
    /// `Link` -> `None`. Mirrors `NSDragOperation` / `Qt::DropAction`
    /// default behaviour.
    pub fn pick(set: DropEffectSet) -> DropEffect {
        if set.contains(DropEffectSet::COPY) {
            DropEffect::Copy
        } else if set.contains(DropEffectSet::MOVE) {
            DropEffect::Move
        } else if set.contains(DropEffectSet::LINK) {
            DropEffect::Link
        } else {
            DropEffect::None
        }
    }
}

/// Translate inbound platform `FileDropped` events into [`DropAccepted`]
/// when the target carries a [`DropAccept`] filter. Targets without a
/// `DropAccept` keep the old "accept anything" behaviour, so
/// `apps/drop-target` keeps working.
///
/// This is the only DnD system the crate ships in W6.2 - the in-app
/// drag-gesture pipeline is wired by `lumen-primitives::drag` (already
/// present) and tagged for hookup in a follow-up.
pub fn translate_file_drops_to_payload(
    mut file_drops: MessageReader<FileDropped>,
    accept: Query<&DropAccept>,
    mut out: MessageWriter<DropAccepted>,
) {
    for ev in file_drops.read() {
        // Build a uri-list payload from the single dropped file -
        // mirrors what the platform DnD layer would have sent.
        let payload: MimePayload = vec![ev.path.clone()].into();
        let effect = if let Ok(da) = accept.get(ev.entity) {
            if !da.accepts(&payload) {
                continue;
            }
            DropEffect::pick(da.effects)
        } else {
            DropEffect::Copy
        };
        out.write(DropAccepted {
            source: None,
            target: ev.entity,
            payload: payload.clone(),
            effect,
        });
    }
}

/// In-flight in-app drag session: the source entity, the payload it
/// published at drag-start, and the effects it advertises. `None` when no
/// drag gesture is active.
///
/// Holds the payload for the whole gesture the way a platform session
/// (`NSDraggingSession` / `IDataObject` behind `DoDragDrop`) owns the
/// dragged data - so the drop can deliver it even though the source
/// component may have changed underneath a reactive `<for>` rebuild.
#[derive(Resource, Clone, Debug, Default)]
pub struct ActiveDrag(pub Option<ActiveDragData>);

/// Payload of an in-flight [`ActiveDrag`].
#[derive(Clone, Debug)]
pub struct ActiveDragData {
    /// Entity that started the drag (carries [`DragSource`]).
    pub source: Entity,
    /// Payload captured at drag-start.
    pub payload: MimePayload,
    /// Effects the source advertises (negotiated against each target).
    pub effects: DropEffectSet,
}

/// Sum every ancestor's [`ScrollOffset`] (excluding the entity's own) so
/// a target's logical bounds match what the user sees - the same walk
/// `lumen-input::ancestor_scroll` performs for the file-drop hit-test,
/// inlined here to keep `lumen-os-dnd` free of an `lumen-input` dep.
fn ancestor_scroll_offset(
    entity: Entity,
    parents: &Query<&ChildOf>,
    scrolls: &Query<&ScrollOffset>,
) -> Vec2 {
    let mut total = Vec2::ZERO;
    let mut cur = entity;
    while let Ok(child_of) = parents.get(cur) {
        let parent = child_of.parent();
        if let Ok(off) = scrolls.get(parent) {
            total += off.0;
        }
        cur = parent;
    }
    total
}

/// Hit-test `pos` against every [`DropTarget`], returning the topmost
/// (deepest in the hierarchy, then highest entity id) target that accepts
/// `payload`, paired with the negotiated [`DropEffect`].
///
/// Effect negotiation mirrors `Qt::DropAction` / `acceptProposedAction`:
/// the source's advertised `effects` are intersected with the target's
/// [`DropAccept::effects`], then [`DropEffect::pick`] resolves the single
/// action. A target with no [`DropAccept`] keeps the legacy "accept
/// anything, Copy" behaviour so the file-drop path and `apps/drop-target`
/// are unchanged.
#[allow(clippy::type_complexity)]
fn topmost_target(
    pos: Vec2,
    payload: &MimePayload,
    source_effects: DropEffectSet,
    targets: &Query<(Entity, &Transform, Option<&DropAccept>), With<DropTarget>>,
    parents: &Query<&ChildOf>,
    scrolls: &Query<&ScrollOffset>,
) -> Option<(Entity, DropEffect)> {
    let mut best: Option<(u32, Entity, DropEffect)> = None;
    for (e, t, accept) in targets.iter() {
        let off = ancestor_scroll_offset(e, parents, scrolls);
        let origin = t.absolute - off;
        if !(pos.x >= origin.x
            && pos.y >= origin.y
            && pos.x < origin.x + t.size.x
            && pos.y < origin.y + t.size.y)
        {
            continue;
        }
        let effect = match accept {
            Some(da) => {
                if !da.accepts(payload) {
                    continue;
                }
                DropEffect::pick(da.effects.intersect(source_effects))
            }
            None => DropEffect::pick(source_effects),
        };
        if effect == DropEffect::None {
            continue;
        }
        let mut depth = 0u32;
        let mut cur = e;
        while let Ok(co) = parents.get(cur) {
            depth += 1;
            cur = co.parent();
        }
        match best {
            None => best = Some((depth, e, effect)),
            Some((bd, be, _)) if (depth, e) > (bd, be) => best = Some((depth, e, effect)),
            _ => {}
        }
    }
    best.map(|(_, e, eff)| (e, eff))
}

/// Open an [`ActiveDrag`] session and announce [`DragStarted`] when a
/// drag gesture crosses the threshold on a [`DragSource`] entity. Mirrors
/// `QDrag::exec` beginning a drag with the source's `QMimeData` /
/// HTML5 `dragstart` populating `dataTransfer`.
pub fn begin_in_app_drag(
    mut starts: MessageReader<DragStartEvent>,
    sources: Query<&DragSource>,
    mut active: ResMut<ActiveDrag>,
    mut out: MessageWriter<DragStarted>,
) {
    for ev in starts.read() {
        if let Ok(src) = sources.get(ev.entity) {
            active.0 = Some(ActiveDragData {
                source: ev.entity,
                payload: src.payload.clone(),
                effects: src.effects,
            });
            out.write(DragStarted {
                source: ev.entity,
                payload: src.payload.clone(),
            });
        }
    }
}

/// While a drag is active, keep [`DropHovered`] on the topmost accepting
/// [`DropTarget`] under the pointer (and off every other target). Mirrors
/// HTML5 `dragenter` / `dragleave` so `:drag-over` styling follows the
/// cursor.
pub fn track_drop_hover(
    mut commands: Commands,
    active: Res<ActiveDrag>,
    pointer: Res<PointerState>,
    targets: Query<(Entity, &Transform, Option<&DropAccept>), With<DropTarget>>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    hovered: Query<Entity, With<DropHovered>>,
) {
    let want = match (&active.0, pointer.position) {
        (Some(d), Some(pos)) => {
            topmost_target(pos, &d.payload, d.effects, &targets, &parents, &scrolls).map(|(e, _)| e)
        }
        _ => None,
    };
    for e in &hovered {
        if Some(e) != want {
            commands.entity(e).remove::<DropHovered>();
        }
    }
    if let Some(e) = want
        && !hovered.contains(e)
    {
        commands.entity(e).insert(DropHovered);
    }
}

/// On the release edge of an active drag, hit-test the drop point and
/// emit [`DropAccepted`] at the topmost accepting target - the in-app
/// twin of [`translate_file_drops_to_payload`]. Mirrors Qt `dropEvent`
/// carrying the source + `QMimeData` / HTML5 `drop` carrying
/// `dataTransfer`.
#[allow(clippy::too_many_arguments)] // ECS system: each arg is a query/param
pub fn finish_in_app_drag(
    mut commands: Commands,
    mut ends: MessageReader<DragEndEvent>,
    mut active: ResMut<ActiveDrag>,
    targets: Query<(Entity, &Transform, Option<&DropAccept>), With<DropTarget>>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    hovered: Query<Entity, With<DropHovered>>,
    mut out: MessageWriter<DropAccepted>,
) {
    for ev in ends.read() {
        let Some(data) = active.0.clone() else {
            continue;
        };
        if ev.entity != data.source {
            continue;
        }
        if let Some((target, effect)) = topmost_target(
            ev.position,
            &data.payload,
            data.effects,
            &targets,
            &parents,
            &scrolls,
        ) {
            out.write(DropAccepted {
                source: Some(data.source),
                target,
                payload: data.payload.clone(),
                effect,
            });
        }
        active.0 = None;
        for e in &hovered {
            commands.entity(e).remove::<DropHovered>();
        }
    }
}

/// Plugin: registers DnD messages + both the platform file-drop -> payload
/// translator and the in-app drag-gesture -> drop pipeline.
pub struct DndPlugin;

impl lumen_core::app::Plugin for DndPlugin {
    fn build(self, app: &mut lumen_core::app::App) {
        app.add_message::<DragStarted>();
        app.add_message::<DropAccepted>();
        app.world.insert_resource(ActiveDrag::default());
        app.add_systems(
            lumen_core::tick::TickStage::Systems,
            translate_file_drops_to_payload,
        );
        // In-app gesture path: open the session, resolve the drop, then
        // refresh the hover marker (so a just-finished drag clears its
        // `:drag-over` in the same tick).
        app.add_systems(lumen_core::tick::TickStage::Systems, begin_in_app_drag);
        app.add_systems(
            lumen_core::tick::TickStage::Systems,
            finish_in_app_drag.after(begin_in_app_drag),
        );
        app.add_systems(
            lumen_core::tick::TickStage::Systems,
            track_drop_hover.after(finish_in_app_drag),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_effect_set_basics() {
        let s = DropEffectSet::COPY;
        assert!(s.contains(DropEffectSet::COPY));
        assert!(!s.contains(DropEffectSet::MOVE));
        assert!(!s.is_empty());
        assert!(DropEffectSet::NONE.is_empty());

        let any = DropEffectSet::ANY;
        assert!(any.contains(DropEffectSet::COPY));
        assert!(any.contains(DropEffectSet::MOVE));
        assert!(any.contains(DropEffectSet::LINK));
    }

    #[test]
    fn drop_effect_pick_priority() {
        assert_eq!(DropEffect::pick(DropEffectSet::ANY), DropEffect::Copy);
        assert_eq!(DropEffect::pick(DropEffectSet::MOVE), DropEffect::Move);
        assert_eq!(DropEffect::pick(DropEffectSet::LINK), DropEffect::Link);
        assert_eq!(DropEffect::pick(DropEffectSet::NONE), DropEffect::None);
    }

    #[test]
    fn drop_accept_filters_by_mime() {
        let a = DropAccept::only([MimeKind::TextUriList]);
        let path_payload: MimePayload = vec![std::path::PathBuf::from("/x")].into();
        let text_payload: MimePayload = "hello".into();
        assert!(a.accepts(&path_payload));
        assert!(!a.accepts(&text_payload));
    }

    #[test]
    fn drop_accept_any_accepts_everything() {
        let a = DropAccept::any();
        let p: MimePayload = "x".into();
        assert!(a.accepts(&p));
        assert!(a.accepts(&MimePayload::new()));
    }

    // --- in-app drag-gesture -> drop pipeline ---
    mod in_app {
        use super::super::*;
        use bevy_ecs::message::Messages;
        use bevy_ecs::system::RunSystemOnce;
        use glam::Vec2;
        use lumen_core::components::Transform;

        fn setup() -> World {
            let mut world = World::new();
            world.init_resource::<Messages<DragStartEvent>>();
            world.init_resource::<Messages<DragEndEvent>>();
            world.init_resource::<Messages<DragStarted>>();
            world.init_resource::<Messages<DropAccepted>>();
            world.insert_resource(ActiveDrag::default());
            world.insert_resource(PointerState::default());
            world
        }

        fn target(world: &mut World, origin: Vec2, size: Vec2, accept: DropAccept) -> Entity {
            world
                .spawn((Transform::new(origin, size), DropTarget, accept))
                .id()
        }

        /// Threshold-crossing on a `DragSource` opens the session and
        /// emits `DragStarted`; releasing over an accepting target emits
        /// `DropAccepted` carrying source + payload + effect.
        #[test]
        fn drag_gesture_to_drop_target_emits_accepted() {
            let mut world = setup();
            let src = world.spawn(DragSource::new("card-42")).id();
            let tgt = target(
                &mut world,
                Vec2::new(100.0, 0.0),
                Vec2::new(100.0, 100.0),
                DropAccept::only([MimeKind::TextPlain]),
            );

            world
                .resource_mut::<Messages<DragStartEvent>>()
                .write(DragStartEvent {
                    entity: src,
                    start: Vec2::new(10.0, 10.0),
                    position: Vec2::new(20.0, 10.0),
                });
            world.run_system_once(begin_in_app_drag).unwrap();

            assert!(world.resource::<ActiveDrag>().0.is_some(), "session open");
            let started: Vec<DragStarted> = world
                .resource_mut::<Messages<DragStarted>>()
                .drain()
                .collect();
            assert_eq!(started.len(), 1);
            assert_eq!(started[0].source, src);
            assert_eq!(started[0].payload.text().as_deref(), Some("card-42"));

            // Release inside the target.
            world
                .resource_mut::<Messages<DragEndEvent>>()
                .write(DragEndEvent {
                    entity: src,
                    position: Vec2::new(150.0, 50.0),
                });
            world.run_system_once(finish_in_app_drag).unwrap();

            let dropped: Vec<DropAccepted> = world
                .resource_mut::<Messages<DropAccepted>>()
                .drain()
                .collect();
            assert_eq!(dropped.len(), 1, "one DropAccepted");
            assert_eq!(dropped[0].source, Some(src));
            assert_eq!(dropped[0].target, tgt);
            assert_eq!(dropped[0].payload.text().as_deref(), Some("card-42"));
            assert_eq!(dropped[0].effect, DropEffect::Copy);
            assert!(world.resource::<ActiveDrag>().0.is_none(), "session closed");
        }

        /// Releasing outside every target drops nothing and closes the
        /// session (drag cancelled).
        #[test]
        fn release_outside_targets_is_cancel() {
            let mut world = setup();
            let src = world.spawn(DragSource::new("x")).id();
            target(
                &mut world,
                Vec2::new(0.0, 0.0),
                Vec2::new(50.0, 50.0),
                DropAccept::any(),
            );
            world
                .resource_mut::<Messages<DragStartEvent>>()
                .write(DragStartEvent {
                    entity: src,
                    start: Vec2::ZERO,
                    position: Vec2::new(10.0, 10.0),
                });
            world.run_system_once(begin_in_app_drag).unwrap();
            world
                .resource_mut::<Messages<DragEndEvent>>()
                .write(DragEndEvent {
                    entity: src,
                    position: Vec2::new(500.0, 500.0),
                });
            world.run_system_once(finish_in_app_drag).unwrap();
            assert!(
                world
                    .resource_mut::<Messages<DropAccepted>>()
                    .drain()
                    .next()
                    .is_none()
            );
            assert!(world.resource::<ActiveDrag>().0.is_none());
        }

        /// A target whose MIME filter rejects the payload is skipped.
        #[test]
        fn mime_filter_rejects_drop() {
            let mut world = setup();
            let src = world.spawn(DragSource::new("plain text")).id();
            target(
                &mut world,
                Vec2::new(0.0, 0.0),
                Vec2::new(100.0, 100.0),
                DropAccept::only([MimeKind::ImagePng]),
            );
            world
                .resource_mut::<Messages<DragStartEvent>>()
                .write(DragStartEvent {
                    entity: src,
                    start: Vec2::ZERO,
                    position: Vec2::new(10.0, 10.0),
                });
            world.run_system_once(begin_in_app_drag).unwrap();
            world
                .resource_mut::<Messages<DragEndEvent>>()
                .write(DragEndEvent {
                    entity: src,
                    position: Vec2::new(50.0, 50.0),
                });
            world.run_system_once(finish_in_app_drag).unwrap();
            assert!(
                world
                    .resource_mut::<Messages<DropAccepted>>()
                    .drain()
                    .next()
                    .is_none(),
                "text payload rejected by image-only target"
            );
        }

        /// `track_drop_hover` marks the target under the pointer with
        /// `DropHovered` while a drag is active, and clears it when the
        /// pointer leaves.
        #[test]
        fn hover_marker_follows_pointer() {
            let mut world = setup();
            let src = world.spawn(DragSource::new("c")).id();
            let tgt = target(
                &mut world,
                Vec2::new(0.0, 0.0),
                Vec2::new(100.0, 100.0),
                DropAccept::any(),
            );
            world.resource_mut::<ActiveDrag>().0 = Some(ActiveDragData {
                source: src,
                payload: "c".into(),
                effects: DropEffectSet::COPY,
            });

            world.resource_mut::<PointerState>().position = Some(Vec2::new(50.0, 50.0));
            world.run_system_once(track_drop_hover).unwrap();
            world.flush();
            assert!(world.get::<DropHovered>(tgt).is_some(), "marked on enter");

            world.resource_mut::<PointerState>().position = Some(Vec2::new(500.0, 500.0));
            world.run_system_once(track_drop_hover).unwrap();
            world.flush();
            assert!(world.get::<DropHovered>(tgt).is_none(), "cleared on leave");
        }
    }

    #[test]
    fn drag_source_round_trip() {
        let src = DragSource::new("hello");
        assert_eq!(src.payload.text().as_deref(), Some("hello"));
        assert_eq!(src.effects, DropEffectSet::COPY);

        let src2 = DragSource::new(vec![std::path::PathBuf::from("/tmp/a")])
            .with_effects(DropEffectSet::MOVE);
        assert!(src2.payload.has(&MimeKind::TextUriList));
        assert_eq!(src2.effects, DropEffectSet::MOVE);
    }
}
