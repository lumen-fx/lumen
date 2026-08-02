//! Low-level introspection read surface (design 4.7): post-layout
//! geometry, full computed style + cascade provenance, typed component
//! reads, tree serialization, and global runtime state.
//!
//! Like the rest of the DOM read side, every getter reads a process-global
//! snapshot the runtime publishes each tick, so a `Send + Sync` script host
//! with no `&World` at call time can inspect the live app. Geometry,
//! component maps, pointer / frame state, and the signal set are published
//! by the runtime's introspection system; `computed_style()` and
//! `matched_rules()` re-run the cascade host-side over the same
//! stylesheet + tree the phase-2 detail snapshot already carries.
//!
//! `computed_style()`, `matched_rules()`, `dump_tree()`, and
//! `signals_all()` walk the whole tree or re-run the cascade; they are
//! inspection calls, not a per-frame hot path.

use bevy_ecs::entity::Entity;
use lumen_core::node::{DomIndex, NodeHandle, dom_index_snapshot};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::node_query::{
    NodeRef, ancestor_infos, attributes_of_handle, dom_details_snapshot, resolved_attributes,
};

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// Post-layout box, `getBoundingClientRect`-class. `x` / `y` are local to
/// the parent; `client_x` / `client_y` are window coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeRect {
    /// Local x (relative to the parent's origin).
    pub x: f32,
    /// Local y.
    pub y: f32,
    /// Box width.
    pub width: f32,
    /// Box height.
    pub height: f32,
    /// Window-space x (client coordinates).
    pub client_x: f32,
    /// Window-space y.
    pub client_y: f32,
}

/// Scroll offsets and their travel limits for a scroll container.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeScroll {
    /// Current horizontal offset.
    pub x: f32,
    /// Current vertical offset.
    pub y: f32,
    /// Maximum horizontal offset (content overflow).
    pub max_x: f32,
    /// Maximum vertical offset.
    pub max_y: f32,
}

/// Per-node geometry published into the snapshot each tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeGeometry {
    /// Border-box rect.
    pub rect: NodeRect,
    /// Content-box rect (inner box minus padding + border).
    pub content_rect: NodeRect,
    /// Scroll offsets / limits.
    pub scroll: NodeScroll,
    /// Effective visibility after cull / `Visible(false)` / `display:none`.
    pub visible: bool,
    /// Resolved stacking order.
    pub z_index: i32,
}

/// Pointer state: window position, pressed buttons, live modifiers.
#[derive(Debug, Clone, Copy, Default)]
pub struct PointerSnapshot {
    /// Window-space x.
    pub x: f32,
    /// Window-space y.
    pub y: f32,
    /// `true` while the pointer is inside the window.
    pub inside: bool,
    /// Bit 0 set while the primary button is held.
    pub buttons: u32,
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
    /// Super / Command held.
    pub super_: bool,
}

/// Per-frame counters for perf-aware scripts.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInfo {
    /// Monotonic tick counter.
    pub frame: u64,
    /// Milliseconds since the previous published frame.
    pub dt_ms: f64,
    /// Number of layout-dirty elements observed this tick.
    pub dirty_count: u64,
}

