//! Drag-and-drop abstractions for Lumen.
//!
//! Reuses the [`MimePayload`] type from [`lumen-os-mime`] so a single
//! payload travels across both the clipboard and the DnD pipeline -
//! mirrors `QMimeData` / `GdkContentProvider`.
//!
//! In-app drag from a [`DragSource`] entity to a `lumen-core::DropTarget`
//! is now wired end to end: [`begin_in_app_drag`] opens an [`ActiveDrag`]
//! session on the drag-gesture threshold (fed by
//! `lumen-primitives::drag`), [`track_drop_hover`] follows the pointer
//! marking the hovered target with `DropHovered`, and
//! [`finish_in_app_drag`] hit-tests the release point and emits
//! [`DropAccepted`]; [`DragHovered`] and [`DragEnded`] report the steps in
//! between, which the script layer turns into the DOM's `dragenter`,
//! `dragover`, `dragleave` and `dragend`. Files the platform drops on the
//! window arrive as `lumen-core::FileDropped` and become a [`DropAccepted`]
//! carrying `text/uri-list` ([`translate_file_drops_to_payload`]). A drag
//! that leaves the window for another app (`NSDraggingSession`,
//! `DoDragDrop`, `wl_data_source`/Xdnd) is not supported: winit exposes no
//! source API.
//!
//! Mirrors: the source-publishes-mime -> target-negotiates-effect ->
//! drop-carries-source+data shape of `QDrag`/`QMimeData`/`dropEvent`, and
//! the HTML5 `dragstart`/`dragenter`/`drop` + `dataTransfer` event model,
//! so the markup `drag-payload` / `drag-type` / `drop-target` / `accept`
//! surface is the same on a page, where the browser runs the drag.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::{DropHovered, DropTarget, Transform};
use lumen_core::input::{
    DragEndEvent, DragStartEvent, FileDropped, PointerState, ScrollOffset, ancestor_scroll,
};
use lumen_os_mime::{MimeKind, MimePayload};

pub use lumen_os_mime as mime;

/// Marker / data component for an entity that can be the *source* of a
/// drag-and-drop. The markup's `drag-payload` (with `drag-type` naming its
/// kind) inserts one; an app can insert one itself.
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
    /// MIME kinds this target accepts, each a type or a `type/*` wildcard
    /// ([`MimeKind::matches`]). Empty means "accept any".
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

    /// The filter an `accept` attribute names: a comma-separated list of
    /// types and `type/*` wildcards, spelled as an HTML `<input accept>`.
    /// An empty list accepts any kind.
    pub fn from_list(list: &str) -> Self {
        Self::only(
            list.split(',')
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
                .map(MimeKind::from),
        )
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

    /// True when this accept-set permits a drag advertising `kinds`.
    ///
    /// A hovering drag exposes the kinds it carries before it exposes the
    /// payload itself (HTML5 `DataTransfer.types` during `dragenter`), which
    /// is what lets a target answer "would this drop be taken" while the
    /// pointer is still moving. [`DropAccept::accepts`] is the same question
    /// asked of a payload that has arrived.
    pub fn accepts_kinds<'a>(&self, kinds: impl IntoIterator<Item = &'a MimeKind>) -> bool {
        if self.kinds.is_empty() {
            return true;
        }
        kinds
            .into_iter()
            .any(|k| self.kinds.iter().any(|want| k.matches(want.as_str())))
    }

    /// True when this accept-set permits the supplied payload.
    pub fn accepts(&self, payload: &MimePayload) -> bool {
        self.accepts_kinds(payload.kinds.iter().map(|(k, _)| k))
    }
}

/// Emitted when a drag starts on a [`DragSource`] entity (in-app
/// gesture). A drag out of the window to another app is not supported; see
/// the crate docs.
#[derive(Message, Clone, Debug)]
pub struct DragStarted {
    /// Source entity carrying [`DragSource`].
    pub source: Entity,
    /// Payload the source published.
    pub payload: MimePayload,
    /// Where the pointer was when the drag began, in window logical pixels.
    pub position: Vec2,
}

