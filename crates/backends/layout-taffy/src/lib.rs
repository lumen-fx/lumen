//! taffy-backed [`lumen_core::traits::LayoutEngine`] impl.
//!
//! The plugin registers systems in [`TickStage::LayoutSync`]:
//!
//! 1. [`sync_viewport`] - copies the viewport into the layout resource
//!    and dirties every root on resize.
//! 2. [`react_to_style_changes`] / [`react_to_children_changes`] /
//!    [`react_to_text_changes`] - `Changed<...>` reactivity hooks (W2.6)
//!    that mark touched entities [`DirtyLayout`] so the engine reflows
//!    without callers having to remember to insert the marker.
//! 3. [`propagate_dirty_layout`] - for each dirty entity, walks the
//!    [`ChildOf`] chain and marks every ancestor [`DirtyLayout`].
//!    Enforces plan invariant 1: dirty propagates to root.
//! 4. [`sync_layout`] - pushes lumen [`Style`] into taffy (diffed
//!    against the previous taffy style - W2.6), computes layout for
//!    every dirty subtree root through the memoising solver in `memo.rs`
//!    so text / image leaves report their intrinsic size via
//!    [`TextShaper::measure`] (W2.5), writes absolute coords into
//!    [`Transform`] for every descendant, then clears [`DirtyLayout`].
//!    The system also drops taffy nodes for entities whose [`Style`]
//!    was removed and whose entity was despawned outright (W2.6
//!    entity-despawn cleanup).
//!
//! ## Why two `NonSend` resources
//!
//! `taffy::TaffyTree` contains `style::CompactLengthInner` which stores
//! a `*const ()` (tagged-pointer optimization for
//! `length`/`percent`/`calc()`). The raw pointer makes the type
//! `!Send + !Sync`. A [`ShaperService`] wraps a font database, which is
//! rarely `Send` either. Both live as `NonSendMut` system params so the
//! layout system is scheduled on the main thread, no `unsafe` needed.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bevy_ecs::resource::IsResource;
use bevy_ecs::system::NonSendMut;
use glam::Vec2;
use lumen_core::components::{
    DirtyLayout, EchoMode, Edges as LumenEdges, FlexAlign as LumenAlign,
    FlexDirection as LumenFlexDir, FlexJustify as LumenJustify, ImageComponent, LayoutDirection,
    Length as LumenLength, LineHeightSpec, Overflow as LumenOverflow, Position as LumenPosition,
    RelayoutBoundary, ResolvedDirection, Style, TextBlockOrigin, TextContent, TextStyle, TextWrap,
    Transform, resolve_line_height, text_block_top,
};
use lumen_core::prelude::*;
use lumen_core::text_model::TextBuffer;
use lumen_text::{
    ShapeOptions, ShapedText, ShaperService, TextShaper, TextViewport, WrapMode, build_shaped_text,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use taffy::prelude::*;

mod memo;

use memo::{Geometry, compute_layout_memoised};

/// Marker for the taffy-backed [`LayoutEngine`].
pub struct TaffyLayout;

impl lumen_core::traits::LayoutEngine for TaffyLayout {}

/// Plugin: install the non-send layout state + register the LayoutSync
/// systems with explicit ordering.
///
/// Text leaves get their intrinsic size from the [`ShaperService`] the
/// app installed, so whoever composes the app picks the shaping backend
/// and layout follows it. A build that installed none falls back to the
/// text-free [`lumen_text::NullShaper`], which measures every run as an
/// empty box.
pub struct TaffyLayoutPlugin;

impl Plugin for TaffyLayoutPlugin {
    fn build(self, app: &mut App) {
        app.world.insert_non_send(LayoutResource::new());
        if app.world.get_non_send::<ShaperService>().is_none() {
            app.world.insert_non_send(ShaperService::default());
        }
        app.world.insert_resource(TextMeasureMemo::default());
        app.add_systems(TickStage::LayoutSync, sync_viewport);
        app.add_systems(
            TickStage::LayoutSync,
            react_to_style_changes.after(sync_viewport),
        );
        app.add_systems(
            TickStage::LayoutSync,
            react_to_children_changes.after(sync_viewport),
        );
        app.add_systems(
            TickStage::LayoutSync,
            react_to_text_changes.after(sync_viewport),
        );
        app.add_systems(
            TickStage::LayoutSync,
            react_to_image_changes.after(sync_viewport),
        );
        // D8: an LTR<->RTL flip re-resolves logical edges + RowReverse, so
        // a direction change must dirty the entity even when nothing else
        // moved. Runs after the resolver stamped fresh values and before
        // propagation so ancestors get marked in the same tick.
        app.add_systems(
            TickStage::LayoutSync,
            react_to_direction_changes
                .after(resolve_layout_direction)
                .before(propagate_dirty_layout),
        );
        app.add_systems(
            TickStage::LayoutSync,
            propagate_dirty_layout
                .after(react_to_style_changes)
                .after(react_to_children_changes)
                .after(react_to_text_changes)
                .after(react_to_image_changes),
        );
        // W5.5 - `sync_layout` reads the per-entity
        // [`ResolvedDirection`] stamped by
        // [`lumen_core::components::resolve_layout_direction`] (also
        // registered in `LayoutSync` by `App::new`). Order
        // `sync_layout` after the resolver so the writing direction
        // is always fresh when the taffy mapper runs.
        app.add_systems(
            TickStage::LayoutSync,
            sync_layout
                .after(propagate_dirty_layout)
                .after(resolve_layout_direction),
        );
        // D4c: shape editable text ONCE per change into a `ShapedText`
        // component the main-world edit / IME systems read next tick. Runs
        // after `sync_layout` so `Transform` (hence the inner box width) is
        // final. Uses the same main-world `ShaperService` as `sync_layout`.
        app.add_systems(TickStage::LayoutSync, update_shaped_text.after(sync_layout));
    }
}

/// D4c producer: shape each editable entity's buffer text once per change
/// into a [`ShapedText`] component (and a companion [`TextViewport`]) that
/// the main-world editing / IME systems read next tick. Keeps `lumen-input`
/// shaper-free: it queries the component, never the shaper.
///
/// The shape is versioned on `TextBuffer.version` folded with the shape
/// inputs (size / inner width+height / wrap / family / weight); an unchanged
/// entity is skipped, and a caching shaper makes any accidental reshape
/// a hit.
///
/// The shaped string is the DISPLAYED one: under a concealed
/// [`EchoMode`] the mask glyphs are shaped, so measuring, hit-testing, and
/// drawing agree on one run. Consumers map their edit-buffer bytes through
/// [`EchoMode::display_offset`] / [`EchoMode::plain_offset`]. Splicing the
/// IME preedit and the placeholder into the same run is deferred to D4-R.
///
/// The producer also publishes [`TextBlockOrigin`], the vertical origin the
/// drawn baseline and the pointer hit test share, evaluated against the
/// SHAPED (soft-wrap aware) line count.
#[allow(clippy::type_complexity)]
pub fn update_shaped_text(
    mut shaper: NonSendMut<ShaperService>,
    mut commands: Commands,
    q: Query<(
        Entity,
        &TextBuffer,
        &Transform,
        Option<&TextStyle>,
        Option<&Style>,
        Option<&EchoMode>,
        Option<&ShapedText>,
    )>,
) {
    for (e, buf, t, ts, style, echo, existing) in &q {
        let echo = echo.copied().unwrap_or_default();
        let ts = ts.cloned().unwrap_or_default();
        let size_px = ts.size_px;
        let (pad_l, pad_r, pad_t, pad_b) = style
            .map(|s| {
                (
                    s.padding.left,
                    s.padding.right,
                    s.padding.top,
                    s.padding.bottom,
                )
            })
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let inner_w = (t.size.x - pad_l - pad_r).max(0.0);
        let inner_h = (t.size.y - pad_t - pad_b).max(size_px);
        let line_h = resolve_line_height(ts.line_height, size_px);
        let version = shape_version_of(ShapeInputs {
            version: buf.version,
            size_px,
            inner_w,
            inner_h,
            wrap: ts.wrap,
            family: ts.family.as_deref(),
            weight: ts.weight,
            echo,
            line_h,
        });
        // Skip entities whose shape inputs are unchanged.
        if existing.is_some_and(|s| s.shape_version == version) {
            continue;
        }
        let wrap = WrapMode::from(ts.wrap);
        let opts = ShapeOptions {
            width: Some(inner_w),
            wrap,
            max_lines: ts.max_lines,
            family: ts.family.clone(),
            weight: ts.weight,
            line_height: Some(line_h),
        };
        let plain = buf.rope.to_string();
        let display = echo.display_string(&plain);
        if let Some(st) = build_shaped_text(&mut **shaper, &display, size_px, opts, version) {
            let stacked = !buf.is_single_line() || st.geometry.line_count() > 1;
            let top = text_block_top(inner_h, line_h, stacked);
            commands.entity(e).insert((
                st,
                TextViewport {
                    inner: Vec2::new(inner_w, inner_h),
                    line_h,
                },
                TextBlockOrigin { top },
            ));
        }
    }
}

/// Every input whose change alters shaped output. Grouped in one struct
/// rather than passed positionally: most of these are `f32`, so a swapped
/// pair would still compile and would silently corrupt the cache key.
struct ShapeInputs<'a> {
    /// [`TextBuffer`] content version.
    version: u64,
    /// Font size in logical pixels.
    size_px: f32,
    /// Content-box width available for wrapping.
    inner_w: f32,
    /// Content-box height.
    inner_h: f32,
    /// Wrap mode.
    wrap: TextWrap,
    /// Font family, or `None` for the default.
    family: Option<&'a str>,
    /// Font weight.
    weight: u16,
    /// [`EchoMode`] - selects which string gets shaped (the masked display
    /// run under a concealed mode), so a mode change must reshape.
    echo: EchoMode,
    /// Resolved line height in logical pixels.
    line_h: f32,
}

/// Fold every shape input into a single scalar the producer compares to
/// decide whether a reshape is needed. Busts on edit, resize, wrap toggle,
/// font, size, weight, or CSS `line-height` change, and on an echo-mode
/// change (it selects which string gets shaped); that is, exactly when the
/// shaped output would differ.
fn shape_version_of(i: ShapeInputs<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    i.version.hash(&mut h);
    i.size_px.to_bits().hash(&mut h);
    i.inner_w.to_bits().hash(&mut h);
    i.inner_h.to_bits().hash(&mut h);
    (i.wrap as u8).hash(&mut h);
    i.family.hash(&mut h);
    i.weight.hash(&mut h);
    (i.echo as u8).hash(&mut h);
    i.line_h.to_bits().hash(&mut h);
    h.finish()
}