/// One matched stylesheet rule with cascade provenance, for
/// `matched_rules()`.
#[derive(Debug, Clone)]
pub struct MatchedRuleView {
    /// The matched selector, serialized to CSS text.
    pub selector: String,
    /// Selectors-4 specificity `(a, b, c)`.
    pub specificity: (u32, u32, u32),
    /// `"author"` or `"user-agent"`.
    pub source: String,
    /// Source order within the stylesheet.
    pub source_order: usize,
    /// The rule's declarations as `(property, value)` pairs.
    pub declarations: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Published snapshots
// ---------------------------------------------------------------------------

/// A component's field map: `(field, value)` pairs.
pub type ComponentFields = Vec<(String, String)>;
/// Per-node component field maps: `(component_name, fields)` in registry
/// order, keyed by packed handle.
pub type ComponentMaps = HashMap<u64, Vec<(String, ComponentFields)>>;

/// Geometry + component maps published each tick, keyed by packed handle.
#[derive(Default)]
pub struct IntrospectSnapshot {
    geometry: HashMap<u64, NodeGeometry>,
    components: ComponentMaps,
    known_components: Vec<String>,
    pointer: PointerSnapshot,
    frame: FrameInfo,
    signals: Vec<(String, String)>,
}

impl IntrospectSnapshot {
    /// Assemble a snapshot from the runtime's per-tick reads.
    pub fn new(
        geometry: HashMap<u64, NodeGeometry>,
        components: ComponentMaps,
        known_components: Vec<String>,
        pointer: PointerSnapshot,
        frame: FrameInfo,
        signals: Vec<(String, String)>,
    ) -> Self {
        Self {
            geometry,
            components,
            known_components,
            pointer,
            frame,
            signals,
        }
    }
}

static INTROSPECT: OnceLock<RwLock<Arc<IntrospectSnapshot>>> = OnceLock::new();

fn cell() -> &'static RwLock<Arc<IntrospectSnapshot>> {
    INTROSPECT.get_or_init(|| RwLock::new(Arc::new(IntrospectSnapshot::default())))
}

/// Publish a freshly-built introspection snapshot for cross-thread readers.
pub fn publish_introspection(snapshot: IntrospectSnapshot) {
    if let Ok(mut g) = cell().write() {
        *g = Arc::new(snapshot);
    }
}

fn snapshot() -> Arc<IntrospectSnapshot> {
    cell()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| Arc::new(IntrospectSnapshot::default()))
}

// ---------------------------------------------------------------------------
// Geometry getters
// ---------------------------------------------------------------------------

/// `n.rect()`: post-layout border-box, local + client.
pub fn node_rect(handle: u64) -> Option<NodeRect> {
    snapshot().geometry.get(&handle).map(|g| g.rect)
}

/// `n.content_rect()`: inner box minus padding + border.
pub fn node_content_rect(handle: u64) -> Option<NodeRect> {
    snapshot().geometry.get(&handle).map(|g| g.content_rect)
}

/// `n.scroll()`: scroll offsets and their limits.
pub fn node_scroll(handle: u64) -> Option<NodeScroll> {
    snapshot().geometry.get(&handle).map(|g| g.scroll)
}

/// `n.is_visible()`: effective visibility. A handle absent from the
/// snapshot (despawned) reads `false`.
pub fn node_is_visible(handle: u64) -> bool {
    snapshot()
        .geometry
        .get(&handle)
        .map(|g| g.visible)
        .unwrap_or(false)
}