/// Emitted while an in-app drag moves: the accepting drop target under the
/// pointer, if any, and whether it is new.
///
/// Written on every tick the pointer moved or the target changed, so a
/// reader sees the web's `dragenter` (`entered`), `dragover` (`over`) and
/// `dragleave` (`left`) in one message.
#[derive(Message, Clone, Debug)]
pub struct DragHovered {
    /// The element the drag started on.
    pub source: Entity,
    /// The accepting drop target under the pointer.
    pub over: Option<Entity>,
    /// True when `over` was not under the pointer the step before.
    pub entered: bool,
    /// The target the pointer left this step.
    pub left: Option<Entity>,
    /// Where the pointer is, in window logical pixels.
    pub position: Vec2,
}

/// Emitted when an in-app drag ends, dropped or not, after the
/// [`DropAccepted`] of a drop.
#[derive(Message, Clone, Debug)]
pub struct DragEnded {
    /// The element the drag started on.
    pub source: Entity,
    /// The target it was dropped on; `None` for a drag released over
    /// nothing that takes it.
    pub target: Option<Entity>,
    /// The effect of the drop, [`DropEffect::None`] when nothing took it.
    pub effect: DropEffect,
    /// Where the pointer was released, in window logical pixels.
    pub position: Vec2,
}

/// Emitted when the user releases the drag on a hovered drop target
/// (in-app gesture), or the platform drops a file on one.
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
    /// Where the pointer was at the drop, in window logical pixels.
    pub position: Vec2,
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
/// This is the inbound half, the drag the platform hands the window. The
/// in-app half - a drag that starts on a [`DragSource`] in the same app -
/// runs through [`begin_in_app_drag`], [`track_drop_hover`] and
/// [`finish_in_app_drag`], which [`DndPlugin`] registers alongside this one.
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
            payload,
            effect,
            position: ev.position,
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
        let off = ancestor_scroll(e, parents, scrolls);
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
/// drag gesture crosses the threshold on a [`DragSource`] entity or on an
/// element inside one. Mirrors
/// `QDrag::exec` beginning a drag with the source's `QMimeData` /
/// HTML5 `dragstart` populating `dataTransfer`.
pub fn begin_in_app_drag(
    mut starts: MessageReader<DragStartEvent>,
    sources: Query<&DragSource>,
    parents: Query<&ChildOf>,
    mut active: ResMut<ActiveDrag>,
    mut out: MessageWriter<DragStarted>,
) {
    for ev in starts.read() {
        // The gesture starts on the element the press went down on, which
        // may sit inside the source (a card's title): the source is the
        // nearest one at or above it, as `draggable` works in a browser.
        let found = std::iter::once(ev.entity)
            .chain(parents.iter_ancestors(ev.entity))
            .find_map(|e| sources.get(e).ok().map(|src| (e, src)));
        if let Some((source, src)) = found {
            active.0 = Some(ActiveDragData {
                source,
                payload: src.payload.clone(),
                effects: src.effects,
            });
            out.write(DragStarted {
                source,
                payload: src.payload.clone(),
                position: ev.position,
            });
        }
    }
}