/// Context attached to every taffy leaf so the measure callback can
/// look up the originating entity and pick the right intrinsic-size
/// path (W2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeContext {
    /// Plain container - no intrinsic size; taffy's `(0, 0)` fallback
    /// applies when neither dimension is known.
    None,
    /// Text-bearing leaf. The measure callback shapes `TextContent`
    /// through the installed [`TextShaper`] to derive the tight
    /// `(width, height)`.
    Text(Entity),
    /// Image-bearing leaf. The measure callback returns
    /// `ImageComponent.natural_size` (or `(0, 0)` while the image is
    /// still decoding).
    Image(Entity),
}

/// Per-app layout state: taffy tree + entity -> NodeId map + viewport +
/// previous-style cache (for the `set_style` diff in W2.6).
pub struct LayoutResource {
    tree: TaffyTree<NodeContext>,
    map: HashMap<Entity, NodeId>,
    /// Snapshot of the last [`Style`] + resolved [`LayoutDirection`]
    /// pushed into taffy for each entity. `sync_layout` skips
    /// `set_style` when both are identical, avoiding pessimistic
    /// invalidation of taffy's internal cache every dirty tick (bug
    /// 13 in `docs/audits/layout.md`). W5.5 folds the direction into
    /// the key because the same `Style` resolves to a different
    /// `taffy::Style` under LTR vs RTL (logical edges + RowReverse).
    last_style: HashMap<Entity, (Style, LayoutDirection)>,
    /// Last [`NodeContext`] pushed into taffy for each entity. `sync_layout`
    /// skips the `set_node_context` call when the freshly-classified context
    /// is unchanged - `set_node_context` marks the taffy node dirty, so
    /// calling it unconditionally per entity re-invalidated the whole styled
    /// set on every dirty frame.
    last_context: HashMap<Entity, NodeContext>,
    viewport: taffy::Size<AvailableSpace>,
    /// Computed boxes for every node, produced by the solver. taffy owns the
    /// styles and the child lists; it has no public setter for a node's own
    /// layout, so the geometry lives here.
    geometry: Geometry,
    /// Number of solves performed by the most recent non-idle [`sync_layout`]
    /// run. Spec section 17.3 invariant: at most one taffy solve per dirty
    /// root per tick - tests assert against this counter and a
    /// `debug_assert` enforces it in-place.
    solves_last_sync: usize,
    /// Number of nodes those solves computed, counting a node once
    /// per distinct set of layout inputs it was asked about. Linear in the
    /// size of the dirty subtrees; a deep-nesting regression shows up here as
    /// a count that climbs with depth instead of with node count.
    visits_last_sync: usize,
}

impl LayoutResource {
    /// New empty layout state. Viewport defaults to `MaxContent`.
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::new(),
            map: HashMap::new(),
            last_style: HashMap::new(),
            last_context: HashMap::new(),
            viewport: taffy::Size {
                width: AvailableSpace::MaxContent,
                height: AvailableSpace::MaxContent,
            },
            geometry: Geometry::default(),
            solves_last_sync: 0,
            visits_last_sync: 0,
        }
    }

    /// Override the viewport size used as available space for root layouts.
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = taffy::Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
    }

    /// Read absolute Transform for an entity (post-sync), if known.
    pub fn computed_transform(&self, entity: Entity) -> Option<Transform> {
        let node = *self.map.get(&entity)?;
        // A node the solver has not reached yet reads as a zero box, which is
        // what taffy's own per-node layout defaulted to.
        let layout = self.geometry.get(node).unwrap_or_default();
        Some(Transform {
            absolute: Vec2::new(layout.location.x, layout.location.y),
            size: Vec2::new(layout.size.width, layout.size.height),
            // taffy's per-node Layout doesn't expose a baseline today;
            // the measure-fn baseline lives in the cached measure
            // output (`tree.measure_node(...)`). We surface it on the
            // per-entity Transform inside `sync_layout` instead - see
            // the call site that pairs the layout with the measured
            // baseline.
            baseline_y: None,
        })
    }

    /// Total number of live taffy nodes. Used by tests to assert that
    /// `RemovedComponents<Style>` actually frees nodes (W2.6).
    pub fn node_count(&self) -> usize {
        self.map.len()
    }

    /// Taffy solve count of the most recent non-idle [`sync_layout`]
    /// run (idle ticks leave the previous value in place). Spec section 17.3:
    /// N mutations per tick must still collapse into <= 1 solve per
    /// dirty root.
    pub fn solves_last_sync(&self) -> usize {
        self.solves_last_sync
    }

    /// Nodes computed by the most recent non-idle [`sync_layout`] run. Grows
    /// with the number of elements in the dirty subtrees, not with their
    /// nesting depth; `tests/deep_tree.rs` holds that line.
    pub fn visits_last_sync(&self) -> usize {
        self.visits_last_sync
    }
}

impl Default for LayoutResource {
    fn default() -> Self {
        Self::new()
    }
}

/// First system in LayoutSync: pull the viewport from the main-world
/// [`Viewport`] resource into the taffy [`LayoutResource`]. When the
/// viewport changes size, every root entity (no [`ChildOf`]) gets
/// [`DirtyLayout`] so its subtree re-lays out with the new available
/// space.
pub fn sync_viewport(
    mut commands: Commands,
    viewport: Res<Viewport>,
    mut layout: NonSendMut<LayoutResource>,
    roots_q: Query<Entity, (With<Style>, Without<ChildOf>)>,
) {
    let want = (viewport.size.x, viewport.size.y);
    let have = match (layout.viewport.width, layout.viewport.height) {
        (AvailableSpace::Definite(w), AvailableSpace::Definite(h)) => Some((w, h)),
        _ => None,
    };
    if have == Some(want) {
        return;
    }
    layout.set_viewport(want.0, want.1);
    for e in &roots_q {
        commands.entity(e).insert(DirtyLayout);
    }
}

/// W2.6: mark entities whose `Style` changed dirty so the relayout
/// fires without callers having to remember to insert `DirtyLayout`.
/// Without this, runtime style mutation silently shows stale
/// `Transform` (bug 2 in `docs/audits/layout.md`).
///
/// D1 addition: when the changed entity is itself a [`RelayoutBoundary`],
/// its parent is dirtied too. A boundary's size is normally
/// constraint-imposed, so descendant dirt stops there - but a change to
/// the boundary's *own* Style can resize it (e.g. `width: 200px ->
/// 300px`), which the parent must observe. Dirtying the parent makes
/// propagation continue from above, so the boundary is laid out by its
/// parent instead of being pinned to its prior box.
pub fn react_to_style_changes(
    mut commands: Commands,
    changed: Query<(Entity, Option<&ChildOf>), Changed<Style>>,
    boundaries: Query<(), With<RelayoutBoundary>>,
) {
    for (e, child_of) in &changed {
        commands.entity(e).insert(DirtyLayout);
        if boundaries.contains(e)
            && let Some(co) = child_of
        {
            commands.entity(co.parent()).insert(DirtyLayout);
        }
    }
}

/// W2.6: mark entities whose [`Children`] vector changed dirty.
/// `rebuild_taffy_subtree` only runs for dirty roots, so adding /
/// removing children without an explicit dirty mark would leave
/// taffy's `set_children` stale (bug 3 in `docs/audits/layout.md`).
pub fn react_to_children_changes(
    mut commands: Commands,
    changed: Query<Entity, Changed<Children>>,
) {
    for e in &changed {
        commands.entity(e).insert(DirtyLayout);
    }
}

/// W2.6: mark text-bearing leaves dirty when their [`TextContent`] or
/// [`TextStyle`] changed so the measure callback re-shapes the run.
/// This is the bridge between IME / `set-text` script updates and the
/// MeasureFunc-driven sizing introduced by W2.5.
/// D8 extends this with `RemovedComponents<TextContent>`: a text leaf
/// whose `TextContent` was removed must re-measure (intrinsic size
/// drops to zero), which `Changed<TextContent>` alone never fires for.
pub fn react_to_text_changes(
    mut commands: Commands,
    text_changed: Query<Entity, Changed<TextContent>>,
    style_changed: Query<Entity, Changed<TextStyle>>,
    mut text_removed: RemovedComponents<TextContent>,
    alive: Query<(), With<Style>>,
) {
    for e in &text_changed {
        commands.entity(e).insert(DirtyLayout);
    }
    for e in &style_changed {
        commands.entity(e).insert(DirtyLayout);
    }
    for e in text_removed.read() {
        // The removal reader also reports despawned entities; only
        // re-dirty ones that still participate in layout.
        if alive.contains(e) {
            commands.entity(e).insert(DirtyLayout);
        }
    }
}

/// D6: mark image-bearing leaves dirty when their [`ImageComponent`]
/// changed - the decode-complete path stamps `natural_size`, and the
/// measure callback can only pick it up through a relayout.
pub fn react_to_image_changes(
    mut commands: Commands,
    changed: Query<Entity, Changed<ImageComponent>>,
) {
    for e in &changed {
        commands.entity(e).insert(DirtyLayout);
    }
}

/// D8: a resolved writing-direction flip changes the taffy style
/// (logical edges resolve to different physical sides, `Row` becomes
/// `RowReverse`), so the entity must relayout. `resolve_layout_direction`
/// only writes [`ResolvedDirection`] when the value actually changed
/// (D9), so this hook is quiet in steady state.
pub fn react_to_direction_changes(
    mut commands: Commands,
    changed: Query<Entity, (Changed<ResolvedDirection>, With<Style>)>,
) {
    for e in &changed {
        commands.entity(e).insert(DirtyLayout);
    }
}

/// Mark every ancestor of every dirty entity as also dirty, stopping
/// at the nearest [`RelayoutBoundary`]. The boundary itself is marked
/// (so `sync_layout` includes it in its dirty-roots set) but its
/// parent is not touched - that's the whole point of a boundary.
///
/// Without boundaries, a single typed character inside a deeply-nested
/// `<scroll>` would mark every ancestor up to root dirty, then taffy
/// would compute_layout from root. With boundaries, only the scroll
/// container's subtree re-flows. Pattern: Flutter's `_relayoutBoundary`.
pub fn propagate_dirty_layout(
    mut commands: Commands,
    dirty: Query<Entity, With<DirtyLayout>>,
    parents: Query<&ChildOf>,
    boundaries: Query<(), With<RelayoutBoundary>>,
) {
    for entity in &dirty {
        // If the dirty entity is itself a boundary, propagation stops
        // immediately - its own DirtyLayout is enough.
        if boundaries.contains(entity) {
            continue;
        }
        let mut cur = entity;
        while let Ok(child_of) = parents.get(cur) {
            let parent = child_of.parent();
            commands.entity(parent).insert(DirtyLayout);
            // Boundary reached - mark it dirty (already done above
            // via insert) but do not walk past it.
            if boundaries.contains(parent) {
                break;
            }
            cur = parent;
        }
    }
}