/// `n.z_index()`: resolved stacking order (`0` when unset / unknown).
pub fn node_z_index(handle: u64) -> i32 {
    snapshot()
        .geometry
        .get(&handle)
        .map(|g| g.z_index)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Computed style + provenance
// ---------------------------------------------------------------------------

/// `n.computed_style()`: every modeled CSS property the cascade resolved,
/// as `(name, value)` pairs. Re-runs the cascade host-side; an inspection
/// call.
pub fn node_computed_style_map(handle: u64) -> Vec<(String, String)> {
    lumen_ir::css::computed_style_map(&resolved_attributes(handle))
}

/// `n.matched_rules()`: the stylesheet rules that matched this node with
/// their cascade provenance, ascending in cascade order (last wins).
pub fn node_matched_rules(handle: u64) -> Vec<MatchedRuleView> {
    let Some(entity) = NodeRef(handle).entity() else {
        return Vec::new();
    };
    let details = dom_details_snapshot();
    let Some(sheet) = details.sheet() else {
        return Vec::new();
    };
    let index = dom_index_snapshot();
    let Some(rec) = index.record(entity) else {
        return Vec::new();
    };
    let subject =
        lumen_ir::css::AncestorInfo::new(rec.tag.clone(), rec.classes.clone(), rec.id.clone())
            .with_position(rec.child_index, rec.sibling_count);
    let ancestors = ancestor_infos(&index, entity);
    let has_children = !rec.children.is_empty();
    let text = crate::node_query::node_text(handle);
    lumen_ir::css::matched_rules_for(
        &subject,
        sheet,
        &details.media(),
        &ancestors,
        has_children,
        text.as_deref(),
    )
    .into_iter()
    .map(|m| MatchedRuleView {
        selector: m.selector,
        specificity: (m.specificity.a, m.specificity.b, m.specificity.c),
        source: match m.origin {
            lumen_ir::css::Origin::UserAgent => "user-agent".to_string(),
            lumen_ir::css::Origin::Author => "author".to_string(),
        },
        source_order: m.source_order,
        declarations: m.declarations,
    })
    .collect()
}

/// `n.inline_style()`: the `element.style` override map.
pub fn node_inline_style(handle: u64) -> Vec<(String, String)> {
    dom_details_snapshot().inline_style_of(handle)
}

/// `n.attrs()`: the full attribute map, `id` / `class` (from the index),
/// `text`, then every generic attribute.
pub fn node_attrs(handle: u64) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let index = dom_index_snapshot();
    if let Some(entity) = NodeRef(handle).entity() {
        if let Some(rec) = index.record(entity) {
            if let Some(id) = &rec.id {
                out.push(("id".to_string(), id.clone()));
            }
            if !rec.classes.is_empty() {
                out.push(("class".to_string(), rec.classes.join(" ")));
            }
        }
    }
    if let Some(text) = crate::node_query::node_text(handle) {
        out.push(("text".to_string(), text));
    }
    out.extend(attributes_of_handle(handle));
    out
}