/// While a drag is active, keep [`DropHovered`] on the topmost accepting
/// [`DropTarget`] under the pointer (and off every other target), and
/// report each step with a [`DragHovered`]. Mirrors HTML5 `dragenter` /
/// `dragover` / `dragleave` so `:drag-over` styling follows the cursor.
#[allow(clippy::too_many_arguments)] // ECS system: each arg is a query/param
pub fn track_drop_hover(
    mut commands: Commands,
    active: Res<ActiveDrag>,
    pointer: Res<PointerState>,
    targets: Query<(Entity, &Transform, Option<&DropAccept>), With<DropTarget>>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    hovered: Query<Entity, With<DropHovered>>,
    mut out: MessageWriter<DragHovered>,
    mut last: Local<Option<Vec2>>,
) {
    let want = match (&active.0, pointer.position) {
        (Some(d), Some(pos)) => {
            topmost_target(pos, &d.payload, d.effects, &targets, &parents, &scrolls).map(|(e, _)| e)
        }
        _ => None,
    };
    let mut left = None;
    for e in &hovered {
        if Some(e) != want {
            commands.entity(e).remove::<DropHovered>();
            left = Some(e);
        }
    }
    let entered = want.is_some_and(|e| !hovered.contains(e));
    if let Some(e) = want
        && entered
    {
        commands.entity(e).insert(DropHovered);
    }
    let Some(drag) = &active.0 else {
        *last = None;
        return;
    };
    let Some(position) = pointer.position else {
        return;
    };
    // A drag that holds still over the same target says nothing new.
    if *last == Some(position) && !entered && left.is_none() {
        return;
    }
    *last = Some(position);
    out.write(DragHovered {
        source: drag.source,
        over: want,
        entered,
        left,
        position,
    });
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
    mut ended: MessageWriter<DragEnded>,
) {
    for ev in ends.read() {
        let Some(data) = active.0.clone() else {
            continue;
        };
        // The gesture may have started inside the source.
        if ev.entity != data.source && !parents.iter_ancestors(ev.entity).any(|a| a == data.source)
        {
            continue;
        }
        let dropped = topmost_target(
            ev.position,
            &data.payload,
            data.effects,
            &targets,
            &parents,
            &scrolls,
        );
        if let Some((target, effect)) = dropped {
            out.write(DropAccepted {
                source: Some(data.source),
                target,
                payload: data.payload.clone(),
                effect,
                position: ev.position,
            });
        }
        ended.write(DragEnded {
            source: data.source,
            target: dropped.map(|(t, _)| t),
            effect: dropped.map_or(DropEffect::None, |(_, e)| e),
            position: ev.position,
        });
        active.0 = None;
        for e in &hovered {
            commands.entity(e).remove::<DropHovered>();
        }
    }
}

/// The systems [`DndPlugin`] runs. Whoever reads this tick's drag messages
/// orders itself after it.
#[derive(bevy_ecs::schedule::SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct DndSet;

/// Plugin: registers DnD messages + both the platform file-drop -> payload
/// translator and the in-app drag-gesture -> drop pipeline.
pub struct DndPlugin;