/// Push lumen Style -> taffy, recompute dirty subtrees, write
/// Transforms, clear DirtyLayout markers.
///
/// W2.5 wires the solver's measure callback so text / image leaves get
/// intrinsic sizes from [`TextShaper::measure`] /
/// [`ImageComponent::natural_size`]. W2.6 adds:
/// - a per-entity [`Style`] diff so taffy's internal cache is not
///   pessimistically invalidated every dirty tick;
/// - `RemovedComponents<Style>` + map-resident-entity sweep so
///   despawned entities free their taffy node instead of leaking.
#[allow(clippy::too_many_arguments)]
pub fn sync_layout(
    mut commands: Commands,
    mut layout: NonSendMut<LayoutResource>,
    mut shaper: NonSendMut<ShaperService>,
    style_q: Query<(Entity, &Style)>,
    text_q: Query<(&TextContent, Option<&TextStyle>)>,
    image_q: Query<&ImageComponent>,
    children_q: Query<&Children>,
    parent_q: Query<&ChildOf>,
    dirty_q: Query<Entity, With<DirtyLayout>>,
    mut transform_q: Query<&mut Transform>,
    mut removed_style: RemovedComponents<Style>,
    // Liveness probe for the stale-node sweep below. Filtered so a freed
    // entity id that has since been handed to a resource entity still reads
    // as dead and its taffy node gets released.
    all_entities: Query<Entity, Without<IsResource>>,
    // W5.5: per-entity resolved writing direction. Stamped by
    // `resolve_layout_direction` earlier in `LayoutSync`. Absent =>
    // treat as Ltr.
    dir_q: Query<&ResolvedDirection>,
    // Lazy text measurement: scroll containers + their current offset,
    // and the remembered-height memo. Used to skip full shaping of
    // scroll-contained text that lies outside the visible band.
    scroll_q: Query<(Entity, &ScrollOffset), With<Scroll>>,
    mut memo: ResMut<TextMeasureMemo>,
) {
    // Idle-tick fast path: with no dirty entities there is no relayout to
    // do, so we also skip the map/tree bookkeeping below. Entity despawns
    // and child-list edits always dirty an ancestor (via
    // `react_to_children_changes` / `propagate_dirty_layout`), so any tick
    // that frees nodes is a dirty tick - the cleanup can safely ride
    // behind this early-return instead of running its O(map) sweep every
    // frame.
    if dirty_q.is_empty() {
        return;
    }
    let layout = &mut *layout;

    // W2.6 cleanup: free taffy nodes for entities whose `Style` was
    // removed *or* whose entity was despawned outright. Drop both the
    // taffy node and the per-entity style cache. Without this both
    // `layout.map` and `layout.tree` grow unbounded across the
    // application's lifetime (bug 4 in `docs/audits/layout.md`).
    {
        // Removed-component reader: tells us which entities lost their
        // `Style` since last tick (including the despawn-everything
        // path).
        for entity in removed_style.read() {
            if let Some(node) = layout.map.remove(&entity) {
                let _ = layout.tree.remove(node);
                layout.geometry.remove(node);
            }
            layout.last_style.remove(&entity);
            layout.last_context.remove(&entity);
            memo.heights.remove(&entity);
        }
        // Defensive sweep: an entity may be despawned without
        // RemovedComponents firing across tick boundaries (Bevy's
        // RemovedComponents is bounded). Drop any map entry whose
        // entity no longer exists.
        let stale: Vec<Entity> = layout
            .map
            .keys()
            .copied()
            .filter(|e| all_entities.get(*e).is_err())
            .collect();
        for e in stale {
            if let Some(node) = layout.map.remove(&e) {
                let _ = layout.tree.remove(node);
                layout.geometry.remove(node);
            }
            layout.last_style.remove(&e);
            layout.last_context.remove(&e);
            memo.heights.remove(&e);
        }
    }

    // Ensure every styled entity has a taffy node + push current
    // styles (with W2.6 diff: skip `set_style` when the lumen Style is
    // unchanged since the last sync). W5.5 folds the resolved
    // [`LayoutDirection`] into the diff key so an LTR<->RTL flip
    // invalidates the cache.
    for (entity, style) in &style_q {
        let context = classify_node_context(entity, &text_q, &image_q);
        let dir = dir_q
            .get(entity)
            .map(|r| match r.direction() {
                LayoutDirection::Auto => LayoutDirection::Ltr,
                d => d,
            })
            .unwrap_or(LayoutDirection::Ltr);
        let style_changed = layout
            .last_style
            .get(&entity)
            .map(|(prev_style, prev_dir)| prev_style != style || *prev_dir != dir)
            .unwrap_or(true);
        if let Some(&node) = layout.map.get(&entity) {
            if style_changed {
                let taffy_style = lumen_style_to_taffy(style, dir);
                let _ = layout.tree.set_style(node, taffy_style);
                layout.last_style.insert(entity, (style.clone(), dir));
            }
            // Keep context fresh even if Style didn't change - a
            // `TextContent` insertion on a previously-empty leaf
            // promotes `NodeContext::None` -> `NodeContext::Text`. But
            // `set_node_context` marks the node dirty in taffy, so only
            // call it when the classification actually changed.
            if layout.last_context.get(&entity) != Some(&context) {
                let _ = layout.tree.set_node_context(node, Some(context));
                layout.last_context.insert(entity, context);
            }
        } else {
            let taffy_style = lumen_style_to_taffy(style, dir);
            let node = layout
                .tree
                .new_leaf_with_context(taffy_style, context)
                .expect("taffy new_leaf_with_context should not fail");
            layout.map.insert(entity, node);
            layout.last_style.insert(entity, (style.clone(), dir));
            layout.last_context.insert(entity, context);
        }
    }

    // D7 companion: `rebuild_taffy_subtree` no longer calls
    // `set_children` unconditionally (its dirty-marking side effect was
    // what forced re-measures before), so content-driven dirt - text
    // edits, image `natural_size` stamps - must invalidate taffy's
    // cache chain explicitly. `mark_dirty` invalidates the node's own
    // cached measure plus every ancestor's cache while leaving sibling
    // subtrees' caches intact (Qt section 17.1: invalidation is O(depth)
    // flag-setting up the chain, never subtree-wide).
    for entity in &dirty_q {
        if let Some(&node) = layout.map.get(&entity) {
            let _ = layout.tree.mark_dirty(node);
        }
    }

    // Roots inside the dirty set: entities whose parent is not dirty
    // (or who have no parent).
    let dirty_roots: Vec<Entity> = dirty_q
        .iter()
        .filter(|e| match parent_q.get(*e) {
            Ok(p) => dirty_q.get(p.parent()).is_err(),
            Err(_) => true,
        })
        .collect();

    for root in &dirty_roots {
        rebuild_taffy_subtree(layout, *root, &children_q);
    }

    // D1: a dirty root that has a (clean) parent is a RelayoutBoundary
    // subtree recompute. Its size is constraint-imposed - that's what
    // made it a boundary - so the recompute must be solved against the
    // box the parent last gave it, not the viewport, and its own
    // absolute position must not move (only descendants get new
    // geometry). Pin the taffy root style to the prior `Transform.size`
    // for the duration of the solve (percent sizes would otherwise
    // re-resolve against the subtree's available space) and remember
    // the prior absolute origin for the transform write. The original
    // style is restored after the solve. Boundaries whose *own* Style
    // changed never land here: `react_to_style_changes` pierces the
    // boundary by dirtying its parent, so the dirty root moves above.
    struct SubtreePin {
        prior_absolute: Vec2,
        available: taffy::Size<AvailableSpace>,
        original_style: taffy::Style,
    }
    let mut pins: HashMap<Entity, SubtreePin> = HashMap::new();
    for root in &dirty_roots {
        if parent_q.get(*root).is_err() {
            continue;
        }
        // No prior geometry (first layout of a freshly-spawned subtree
        // root) -> fall back to the legacy viewport-based solve.
        let Ok(prior) = transform_q.get(*root) else {
            continue;
        };
        let (prior_absolute, prior_size) = (prior.absolute, prior.size);
        let Some(&node) = layout.map.get(root) else {
            continue;
        };
        let Ok(original_style) = layout.tree.style(node).cloned() else {
            continue;
        };
        let mut pinned = original_style.clone();
        pinned.size = taffy::Size {
            width: taffy::style::Dimension::length(prior_size.x),
            height: taffy::style::Dimension::length(prior_size.y),
        };
        let _ = layout.tree.set_style(node, pinned);
        pins.insert(
            *root,
            SubtreePin {
                prior_absolute,
                available: taffy::Size {
                    width: AvailableSpace::Definite(prior_size.x),
                    height: AvailableSpace::Definite(prior_size.y),
                },
                original_style,
            },
        );
    }

    // Snapshot every entity's measure input before the solve starts. The
    // measure closure captures `&mut shaper` and a borrow of this map; doing
    // the world reads up front keeps the closure's borrow set tiny (no
    // queries inside).
    let measure_inputs: HashMap<Entity, MeasureInput> =
        build_measure_inputs(&style_q, &text_q, &image_q);

    // W5.9 baseline writeback: collect text leaves' first-line baselines
    // during measure. Once the solve returns, `write_transforms_recursive`
    // looks each entity up here and stamps `Transform.baseline_y`.
    // FlexAlign::Baseline + AccessKit text-position reporting consume the
    // result.
    let mut measured_baselines: HashMap<Entity, f32> = HashMap::new();

    // -- Lazy text measurement ----------------------------------------
    // Decide which scroll-contained text leaves are *offscreen* and can
    // therefore be height-estimated instead of shaped. Only the viewport
    // (plus an overscan margin) is shaped exactly, so a huge scrolling
    // document's first layout costs a screenful of shaping, not the whole
    // document. Mirrors how Qt's `QPlainTextEdit`/`QPlainTextDocumentLayout`
    // lays out only visible blocks and derives the scrollbar range from an
    // estimate, GtkTextView validates lines lazily around the visible
    // area, and Slint's ListView instantiates only visible delegates.
    //
    // Correctness: visible content is always shaped exactly (never in
    // `lazy_text`), so rendered pixels - and the golden images - are
    // unchanged. The scroll *extent* is approximate and refines as
    // paragraphs are scrolled into view and memoised.
    let window_h = match layout.viewport.height {
        AvailableSpace::Definite(v) => v,
        _ => 0.0,
    };
    let window_w = match layout.viewport.width {
        AvailableSpace::Definite(v) => v,
        _ => 0.0,
    };
    // Text leaves that can be height-estimated instead of shaped, each
    // paired with the width they wrap at (needed so taffy's *intrinsic*
    // measure passes - which pass `known.width = None` - also skip
    // shaping instead of falling through to the exact path).
    let mut lazy_text: HashMap<Entity, f32> = HashMap::new();
    let scroll_set: HashSet<Entity> = scroll_q.iter().map(|(e, _)| e).collect();
    for (sc, offset) in scroll_q.iter() {
        if !layout.map.contains_key(&sc) {
            continue;
        }
        let mut leaves: Vec<Entity> = Vec::new();
        collect_text_leaves(sc, &children_q, &measure_inputs, &scroll_set, &mut leaves);
        // Below this many leaves, shaping the lot is cheap - skip the
        // machinery so small scrollers behave exactly as before.
        if leaves.len() < LAZY_TEXT_MIN_LEAVES {
            continue;
        }
        let container_tf = transform_q.get(sc).ok().copied();
        let warm = container_tf.map(|t| t.size.y > 0.5).unwrap_or(false);
        // Fallback wrap width for leaves without a prior box: the scroll
        // container's content width, or the window width on the very
        // first pass. Only affects the *estimated* extent, never the
        // shaped-exact visible content.
        let fallback_w = container_tf
            .map(|t| t.size.x)
            .filter(|w| *w > 0.5)
            .unwrap_or(window_w);
        if let Some(ctf) = container_tf.filter(|_| warm) {
            // Warm frame: use the previous layout's geometry to test each
            // leaf's band against the current scroll offset.
            let view_h = ctf.size.y;
            let content_top = ctf.absolute.y;
            let overscan = view_h.max(LAZY_TEXT_MIN_OVERSCAN);
            let band_lo = offset.0.y - overscan;
            let band_hi = offset.0.y + view_h + overscan;
            for e in &leaves {
                // Never-measured leaves (fresh content) stay exact.
                if let Ok(t) = transform_q.get(*e)
                    && t.size.y > 0.5
                {
                    let rel_y = t.absolute.y - content_top;
                    let visible = rel_y + t.size.y >= band_lo && rel_y <= band_hi;
                    if !visible {
                        let w = if t.size.x > 0.5 { t.size.x } else { fallback_w };
                        lazy_text.insert(*e, w);
                    }
                }
            }
        } else {
            // Cold first layout: no geometry yet. Shape a DOM-order prefix
            // certain to cover the viewport (+overscan) and estimate the
            // rest. At scroll offset 0 the prefix *is* the visible content.
            let overscan = window_h.max(LAZY_TEXT_MIN_OVERSCAN);
            let prefix = (((window_h + overscan) / LAZY_TEXT_MIN_LINE_PX).ceil() as usize)
                .max(LAZY_TEXT_MIN_LEAVES);
            for e in leaves.iter().skip(prefix) {
                lazy_text.insert(*e, fallback_w);
            }
        }
    }

    let viewport = layout.viewport;
    layout.solves_last_sync = 0;
    layout.visits_last_sync = 0;
    let shaper: &mut dyn TextShaper = &mut **shaper;
    let lazy_text = &lazy_text;
    let memo: &mut TextMeasureMemo = &mut memo;
    let tree: &mut TaffyTree<NodeContext> = &mut layout.tree;
    let geometry: &mut Geometry = &mut layout.geometry;
    for root in &dirty_roots {
        // Boundary subtree recomputes solve against the boundary's
        // prior box; true roots solve against the viewport.
        let available = pins.get(root).map(|p| p.available).unwrap_or(viewport);
        if let Some(&node) = layout.map.get(root) {
            layout.solves_last_sync += 1;
            layout.visits_last_sync += compute_layout_memoised(
                tree,
                geometry,
                node,
                available,
                |known: taffy::Size<Option<f32>>,
                 _available: taffy::Size<AvailableSpace>,
                 _node_id: NodeId,
                 ctx: Option<NodeContext>,
                 _style: &taffy::Style| {
                    // taffy hands us the parent-resolved dimensions
                    // first; honour them when present so a fixed-size
                    // container always wins over intrinsic size.
                    if let (Some(w), Some(h)) = (known.width, known.height) {
                        return taffy::Size {
                            width: w,
                            height: h,
                        };
                    }
                    match ctx.as_ref() {
                        Some(NodeContext::Text(entity)) => match measure_inputs.get(entity) {
                            Some(MeasureInput::Text(t)) => {
                                let max_width = known.width.or(t.max_width);
                                // Lazy path: a scroll-contained, word-wrapped
                                // paragraph outside the visible band (see the
                                // `lazy_text` set below) gets a cheap height
                                // estimate - or its memoised exact height if
                                // it has been shaped before - instead of a
                                // full shape. Visible text, and
                                // any text without a definite wrap width,
                                // always take the exact path, so on-screen
                                // output is byte-for-byte unchanged.
                                if t.wrap != WrapMode::None
                                    && let Some(&lazy_w) = lazy_text.get(entity)
                                {
                                    // Prefer taffy's resolved width when it
                                    // gives one; else the pre-pass wrap width
                                    // (so the intrinsic passes skip shaping
                                    // too).
                                    // Deliberately not wired to `t.line_height`: this
                                    // estimate only ever backs an off-screen / not-yet-
                                    // shaped paragraph's provisional height, and the memo
                                    // it first checks is itself keyed on (text, width)
                                    // only, not line-height - so threading a CSS override
                                    // through just this literal would still go stale the
                                    // moment the entity scrolls into view and re-memoises.
                                    // The exact path below (taken once the paragraph is
                                    // visible) already carries the real `line_height`.
                                    let w = max_width.unwrap_or(lazy_w);
                                    let h = memo.get(*entity, &t.text, w).unwrap_or_else(|| {
                                        estimate_wrapped_height(&t.text, t.size_px, w)
                                    });
                                    measured_baselines
                                        .insert(*entity, (t.size_px * 1.2 * 0.8).max(0.0));
                                    return taffy::Size {
                                        width: known.width.unwrap_or(w),
                                        height: known.height.unwrap_or(h),
                                    };
                                }
                                let line_h = resolve_line_height(t.line_height, t.size_px);
                                let opts = ShapeOptions {
                                    width: max_width,
                                    wrap: t.wrap,
                                    max_lines: t.max_lines,
                                    family: t.family.clone(),
                                    weight: t.weight,
                                    line_height: Some(line_h),
                                };
                                let (w, h, baseline) =
                                    shaper.measure_with_baseline(&t.text, t.size_px, &opts);
                                // Remember the exact height so that once this
                                // paragraph scrolls back out of view the lazy
                                // path returns the real value - the extent
                                // only ever refines, never snaps shorter.
                                if let Some(mw) = max_width {
                                    memo.record(*entity, &t.text, mw, h);
                                }
                                measured_baselines.insert(*entity, baseline);
                                taffy::Size {
                                    width: known.width.unwrap_or(w),
                                    height: known.height.unwrap_or(h),
                                }
                            }
                            _ => fallback_size(known),
                        },
                        Some(NodeContext::Image(entity)) => match measure_inputs.get(entity) {
                            Some(MeasureInput::Image { natural }) => taffy::Size {
                                width: known.width.unwrap_or(natural.x),
                                height: known.height.unwrap_or(natural.y),
                            },
                            _ => fallback_size(known),
                        },
                        _ => fallback_size(known),
                    }
                },
            );
        }
    }

    // Write absolute coords into Transform for every descendant of
    // every dirty root, regardless of per-entity dirty bit (plan
    // invariant 1). D1: a pinned boundary root keeps its prior
    // absolute position - taffy zeroes the compute-root's location,
    // which must not leak into the boundary's own Transform.
    for root in &dirty_roots {
        let absolute_override = pins.get(root).map(|p| p.prior_absolute);
        let parent_origin = match parent_q.get(*root) {
            Ok(p) => transform_q
                .get(p.parent())
                .map(|t| t.absolute)
                .unwrap_or(Vec2::ZERO),
            Err(_) => Vec2::ZERO,
        };
        write_transforms_recursive(
            layout,
            *root,
            parent_origin,
            absolute_override,
            &children_q,
            &mut transform_q,
            &mut commands,
            &measured_baselines,
        );
    }

    // D1: un-pin the boundary roots so the next ancestor-driven solve
    // sees the author's real style (the parent may resize the boundary
    // later; a lingering pinned size would freeze it).
    for (root, pin) in pins {
        if let Some(&node) = layout.map.get(&root) {
            let _ = layout.tree.set_style(node, pin.original_style);
        }
    }

    for entity in &dirty_q {
        commands.entity(entity).remove::<DirtyLayout>();
    }
}