/// `n.classes()`: the full class list.
pub fn node_classes(handle: u64) -> Vec<String> {
    let Some(entity) = NodeRef(handle).entity() else {
        return Vec::new();
    };
    dom_index_snapshot()
        .record(entity)
        .map(|r| r.classes.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// ECS introspection
// ---------------------------------------------------------------------------

/// `n.entity_id()`: the raw `(index, generation)` for debugging / handle
/// round-trip. `None` for a null / reserved-token / invalid handle.
pub fn node_entity_id(handle: u64) -> Option<(u32, u32)> {
    let h = NodeHandle::unpack(handle)?;
    // Entity::to_bits packs the index in the low half, generation in the
    // high half (see NodeHandle::pack).
    let bits = h.entity.to_bits();
    Some((bits as u32, (bits >> 32) as u32))
}

/// `n.components()`: the names of the whitelisted Lumen components present
/// on this node.
pub fn node_components(handle: u64) -> Vec<String> {
    snapshot()
        .components
        .get(&handle)
        .map(|list| list.iter().map(|(n, _)| n.clone()).collect())
        .unwrap_or_default()
}

/// `n.component(name)`: that component's public fields as a value map.
/// `Err` names an unknown / non-whitelisted component; `Ok(None)` means the
/// component is whitelisted but absent from this node.
pub fn node_component(handle: u64, name: &str) -> Result<Option<Vec<(String, String)>>, String> {
    let snap = snapshot();
    if !snap.known_components.iter().any(|n| n == name) {
        return Err(format!(
            "component(\"{name}\"): not a whitelisted component"
        ));
    }
    Ok(snap.components.get(&handle).and_then(|list| {
        list.iter()
            .find(|(n, _)| n == name)
            .map(|(_, map)| map.clone())
    }))
}

// ---------------------------------------------------------------------------
// Tree / document serialization
// ---------------------------------------------------------------------------

/// `n.outer_markup()`: serialize this subtree to `.lmn`-ish text.
pub fn outer_markup(handle: u64) -> String {
    let Some(entity) = NodeRef(handle).entity() else {
        return String::new();
    };
    let index = dom_index_snapshot();
    let mut out = String::new();
    write_markup(&index, entity, 0, &mut out);
    out
}

/// `n.inner_markup()`: serialize this node's children (not the node itself)
/// to `.lmn`-ish text. The read half of `set_inner_markup`; reuses the same
/// serializer over the child range.
pub fn inner_markup(handle: u64) -> String {
    let Some(entity) = NodeRef(handle).entity() else {
        return String::new();
    };
    let index = dom_index_snapshot();
    let Some(rec) = index.record(entity) else {
        return String::new();
    };
    let mut out = String::new();
    for child in &rec.children {
        write_markup(&index, *child, 0, &mut out);
    }
    out
}

fn write_markup(index: &DomIndex, entity: Entity, depth: usize, out: &mut String) {
    let Some(rec) = index.record(entity) else {
        return;
    };
    let pad = "  ".repeat(depth);
    let tag = if rec.tag.is_empty() { "div" } else { &rec.tag };
    out.push_str(&pad);
    out.push('<');
    out.push_str(tag);
    if let Some(id) = &rec.id {
        out.push_str(&format!(" id=\"{id}\""));
    }
    if !rec.classes.is_empty() {
        out.push_str(&format!(" class=\"{}\"", rec.classes.join(" ")));
    }
    let handle = NodeHandle::new(entity).pack();
    for (k, v) in attributes_of_handle(handle) {
        out.push_str(&format!(" {k}=\"{v}\""));
    }
    let text = crate::node_query::node_text(handle);
    if rec.children.is_empty() && text.as_deref().unwrap_or("").is_empty() {
        out.push_str("/>\n");
        return;
    }
    out.push('>');
    if let Some(t) = &text {
        if !t.is_empty() {
            out.push_str(t);
        }
    }
    if rec.children.is_empty() {
        out.push_str(&format!("</{tag}>\n"));
        return;
    }
    out.push('\n');
    for child in &rec.children {
        write_markup(index, *child, depth + 1, out);
    }
    out.push_str(&pad);
    out.push_str(&format!("</{tag}>\n"));
}

/// `dump_tree()`: a whole-tree structural dump (id / tag / classes / rect)
/// for debugging. An inspection call.
pub fn dump_tree() -> String {
    let index = dom_index_snapshot();
    let snap = snapshot();
    let mut out = String::new();
    for root in index.roots() {
        write_dump(&index, &snap, *root, 0, &mut out);
    }
    out
}

fn write_dump(
    index: &DomIndex,
    snap: &IntrospectSnapshot,
    entity: Entity,
    depth: usize,
    out: &mut String,
) {
    let Some(rec) = index.record(entity) else {
        return;
    };
    let pad = "  ".repeat(depth);
    let tag = if rec.tag.is_empty() { "div" } else { &rec.tag };
    out.push_str(&pad);
    out.push_str(tag);
    if let Some(id) = &rec.id {
        out.push('#');
        out.push_str(id);
    }
    for c in &rec.classes {
        out.push('.');
        out.push_str(c);
    }
    let handle = NodeHandle::new(entity).pack();
    if let Some(g) = snap.geometry.get(&handle) {
        out.push_str(&format!(
            " [{} {} {} {}]",
            g.rect.client_x, g.rect.client_y, g.rect.width, g.rect.height
        ));
    }
    out.push('\n');
    for child in &rec.children {
        write_dump(index, snap, *child, depth + 1, out);
    }
}

// ---------------------------------------------------------------------------
// Global runtime state
// ---------------------------------------------------------------------------

/// `pointer_state()`: window position, buttons, modifiers.
pub fn pointer_state() -> PointerSnapshot {
    snapshot().pointer
}

/// `frame_info()`: `{frame, dt_ms, dirty_count}`.
pub fn frame_info() -> FrameInfo {
    snapshot().frame
}

/// `signals_all()`: the whole PropertyStore signal set as `(name, value)`
/// pairs. An inspection call.
pub fn signals_all() -> Vec<(String, String)> {
    snapshot().signals.clone()
}