impl lumen_core::app::Plugin for DndPlugin {
    fn build(self, app: &mut lumen_core::app::App) {
        app.add_message::<DragStarted>();
        app.add_message::<DragHovered>();
        app.add_message::<DragEnded>();
        app.add_message::<DropAccepted>();
        app.world.insert_resource(ActiveDrag::default());
        app.add_systems(
            lumen_core::tick::TickStage::Systems,
            translate_file_drops_to_payload.in_set(DndSet),
        );
        // In-app gesture path: open the session, resolve the drop, then
        // refresh the hover marker (so a just-finished drag clears its
        // `:drag-over` in the same tick).
        app.add_systems(
            lumen_core::tick::TickStage::Systems,
            (begin_in_app_drag, finish_in_app_drag, track_drop_hover)
                .chain()
                .in_set(DndSet),
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

    /// A hovering drag names what it carries before it hands the data over,
    /// which is the question a `:drag-over` marker is the answer to.
    #[test]
    fn drop_accept_filters_a_hovering_drag_by_the_kinds_it_advertises() {
        let a = DropAccept::only([MimeKind::TextUriList]);
        let both = [&MimeKind::TextPlain, &MimeKind::TextUriList];
        assert!(a.accepts_kinds([&MimeKind::TextUriList]));
        assert!(a.accepts_kinds(both));
        assert!(!a.accepts_kinds([&MimeKind::TextPlain]));
        assert!(!a.accepts_kinds([]), "a drag naming nothing is not a match");
        assert!(
            DropAccept::any().accepts_kinds([]),
            "a target filtering nothing takes it anyway"
        );
    }

    #[test]
    fn drop_accept_from_list_reads_an_input_accept_list() {
        let a = DropAccept::from_list(" image/* ,text/uri-list,, ");
        assert_eq!(a.kinds.len(), 2);
        assert!(a.accepts_kinds([&MimeKind::ImageJpeg]));
        assert!(a.accepts_kinds([&MimeKind::TextUriList]));
        assert!(!a.accepts_kinds([&MimeKind::TextPlain]));
        assert!(
            DropAccept::from_list("").kinds.is_empty(),
            "empty takes any"
        );
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
            world.init_resource::<Messages<DragHovered>>();
            world.init_resource::<Messages<DragEnded>>();
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
            assert_eq!(started[0].position, Vec2::new(20.0, 10.0));
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
            assert_eq!(dropped[0].position, Vec2::new(150.0, 50.0));
            assert!(world.resource::<ActiveDrag>().0.is_none(), "session closed");
            let ended: Vec<DragEnded> = world
                .resource_mut::<Messages<DragEnded>>()
                .drain()
                .collect();
            assert_eq!(ended.len(), 1, "one DragEnded after the drop");
            assert_eq!(ended[0].target, Some(tgt));
            assert_eq!(ended[0].effect, DropEffect::Copy);
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

            let mut system = IntoSystem::into_system(track_drop_hover);
            system.initialize(&mut world);
            let mut step = |world: &mut World, at: Vec2| {
                world.resource_mut::<PointerState>().position = Some(at);
                system.run((), world).unwrap();
                world.flush();
                world
                    .resource_mut::<Messages<DragHovered>>()
                    .drain()
                    .collect::<Vec<_>>()
            };

            let hover = step(&mut world, Vec2::new(50.0, 50.0));
            assert!(world.get::<DropHovered>(tgt).is_some(), "marked on enter");
            assert_eq!(hover.len(), 1);
            assert_eq!((hover[0].over, hover[0].entered), (Some(tgt), true));

            let hover = step(&mut world, Vec2::new(60.0, 50.0));
            assert_eq!(hover.len(), 1, "a move over the target is a dragover");
            assert_eq!((hover[0].over, hover[0].entered), (Some(tgt), false));
            assert!(
                step(&mut world, Vec2::new(60.0, 50.0)).is_empty(),
                "holding still says nothing"
            );

            let hover = step(&mut world, Vec2::new(500.0, 500.0));
            assert!(world.get::<DropHovered>(tgt).is_none(), "cleared on leave");
            assert_eq!((hover[0].over, hover[0].left), (None, Some(tgt)));
        }

        /// An `accept` list takes any kind it names, wildcards included, and
        /// lights `:drag-over` only for a drag carrying one of them.
        #[test]
        fn accept_list_filters_the_hover() {
            let mut world = setup();
            let src = world.spawn(DragSource::new("c")).id();
            let images = target(
                &mut world,
                Vec2::ZERO,
                Vec2::new(100.0, 100.0),
                DropAccept::from_list("image/*, application/x-asset"),
            );
            let mut payload = MimePayload::from("c");
            world.resource_mut::<ActiveDrag>().0 = Some(ActiveDragData {
                source: src,
                payload: payload.clone(),
                effects: DropEffectSet::COPY,
            });
            world.resource_mut::<PointerState>().position = Some(Vec2::new(50.0, 50.0));
            world.run_system_once(track_drop_hover).unwrap();
            world.flush();
            assert!(
                world.get::<DropHovered>(images).is_none(),
                "text is refused"
            );

            payload.set(MimeKind::from("application/x-asset"), b"a".to_vec());
            world
                .resource_mut::<ActiveDrag>()
                .0
                .as_mut()
                .unwrap()
                .payload = payload;
            world.run_system_once(track_drop_hover).unwrap();
            world.flush();
            assert!(
                world.get::<DropHovered>(images).is_some(),
                "the asset is taken"
            );
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