/// Remembered exact text heights, keyed by entity + a `(text, width)`
/// fingerprint. Backs the lazy-measure path (see [`sync_layout`]): once a
/// scroll-contained paragraph is exact-shaped (because it entered the
/// viewport), its height is memoised here so that on later frames - when
/// it has scrolled back out of the visible band - the offscreen estimate
/// path returns the real height instead of the cheap arithmetic guess.
///
/// This is what makes the approximate scroll extent *monotonically
/// refine* as the document is scrolled, and keeps already-seen content
/// from shifting under later content - the incremental-validation
/// behaviour of `GtkTextView` (`gtk_text_layout_validate`) and the block
/// caching in Qt's `QPlainTextDocumentLayout`.
#[derive(Resource, Default)]
pub struct TextMeasureMemo {
    /// entity -> (text hash, width bucket, exact height in logical px).
    heights: HashMap<Entity, (u64, i32, f32)>,
}

impl TextMeasureMemo {
    /// Width bucket size in logical px - mirrors the shaper's own
    /// `WIDTH_BUCKET` so a memo hit implies a shape-cache hit width.
    const WIDTH_BUCKET: f32 = 25.0;

    fn bucket(width: f32) -> i32 {
        (width / Self::WIDTH_BUCKET).round() as i32
    }

    fn hash_text(text: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        h.finish()
    }

    /// Exact height for `entity` if one was recorded for this exact
    /// `(text, width-bucket)`; `None` on a miss (fresh text, resized).
    fn get(&self, entity: Entity, text: &str, width: f32) -> Option<f32> {
        self.heights.get(&entity).and_then(|(h, b, height)| {
            (*h == Self::hash_text(text) && *b == Self::bucket(width)).then_some(*height)
        })
    }

    fn record(&mut self, entity: Entity, text: &str, width: f32, height: f32) {
        self.heights
            .insert(entity, (Self::hash_text(text), Self::bucket(width), height));
    }
}

/// Minimum text-leaf count under a single scroll container before the
/// lazy-measure path engages. Below this, shaping every leaf is cheap and
/// not worth the bookkeeping - small scrollers keep the exact-everywhere
/// behaviour bit-for-bit.
const LAZY_TEXT_MIN_LEAVES: usize = 48;

/// Overscan (logical px) shaped beyond the visible band on each side, so a
/// small scroll nudge never reveals un-shaped (estimated) content. At
/// least this much even for a tiny viewport.
const LAZY_TEXT_MIN_OVERSCAN: f32 = 400.0;

/// Conservative minimum height (logical px) of one text leaf, used to size
/// the cold-start DOM-order prefix. Smaller than any real single line so
/// the prefix always over-covers the viewport.
const LAZY_TEXT_MIN_LINE_PX: f32 = 14.0;

/// Depth-first collect the text-leaf descendants of `root` in DOM order,
/// not descending into nested scroll containers (each scroll owns its own
/// leaves via its own pass).
fn collect_text_leaves(
    root: Entity,
    children_q: &Query<&Children>,
    measure_inputs: &HashMap<Entity, MeasureInput>,
    scroll_stop: &HashSet<Entity>,
    out: &mut Vec<Entity>,
) {
    let Ok(children) = children_q.get(root) else {
        return;
    };
    for child in children.iter() {
        if matches!(measure_inputs.get(&child), Some(MeasureInput::Text(_))) {
            out.push(child);
        }
        if scroll_stop.contains(&child) {
            continue;
        }
        collect_text_leaves(child, children_q, measure_inputs, scroll_stop, out);
    }
}

/// Cheap arithmetic estimate of a word-wrapped paragraph's height without
/// shaping it. Used only for scroll-contained text that lies outside the
/// visible band (see [`sync_layout`]); visible text is always shaped
/// exactly. The estimate assumes an average glyph advance of
/// [`AVG_ADVANCE_RATIO`]`x size_px` (a good fit for proportional Latin
/// copy) and adds word-boundary slack so it tends to slightly *over*
/// count lines - a conservative extent is preferable to one that snaps
/// shorter when content is scrolled into view.
///
/// Uses the default `size_px * 1.2` line-height rather than a resolved
/// CSS override deliberately: this is a provisional estimate for
/// off-screen text (see the call site's comment), not a measured or
/// painted value, so wiring an override through here would add a plumbing
/// path without fixing the underlying staleness (the memo this backs is
/// keyed on text + width only, not line-height).
fn estimate_wrapped_height(text: &str, size_px: f32, width: f32) -> f32 {
    /// Mean advance as a fraction of the em; tuned against the shaped
    /// height of the English-prose benchmark corpus.
    const AVG_ADVANCE_RATIO: f32 = 0.5;
    /// Fraction of a line's width that word-wrap typically leaves unused
    /// (the last word rarely fills the line exactly).
    const WRAP_FILL: f32 = 0.92;
    let line_height = (size_px * 1.2).max(1.0);
    if text.is_empty() || width <= 0.0 {
        return 0.0;
    }
    let avg_advance = (size_px * AVG_ADVANCE_RATIO).max(1.0);
    let chars_per_line = ((width / avg_advance) * WRAP_FILL).max(1.0);
    let char_count = text.chars().count() as f32;
    // Explicit newlines force line breaks regardless of width.
    let hard_lines = text.lines().count().max(1) as f32;
    let wrapped_lines = (char_count / chars_per_line).ceil().max(1.0);
    wrapped_lines.max(hard_lines) * line_height
}

/// Snapshot of one leaf's intrinsic-size inputs, captured before the
/// taffy measure closure runs (the closure cannot re-query the world
/// because the tree borrow already pins the system params).
#[derive(Clone, Debug)]
enum MeasureInput {
    Text(TextMeasureInput),
    Image { natural: Vec2 },
}

#[derive(Clone, Debug)]
struct TextMeasureInput {
    text: String,
    size_px: f32,
    wrap: WrapMode,
    max_lines: Option<u32>,
    /// Honoured when taffy doesn't pass a `known.width`.
    max_width: Option<f32>,
    /// CSS font-family chain forwarded to the shaper - a family swap
    /// changes metrics, so it must participate in measure.
    family: Option<Arc<str>>,
    /// CSS font-weight (variable fonts change advance widths per weight).
    weight: u16,
    /// CSS `line-height`, resolved against [`Self::size_px`] just before
    /// the exact (non-lazy) measure call below. `None` => the shaper's own
    /// `line-height: normal` fallback.
    line_height: Option<LineHeightSpec>,
}

fn classify_node_context(
    entity: Entity,
    text_q: &Query<(&TextContent, Option<&TextStyle>)>,
    image_q: &Query<&ImageComponent>,
) -> NodeContext {
    if text_q
        .get(entity)
        .map(|(t, _)| !t.0.is_empty())
        .unwrap_or(false)
    {
        return NodeContext::Text(entity);
    }
    if image_q
        .get(entity)
        .map(|img| img.natural_size.is_some())
        .unwrap_or(false)
    {
        return NodeContext::Image(entity);
    }
    NodeContext::None
}

fn build_measure_inputs(
    style_q: &Query<(Entity, &Style)>,
    text_q: &Query<(&TextContent, Option<&TextStyle>)>,
    image_q: &Query<&ImageComponent>,
) -> HashMap<Entity, MeasureInput> {
    let mut out = HashMap::new();
    let default_style = TextStyle::default();
    for (entity, style) in style_q.iter() {
        if let Ok((tc, ts)) = text_q.get(entity)
            && !tc.0.is_empty()
        {
            let ts = ts.unwrap_or(&default_style);
            let wrap = WrapMode::from(ts.wrap);
            // Fall back to the lumen `max_width` style hint when the
            // author authored one; taffy will usually pass
            // `known.width` first anyway, but for absolute-positioned
            // text this is the only hint that reaches the shaper.
            let max_width = match style.max_width {
                LumenLength::Px(v) => Some(v),
                _ => None,
            };
            out.insert(
                entity,
                MeasureInput::Text(TextMeasureInput {
                    text: tc.0.clone(),
                    size_px: ts.size_px,
                    wrap,
                    max_lines: ts.max_lines,
                    max_width,
                    family: ts.family.clone(),
                    weight: ts.weight,
                    line_height: ts.line_height,
                }),
            );
            continue;
        }
        if let Ok(img) = image_q.get(entity)
            && let Some(natural) = img.natural_size
        {
            out.insert(entity, MeasureInput::Image { natural });
        }
    }
    out
}

fn fallback_size(known: taffy::Size<Option<f32>>) -> taffy::Size<f32> {
    taffy::Size {
        width: known.width.unwrap_or(0.0),
        height: known.height.unwrap_or(0.0),
    }
}

fn rebuild_taffy_subtree(
    layout: &mut LayoutResource,
    entity: Entity,
    children_q: &Query<&Children>,
) {
    let Some(&parent_node) = layout.map.get(&entity) else {
        return;
    };
    let child_ids: Vec<NodeId> = match children_q.get(entity) {
        Ok(children) => children
            .iter()
            .filter_map(|c| layout.map.get(&c).copied())
            .collect(),
        Err(_) => Vec::new(),
    };
    // D7: `set_children` marks the node dirty in taffy even when the
    // list is identical, pessimistically invalidating taffy's caches
    // for the whole subtree on every relayout. Diff first - rewire only
    // when the child list actually changed.
    let current: Vec<NodeId> = layout.tree.children(parent_node).unwrap_or_default();
    if current != child_ids {
        let _ = layout.tree.set_children(parent_node, &child_ids);
    }

    if let Ok(children) = children_q.get(entity) {
        let owned: Vec<Entity> = children.iter().collect();
        for child in owned {
            rebuild_taffy_subtree(layout, child, children_q);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_transforms_recursive(
    layout: &LayoutResource,
    entity: Entity,
    parent_origin: Vec2,
    absolute_override: Option<Vec2>,
    children_q: &Query<&Children>,
    transform_q: &mut Query<&mut Transform>,
    commands: &mut Commands,
    measured_baselines: &HashMap<Entity, f32>,
) {
    let Some(&node) = layout.map.get(&entity) else {
        return;
    };
    let layout_result = layout.geometry.get(node).unwrap_or_default();
    let local = Vec2::new(layout_result.location.x, layout_result.location.y);
    let size = Vec2::new(layout_result.size.width, layout_result.size.height);
    // D1: boundary subtree recomputes keep the root's prior absolute
    // position (taffy zeroes the compute-root's location).
    let absolute = absolute_override.unwrap_or(parent_origin + local);

    // W5.9: prefer the freshly-measured baseline (text leaf reshaped
    // this tick); fall back to the prior tick's baseline when taffy
    // skipped re-measuring (text unchanged) so non-text leaves don't
    // need an entry in the map.
    let baseline_y = measured_baselines
        .get(&entity)
        .copied()
        .or_else(|| transform_q.get(entity).ok().and_then(|t| t.baseline_y));
    let next = Transform {
        absolute,
        size,
        baseline_y,
    };
    if let Ok(mut t) = transform_q.get_mut(entity) {
        // set_if_neq semantics: a solve that lands on the same box must
        // not raise `Changed<Transform>` - downstream change detection
        // (FrameDirty roll-up, extract upserts, a11y sync) treats every
        // Transform write as "moved", so an unconditional `*t = next`
        // here turned every layout pass into a full repaint.
        if *t != next {
            *t = next;
        }
    } else {
        commands.entity(entity).insert(next);
    }

    if let Ok(children) = children_q.get(entity) {
        let owned: Vec<Entity> = children.iter().collect();
        for child in owned {
            write_transforms_recursive(
                layout,
                child,
                absolute,
                None,
                children_q,
                transform_q,
                commands,
                measured_baselines,
            );
        }
    }
}

fn lumen_style_to_taffy(s: &Style, dir: LayoutDirection) -> taffy::Style {
    use taffy::style::{
        FlexDirection as TaffyFlexDir, JustifyContent, Overflow as TaffyOverflow,
        Position as TaffyPosition,
    };

    let width = lumen_length_to_taffy_dim(s.width);
    let height = lumen_length_to_taffy_dim(s.height);

    // Spec section 0: an explicit fixed size wins over intrinsic content, full
    // stop (Slint: "any element with a specified width and height has a
    // fixed size in a layout"; Qt: an explicit size makes the hint
    // irrelevant). Under raw CSS-flex semantics that's not what taffy
    // does - `min-size: auto` floors the item at its content's
    // min-content size and the default `flex-shrink: 1` lets siblings
    // squeeze it - so a `width: 200px` element could still end up wider
    // (long child) or narrower (crowded row) than 200. Pinning
    // `min_size` to the explicit length kills both leaks: the content
    // floor no longer applies and shrink can't go below the authored
    // size. This is also what makes fixed-px [`RelayoutBoundary`]
    // entities sound: their laid-out size genuinely cannot change when
    // their content changes (D1). An author-supplied `min-width` /
    // `min-height` still takes precedence.
    let min_width =
        if matches!(s.min_width, LumenLength::Auto) && matches!(s.width, LumenLength::Px(_)) {
            width
        } else {
            lumen_length_to_taffy_dim(s.min_width)
        };
    let min_height =
        if matches!(s.min_height, LumenLength::Auto) && matches!(s.height, LumenLength::Px(_)) {
            height
        } else {
            lumen_length_to_taffy_dim(s.min_height)
        };

    // W5.5: `Row` under Rtl resolves to RowReverse so the layout
    // backend mirrors the inline axis automatically.
    let flex_direction = match s.flex_direction.resolved(dir) {
        LumenFlexDir::Row => TaffyFlexDir::Row,
        LumenFlexDir::Column => TaffyFlexDir::Column,
        LumenFlexDir::RowReverse => TaffyFlexDir::RowReverse,
        LumenFlexDir::ColumnReverse => TaffyFlexDir::ColumnReverse,
    };

    let align_items = Some(lumen_align_to_taffy(s.align));

    let justify_content = Some(match s.justify {
        LumenJustify::Start => JustifyContent::FLEX_START,
        LumenJustify::End => JustifyContent::FLEX_END,
        LumenJustify::Center => JustifyContent::CENTER,
        LumenJustify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        LumenJustify::SpaceAround => JustifyContent::SPACE_AROUND,
        LumenJustify::SpaceEvenly => JustifyContent::SPACE_EVENLY,
    });

    let position = match s.position {
        LumenPosition::Relative => TaffyPosition::Relative,
        LumenPosition::Absolute => TaffyPosition::Absolute,
    };

    let map_overflow = |o: LumenOverflow| match o {
        LumenOverflow::Visible => TaffyOverflow::Visible,
        LumenOverflow::Hidden => TaffyOverflow::Hidden,
        LumenOverflow::Scroll => TaffyOverflow::Scroll,
    };

    // W5.5: resolve every Edges into physical sides once before
    // handing the taffy mapper f32s. Authors who only used `left` /
    // `right` get the same numbers (the logical fields default
    // `None`); authors who used `inline_start` get LTR/RTL-aware
    // physical sides.
    let padding = s.padding.resolved(dir);
    let margin = s.margin.resolved(dir);
    let inset = s.inset.resolved(dir);
    let border = s.border.resolved(dir);

    // CSS box model: border widths consume space. `box_sizing` picks
    // whether explicit sizes include padding + border (BorderBox - the
    // Lumen UA default and taffy's own default) or content only.
    let box_sizing = match s.box_sizing {
        lumen_core::components::BoxSizing::BorderBox => taffy::style::BoxSizing::BorderBox,
        lumen_core::components::BoxSizing::ContentBox => taffy::style::BoxSizing::ContentBox,
    };

    // Flex completeness: shrink / basis / wrap / align-content map 1:1.
    let flex_wrap = match s.flex_wrap {
        lumen_core::components::FlexWrap::NoWrap => taffy::style::FlexWrap::NoWrap,
        lumen_core::components::FlexWrap::Wrap => taffy::style::FlexWrap::Wrap,
        lumen_core::components::FlexWrap::WrapReverse => taffy::style::FlexWrap::WrapReverse,
    };
    let align_content = s.align_content.map(|a| {
        use lumen_core::components::AlignContent as L;
        use taffy::style::AlignContent as T;
        match a {
            L::Start => T::FLEX_START,
            L::End => T::FLEX_END,
            L::Center => T::CENTER,
            L::Stretch => T::STRETCH,
            L::SpaceBetween => T::SPACE_BETWEEN,
            L::SpaceAround => T::SPACE_AROUND,
            L::SpaceEvenly => T::SPACE_EVENLY,
        }
    });

    // W5.9: pick the taffy display mode + (for Grid) lower the lumen
    // grid template into taffy's `GridTrackVec`. `Display::None`
    // suppresses layout entirely; `Display::Grid` swaps the algorithm
    // (taffy's `grid` feature is on by default in 0.10).
    let display = match s.display {
        lumen_core::components::Display::Flex => Display::Flex,
        lumen_core::components::Display::Grid => Display::Grid,
        lumen_core::components::Display::None => Display::None,
    };
    let (grid_template_rows, grid_template_columns) = match &s.grid_template {
        Some(t) => (
            t.rows.iter().map(lumen_track_to_taffy_template).collect(),
            t.columns
                .iter()
                .map(lumen_track_to_taffy_template)
                .collect(),
        ),
        None => (Default::default(), Default::default()),
    };
    let grid_row = lumen_grid_line_to_taffy(s.grid_row);
    let grid_column = lumen_grid_line_to_taffy(s.grid_column);

    // W5.9: per-axis gap. CSS `gap` is `<row> <column>`; taffy's
    // `Size { width, height }` for gap means the *spacing along the
    // axis*, where `width` = horizontal spacing (between adjacent
    // columns) and `height` = vertical spacing (between adjacent
    // rows). Map row->height, column->width. Percent gaps resolve
    // against the container's content-box size (taffy handles this
    // once handed a `LengthPercentage::percent`).
    let gap_one = |px: f32, pct: Option<f32>| match pct {
        Some(p) => taffy::style::LengthPercentage::percent(p / 100.0),
        None => taffy::style::LengthPercentage::length(px),
    };
    let gap = taffy::Size {
        width: gap_one(s.gap.column, s.gap.column_pct),
        height: gap_one(s.gap.row, s.gap.row_pct),
    };

    let align_self = s.align_self.map(lumen_align_to_taffy);
    let justify_items = s.justify_items.map(lumen_align_to_taffy);
    let justify_self = s.justify_self.map(lumen_align_to_taffy);

    taffy::Style {
        display,
        flex_direction,
        align_items,
        align_self,
        justify_items,
        justify_self,
        justify_content,
        position,
        inset: edges_to_lpa(inset),
        size: taffy::Size { width, height },
        min_size: taffy::Size {
            width: min_width,
            height: min_height,
        },
        max_size: taffy::Size {
            width: lumen_length_to_taffy_dim(s.max_width),
            height: lumen_length_to_taffy_dim(s.max_height),
        },
        aspect_ratio: s.aspect_ratio,
        overflow: taffy::Point {
            x: map_overflow(s.overflow_x),
            y: map_overflow(s.overflow_y),
        },
        padding: edges_to_lp(padding),
        margin: edges_to_lpa(margin),
        border: edges_to_lp(border),
        box_sizing,
        gap,
        flex_grow: s.grow,
        flex_shrink: s.shrink,
        flex_basis: lumen_length_to_taffy_dim(s.basis),
        flex_wrap,
        align_content,
        grid_template_rows,
        grid_template_columns,
        grid_row,
        grid_column,
        ..taffy::Style::DEFAULT
    }
}

/// W5.9: map lumen's [`FlexAlign`] (= `align-items` / `align-self` /
/// `justify-items` / `justify-self`) to taffy's `AlignItems`. The
/// `Baseline` variant maps to taffy's native `Baseline` so mixed-size
/// text rows align by first-line baseline.
fn lumen_align_to_taffy(a: LumenAlign) -> taffy::style::AlignItems {
    use taffy::style::AlignItems;
    match a {
        LumenAlign::Start => AlignItems::FLEX_START,
        LumenAlign::End => AlignItems::FLEX_END,
        LumenAlign::Center => AlignItems::CENTER,
        LumenAlign::Stretch => AlignItems::STRETCH,
        LumenAlign::Baseline => AlignItems::BASELINE,
    }
}

/// W5.9: lower a lumen [`TrackSize`] into taffy's
/// [`taffy::style::GridTemplateComponent`]. taffy's default
/// `CheapCloneStr` parameter is `String` in std builds; we never
/// emit named lines, so the parameter is irrelevant beyond type
/// inference.
fn lumen_track_to_taffy_template(
    t: &lumen_core::components::TrackSize,
) -> taffy::style::GridTemplateComponent<String> {
    use lumen_core::components::TrackSize;
    use taffy::style::{
        GridTemplateComponent, MaxTrackSizingFunction as Max, MinTrackSizingFunction as Min,
        TrackSizingFunction,
    };
    match t {
        TrackSize::Fixed(px) => GridTemplateComponent::Single(TrackSizingFunction {
            min: Min::length(*px),
            max: Max::length(*px),
        }),
        TrackSize::Auto => GridTemplateComponent::Single(TrackSizingFunction {
            min: Min::auto(),
            max: Max::auto(),
        }),
        TrackSize::Fr(f) => GridTemplateComponent::Single(TrackSizingFunction {
            min: Min::auto(),
            max: Max::fr(*f),
        }),
        TrackSize::MinContent => GridTemplateComponent::Single(TrackSizingFunction {
            min: Min::min_content(),
            max: Max::min_content(),
        }),
        TrackSize::MaxContent => GridTemplateComponent::Single(TrackSizingFunction {
            min: Min::max_content(),
            max: Max::max_content(),
        }),
        TrackSize::MinMax(min, max) => {
            let min_fn = match min.as_ref() {
                TrackSize::Fixed(px) => Min::length(*px),
                TrackSize::Auto => Min::auto(),
                TrackSize::MinContent => Min::min_content(),
                TrackSize::MaxContent => Min::max_content(),
                // CSS Grid disallows `fr` in the `min` slot of
                // `minmax()`; treat as auto to keep the mapper total.
                TrackSize::Fr(_) | TrackSize::MinMax(_, _) => Min::auto(),
            };
            let max_fn = match max.as_ref() {
                TrackSize::Fixed(px) => Max::length(*px),
                TrackSize::Auto => Max::auto(),
                TrackSize::MinContent => Max::min_content(),
                TrackSize::MaxContent => Max::max_content(),
                TrackSize::Fr(f) => Max::fr(*f),
                // Nested minmax not in CSS L1; collapse to auto.
                TrackSize::MinMax(_, _) => Max::auto(),
            };
            GridTemplateComponent::Single(TrackSizingFunction {
                min: min_fn,
                max: max_fn,
            })
        }
    }
}

/// W5.9: map a lumen `(start, end)` grid line pair into taffy's
/// `Line<GridPlacement>`. Convention: 0 means auto-placement;
/// positive integers are 1-based explicit lines; negative integers
/// are counted from the end (CSS Grid).
fn lumen_grid_line_to_taffy(
    (start, end): (i16, i16),
) -> taffy::geometry::Line<taffy::style::GridPlacement<String>> {
    use taffy::style::GridPlacement;
    let one = |n: i16| -> GridPlacement<String> {
        if n == 0 {
            GridPlacement::Auto
        } else {
            GridPlacement::from_line_index(n)
        }
    };
    taffy::geometry::Line {
        start: one(start),
        end: one(end),
    }
}

fn lumen_length_to_taffy_dim(l: LumenLength) -> taffy::style::Dimension {
    use taffy::style::Dimension;
    match l {
        LumenLength::Auto => Dimension::auto(),
        LumenLength::Px(v) => Dimension::length(v),
        LumenLength::Percent(v) => Dimension::percent(v / 100.0),
    }
}

fn edges_to_lp(e: LumenEdges) -> taffy::Rect<taffy::style::LengthPercentage> {
    use taffy::style::LengthPercentage;
    // CSS percent units win over the px slot for a side when authored
    // (padding/margin percentages resolve against the containing
    // block's width - taffy implements that resolution).
    fn one(px: f32, pct: Option<f32>) -> LengthPercentage {
        match pct {
            Some(p) => LengthPercentage::percent(p / 100.0),
            None => LengthPercentage::length(px),
        }
    }
    taffy::Rect {
        left: one(e.left, e.pct_left),
        right: one(e.right, e.pct_right),
        top: one(e.top, e.pct_top),
        bottom: one(e.bottom, e.pct_bottom),
    }
}

fn edges_to_lpa(e: LumenEdges) -> taffy::Rect<taffy::style::LengthPercentageAuto> {
    use taffy::style::LengthPercentageAuto;
    fn one(v: f32, pct: Option<f32>) -> LengthPercentageAuto {
        if let Some(p) = pct {
            // CSS percent unit (margin % resolves against the
            // containing block's width, per CSS 2.1 section 8.3).
            LengthPercentageAuto::percent(p / 100.0)
        } else if v.is_nan() {
            LengthPercentageAuto::auto()
        } else if !v.is_finite() {
            // W2.6 belt-and-braces: should `f32::INFINITY` ever sneak
            // into an edge from outside the audited callsites, clamp
            // to `f32::MAX / 2` so taffy's arithmetic doesn't poison
            // downstream comparisons (bug 5 in `docs/audits/layout.md`).
            LengthPercentageAuto::length(f32::MAX / 2.0)
        } else {
            LengthPercentageAuto::length(v)
        }
    }
    taffy::Rect {
        left: one(e.left, e.pct_left),
        right: one(e.right, e.pct_right),
        top: one(e.top, e.pct_top),
        bottom: one(e.bottom, e.pct_bottom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::hierarchy::ChildOf;
    use bevy_ecs::world::World;

    /// Build a chain `root -> boundary -> leaf`, mark the leaf dirty,
    /// run propagation. The boundary's parent (root) must not receive
    /// `DirtyLayout` - propagation stops at the boundary.
    #[test]
    fn dirty_propagation_stops_at_boundary() {
        let mut world = World::new();
        let root = world.spawn(Style::default()).id();
        let boundary = world
            .spawn((Style::default(), RelayoutBoundary, ChildOf(root)))
            .id();
        let leaf = world
            .spawn((Style::default(), DirtyLayout, ChildOf(boundary)))
            .id();

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(propagate_dirty_layout);
        schedule.run(&mut world);

        assert!(
            world.get::<DirtyLayout>(boundary).is_some(),
            "boundary itself receives DirtyLayout"
        );
        assert!(
            world.get::<DirtyLayout>(root).is_none(),
            "root above the boundary must stay clean"
        );
        // leaf still dirty, of course.
        assert!(world.get::<DirtyLayout>(leaf).is_some());
    }

    /// Without a boundary, dirty propagates all the way to root -
    /// preserves the legacy behavior for entities that don't carry
    /// RelayoutBoundary.
    #[test]
    fn dirty_propagation_reaches_root_without_boundary() {
        let mut world = World::new();
        let root = world.spawn(Style::default()).id();
        let mid = world.spawn((Style::default(), ChildOf(root))).id();
        let _leaf = world
            .spawn((Style::default(), DirtyLayout, ChildOf(mid)))
            .id();

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(propagate_dirty_layout);
        schedule.run(&mut world);

        assert!(world.get::<DirtyLayout>(mid).is_some());
        assert!(world.get::<DirtyLayout>(root).is_some());
    }

    /// W2.6: mutating `Style` flips `Changed<Style>` and
    /// `react_to_style_changes` must mark the entity dirty.
    #[test]
    fn changed_style_marks_dirty() {
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(react_to_style_changes);
        // First run primes change-detection.
        schedule.run(&mut world);
        world.entity_mut(e).remove::<DirtyLayout>();
        // Mutate the Style: this bumps change-detection.
        world.get_mut::<Style>(e).unwrap().width = LumenLength::Px(42.0);
        schedule.run(&mut world);
        assert!(
            world.get::<DirtyLayout>(e).is_some(),
            "Changed<Style> must insert DirtyLayout"
        );
    }

    /// W2.6: mutating `TextContent` triggers `Changed<TextContent>`
    /// and `react_to_text_changes` re-dirties the leaf so the
    /// MeasureFunc re-shapes.
    #[test]
    fn changed_text_marks_dirty() {
        let mut world = World::new();
        let e = world
            .spawn((Style::default(), TextContent("hi".into())))
            .id();
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(react_to_text_changes);
        schedule.run(&mut world);
        world.entity_mut(e).remove::<DirtyLayout>();
        world.get_mut::<TextContent>(e).unwrap().0.push('!');
        schedule.run(&mut world);
        assert!(
            world.get::<DirtyLayout>(e).is_some(),
            "Changed<TextContent> must insert DirtyLayout"
        );
    }

    /// W2.6 belt-and-braces: `edges_to_lpa` clamps `f32::INFINITY`
    /// instead of forwarding it to taffy (bug 5 in
    /// `docs/audits/layout.md`).
    #[test]
    fn edges_to_lpa_clamps_inf() {
        let edges = LumenEdges {
            top: 0.0,
            right: 0.0,
            bottom: f32::INFINITY,
            left: 0.0,
            ..Default::default()
        };
        let r = edges_to_lpa(edges);
        let s = format!("{:?}", r.bottom);
        assert!(
            !s.contains("inf") && !s.contains("Inf"),
            "bottom edge must not be Infinity: {s}"
        );
    }

    /// W5.5: the Style->taffy mapping resolves logical edges to
    /// physical sides under the entity's resolved direction. With
    /// `padding.inline_start = Some(8)` under RTL, the resulting
    /// taffy padding should have its `right` set to 8 (not `left`).
    #[test]
    fn lumen_style_to_taffy_resolves_logical_padding_under_rtl() {
        let style = Style {
            padding: LumenEdges {
                inline_start: Some(8.0),
                ..LumenEdges::default()
            },
            ..Style::default()
        };
        let t_ltr = lumen_style_to_taffy(&style, LayoutDirection::Ltr);
        let t_rtl = lumen_style_to_taffy(&style, LayoutDirection::Rtl);
        // taffy::LengthPercentage stores its scalar in a tagged
        // pointer; `into_raw().value()` is the public accessor.
        assert_eq!(t_ltr.padding.left.into_raw().value(), 8.0);
        assert_eq!(t_ltr.padding.right.into_raw().value(), 0.0);
        assert_eq!(t_rtl.padding.right.into_raw().value(), 8.0);
        assert_eq!(t_rtl.padding.left.into_raw().value(), 0.0);
    }

    /// W5.5: `FlexDirection::Row` under RTL maps to taffy's
    /// `RowReverse` so siblings stack right->left.
    #[test]
    fn lumen_style_to_taffy_row_under_rtl_becomes_row_reverse() {
        let style = Style {
            flex_direction: LumenFlexDir::Row,
            ..Style::default()
        };
        let t = lumen_style_to_taffy(&style, LayoutDirection::Rtl);
        assert_eq!(t.flex_direction, taffy::style::FlexDirection::RowReverse);
        let t_ltr = lumen_style_to_taffy(&style, LayoutDirection::Ltr);
        assert_eq!(t_ltr.flex_direction, taffy::style::FlexDirection::Row);
    }

    /// W5.9: per-axis gap. CSS `gap: <row> <column>` lands in
    /// `Style.gap.row` + `Style.gap.column`; the taffy mapping
    /// puts row -> height, column -> width (taffy's `Size.width` is
    /// the inline-axis spacing between adjacent items).
    #[test]
    fn lumen_style_to_taffy_per_axis_gap_maps_row_to_height() {
        let style = Style {
            gap: lumen_core::components::Gap {
                row: 8.0,
                column: 16.0,
                ..Default::default()
            },
            ..Style::default()
        };
        let t = lumen_style_to_taffy(&style, LayoutDirection::Ltr);
        assert_eq!(t.gap.width.into_raw().value(), 16.0);
        assert_eq!(t.gap.height.into_raw().value(), 8.0);
    }

    /// W5.9: `display: grid` lowers to `taffy::Display::Grid`.
    #[test]
    fn lumen_style_to_taffy_display_grid_maps() {
        let style = Style {
            display: lumen_core::components::Display::Grid,
            ..Style::default()
        };
        let t = lumen_style_to_taffy(&style, LayoutDirection::Ltr);
        assert_eq!(t.display, taffy::style::Display::Grid);
    }

    /// W5.9: `align-items: baseline` lowers to taffy's native
    /// `Baseline` so mixed-size inline runs align by first-line
    /// baseline.
    #[test]
    fn lumen_style_to_taffy_baseline_align_maps() {
        let style = Style {
            align: lumen_core::components::FlexAlign::Baseline,
            ..Style::default()
        };
        let t = lumen_style_to_taffy(&style, LayoutDirection::Ltr);
        assert_eq!(t.align_items, Some(taffy::style::AlignItems::BASELINE));
    }

    /// W5.9: end-to-end check. A 300-px-wide grid with two `1fr 2fr`
    /// columns should split into 100 + 200 (1/3 + 2/3). Builds a
    /// minimal world, runs `sync_layout`, asserts the resulting
    /// child widths.
    #[test]
    fn grid_template_columns_1fr_2fr_splits_one_third_two_thirds() {
        use lumen_core::components::*;

        // Build the world manually so we don't need the full App
        // plugin (which pulls a winit + render world we don't want
        // in this unit test).
        let mut world = bevy_ecs::world::World::new();
        world.insert_non_send(LayoutResource::new());
        world.insert_non_send(ShaperService::default());
        world.insert_resource(TextMeasureMemo::default());
        world.insert_resource(Viewport {
            size: glam::Vec2::new(300.0, 100.0),
            ..Viewport::default()
        });

        // Parent grid with `1fr 2fr` columns.
        let parent = world
            .spawn((
                Style {
                    display: Display::Grid,
                    width: Length::Px(300.0),
                    height: Length::Px(100.0),
                    grid_template: Some(GridTemplate {
                        rows: vec![TrackSize::Fr(1.0)],
                        columns: vec![TrackSize::Fr(1.0), TrackSize::Fr(2.0)],
                    }),
                    ..Style::default()
                },
                DirtyLayout,
            ))
            .id();
        let child_a = world.spawn(Style::default()).id();
        let child_b = world.spawn(Style::default()).id();
        world.entity_mut(parent).add_children(&[child_a, child_b]);

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(sync_viewport);
        schedule.add_systems(sync_layout.after(sync_viewport));
        // Resolve direction so the layout backend has the writing
        // direction stamped on every entity.
        schedule.add_systems(resolve_layout_direction.before(sync_layout));
        schedule.run(&mut world);

        let layout = world.non_send::<LayoutResource>();
        let t_parent = layout.computed_transform(parent).expect("parent");
        let t_a = layout.computed_transform(child_a).expect("a");
        let t_b = layout.computed_transform(child_b).expect("b");
        assert_eq!(t_parent.size.x, 300.0);
        // 1fr vs 2fr split -> 1/3 vs 2/3.
        assert!(
            (t_a.size.x - 100.0).abs() < 0.5,
            "child A width should be ~100, got {}",
            t_a.size.x
        );
        assert!(
            (t_b.size.x - 200.0).abs() < 0.5,
            "child B width should be ~200, got {}",
            t_b.size.x
        );
    }

    /// W5.9: `align-items: baseline` on a mixed-size flex row places
    /// items so their first-line baselines line up. Without a
    /// MeasureFunc-reported baseline, taffy falls back to
    /// `padding-top` aligned - which still satisfies the
    /// invariant that `Style.align = Baseline` != `Style.align =
    /// Stretch`. This test exercises the Style->taffy mapping path
    /// + the layout pass without asserting on subpixel baseline
    ///   offsets (font metrics differ by system; we
    ///   only check that the items don't stretch).
    #[test]
    fn baseline_align_on_flex_row_does_not_stretch() {
        use lumen_core::components::*;
        let mut world = bevy_ecs::world::World::new();
        world.insert_non_send(LayoutResource::new());
        world.insert_non_send(ShaperService::default());
        world.insert_resource(TextMeasureMemo::default());
        world.insert_resource(Viewport {
            size: glam::Vec2::new(400.0, 100.0),
            ..Viewport::default()
        });
        let parent = world
            .spawn((
                Style {
                    display: Display::Flex,
                    align: FlexAlign::Baseline,
                    width: Length::Px(400.0),
                    height: Length::Px(100.0),
                    ..Style::default()
                },
                DirtyLayout,
            ))
            .id();
        let small = world
            .spawn(Style {
                width: Length::Px(50.0),
                height: Length::Px(20.0),
                ..Style::default()
            })
            .id();
        let big = world
            .spawn(Style {
                width: Length::Px(50.0),
                height: Length::Px(40.0),
                ..Style::default()
            })
            .id();
        world.entity_mut(parent).add_children(&[small, big]);
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(sync_viewport);
        schedule.add_systems(resolve_layout_direction.before(sync_layout));
        schedule.add_systems(sync_layout.after(sync_viewport));
        schedule.run(&mut world);
        let layout = world.non_send::<LayoutResource>();
        let t_small = layout.computed_transform(small).expect("small");
        let t_big = layout.computed_transform(big).expect("big");
        // Baseline align must not stretch either child to the
        // container height.
        assert!(
            t_small.size.y <= 25.0,
            "small not stretched: {}",
            t_small.size.y
        );
        assert!(t_big.size.y <= 45.0, "big not stretched: {}", t_big.size.y);
    }
}

#[cfg(test)]
mod css_flex_wave_tests {
    //! R-css-flex: taffy mapping + compute behaviour for borders,
    //! box-sizing, flex-shrink/basis/wrap/align-content, and percent
    //! padding/margin/gap.
    use super::*;
    use lumen_core::components::{
        AlignContent as LumenAlignContent, BoxSizing as LumenBoxSizing, Edges as CoreEdges,
        FlexWrap as LumenFlexWrap, Length as CoreLength,
    };

    fn solve(
        parent: &Style,
        children: &[Style],
        avail: Vec2,
    ) -> (taffy::Layout, Vec<taffy::Layout>) {
        let mut tree: TaffyTree<()> = TaffyTree::new();
        let kids: Vec<taffy::NodeId> = children
            .iter()
            .map(|s| {
                tree.new_leaf(lumen_style_to_taffy(s, LayoutDirection::Ltr))
                    .expect("leaf")
            })
            .collect();
        let root = tree
            .new_with_children(lumen_style_to_taffy(parent, LayoutDirection::Ltr), &kids)
            .expect("root");
        tree.compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(avail.x),
                height: taffy::AvailableSpace::Definite(avail.y),
            },
        )
        .expect("compute");
        let root_l = *tree.layout(root).expect("root layout");
        let kid_l = kids
            .iter()
            .map(|k| *tree.layout(*k).expect("kid layout"))
            .collect();
        (root_l, kid_l)
    }

    /// CSS box model: a 10px border consumes space inside a border-box
    /// sized element - a grow child fills the remaining 80x80 at (10,10).
    #[test]
    fn border_consumes_space_under_border_box() {
        let parent = Style {
            width: CoreLength::Px(100.0),
            height: CoreLength::Px(100.0),
            border: CoreEdges::all(10.0),
            ..Style::default()
        };
        let child = Style {
            grow: 1.0,
            ..Style::default()
        };
        let (root, kids) = solve(&parent, &[child], Vec2::new(200.0, 200.0));
        assert_eq!(root.size.width, 100.0, "border-box keeps the authored size");
        assert_eq!(kids[0].size.width, 80.0);
        assert_eq!(kids[0].size.height, 80.0);
        assert_eq!(kids[0].location.x, 10.0);
        assert_eq!(kids[0].location.y, 10.0);
    }

    /// `box-sizing: content-box` opts back into CSS-spec initial
    /// sizing: authored width covers the content only, border expands
    /// the box outward.
    #[test]
    fn content_box_expands_by_border() {
        let parent = Style {
            width: CoreLength::Px(100.0),
            height: CoreLength::Px(100.0),
            border: CoreEdges::all(10.0),
            box_sizing: LumenBoxSizing::ContentBox,
            ..Style::default()
        };
        let (root, _) = solve(&parent, &[], Vec2::new(400.0, 400.0));
        assert_eq!(root.size.width, 120.0);
        assert_eq!(root.size.height, 120.0);
    }

    /// flex-shrink: two 80px-basis items in a 100px row shrink to 50
    /// each when shrinkable (explicit `min-width: 0`, CSS's standard
    /// unlock); with `flex-shrink: 0` they overflow at 80 each.
    #[test]
    fn flex_shrink_squeezes_and_zero_prevents() {
        let row = Style {
            width: CoreLength::Px(100.0),
            height: CoreLength::Px(50.0),
            ..Style::default()
        };
        let shrinkable = Style {
            basis: CoreLength::Px(80.0),
            min_width: CoreLength::Px(0.0),
            ..Style::default()
        };
        let (_, kids) = solve(
            &row,
            &[shrinkable.clone(), shrinkable.clone()],
            Vec2::new(100.0, 50.0),
        );
        assert_eq!(kids[0].size.width, 50.0);
        assert_eq!(kids[1].size.width, 50.0);

        let rigid = Style {
            shrink: 0.0,
            ..shrinkable
        };
        let (_, kids) = solve(&row, &[rigid.clone(), rigid], Vec2::new(100.0, 50.0));
        assert_eq!(kids[0].size.width, 80.0, "flex-shrink: 0 must not squeeze");
    }

    /// flex-wrap + align-content: three 40px items in a 100px row wrap
    /// onto a second line; `align-content: start` packs lines at the top.
    #[test]
    fn flex_wrap_wraps_and_align_content_applies() {
        let row = Style {
            width: CoreLength::Px(100.0),
            height: CoreLength::Px(100.0),
            flex_wrap: LumenFlexWrap::Wrap,
            align_content: Some(LumenAlignContent::Start),
            ..Style::default()
        };
        let item = Style {
            width: CoreLength::Px(40.0),
            height: CoreLength::Px(20.0),
            ..Style::default()
        };
        let (_, kids) = solve(
            &row,
            &[item.clone(), item.clone(), item.clone()],
            Vec2::new(100.0, 100.0),
        );
        assert_eq!(kids[0].location.y, 0.0);
        assert_eq!(kids[1].location.y, 0.0);
        assert!(
            kids[2].location.y >= 20.0,
            "third item wraps to the second line, got y={}",
            kids[2].location.y
        );
    }

    /// Percent padding resolves against the containing block width (CSS
    /// 2.1 section 8.4: even for the vertical sides).
    #[test]
    fn percent_padding_resolves_against_parent_width() {
        let parent = Style {
            width: CoreLength::Px(200.0),
            height: CoreLength::Px(100.0),
            padding: CoreEdges {
                pct_left: Some(10.0),
                pct_top: Some(10.0),
                ..CoreEdges::default()
            },
            ..Style::default()
        };
        let child = Style {
            width: CoreLength::Px(20.0),
            height: CoreLength::Px(20.0),
            ..Style::default()
        };
        let (_, kids) = solve(&parent, &[child], Vec2::new(200.0, 100.0));
        assert_eq!(kids[0].location.x, 20.0, "10% of 200px parent width");
        assert_eq!(
            kids[0].location.y, 20.0,
            "vertical percent padding also resolves against WIDTH per CSS"
        );
    }

    /// Percent column-gap resolves against the container's width.
    #[test]
    fn percent_gap_resolves_against_container() {
        let parent = Style {
            width: CoreLength::Px(200.0),
            height: CoreLength::Px(50.0),
            gap: lumen_core::components::Gap {
                column_pct: Some(10.0),
                ..Default::default()
            },
            ..Style::default()
        };
        let item = Style {
            width: CoreLength::Px(50.0),
            height: CoreLength::Px(20.0),
            ..Style::default()
        };
        let (_, kids) = solve(&parent, &[item.clone(), item], Vec2::new(200.0, 50.0));
        assert_eq!(kids[0].location.x, 0.0);
        assert_eq!(
            kids[1].location.x, 70.0,
            "50px item + 20px (10% of 200) gap"
        );
    }

    /// Percent margin resolves against the containing block width.
    #[test]
    fn percent_margin_resolves_against_parent_width() {
        let parent = Style {
            width: CoreLength::Px(200.0),
            height: CoreLength::Px(100.0),
            ..Style::default()
        };
        let child = Style {
            width: CoreLength::Px(20.0),
            height: CoreLength::Px(20.0),
            margin: CoreEdges {
                pct_left: Some(25.0),
                ..CoreEdges::default()
            },
            ..Style::default()
        };
        let (_, kids) = solve(&parent, &[child], Vec2::new(200.0, 100.0));
        assert_eq!(kids[0].location.x, 50.0, "25% of 200px parent width");
    }

    /// Mapping details that don't need a solve: shrink default is the
    /// CSS initial 1.0, basis maps to flex-basis, border edges map to
    /// taffy's border rect.
    #[test]
    fn style_mapping_round_trip() {
        let s = Style {
            shrink: 0.25,
            basis: CoreLength::Percent(50.0),
            border: CoreEdges {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
                ..CoreEdges::default()
            },
            ..Style::default()
        };
        let t = lumen_style_to_taffy(&s, LayoutDirection::Ltr);
        assert_eq!(t.flex_shrink, 0.25);
        assert_eq!(t.flex_basis, taffy::style::Dimension::percent(0.5));
        assert_eq!(t.border.top, taffy::style::LengthPercentage::length(1.0));
        assert_eq!(t.border.right, taffy::style::LengthPercentage::length(2.0));
        assert_eq!(t.border.bottom, taffy::style::LengthPercentage::length(3.0));
        assert_eq!(t.border.left, taffy::style::LengthPercentage::length(4.0));
        // Defaults: CSS initial values.
        let d = lumen_style_to_taffy(&Style::default(), LayoutDirection::Ltr);
        assert_eq!(d.flex_shrink, 1.0);
        assert_eq!(d.flex_wrap, taffy::style::FlexWrap::NoWrap);
        assert_eq!(d.box_sizing, taffy::style::BoxSizing::BorderBox);
        assert!(d.align_content.is_none());
    }
}
