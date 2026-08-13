//! Host-neutral read side of the dynamic DOM API: selector queries,
//! `get_by_id`, and traversal over the per-tick [`DomIndex`] snapshot.
//!
//! Script hosts (`rhai` / `lua` / `candela`) and the C-ABI all funnel
//! through these functions. Handles cross the boundary as a packed `u64`
//! ([`NodeHandle::pack`]); candela, whose value type is `i32`, interns the
//! handle into the process-global side-table instead (see
//! `lumen_core::node::intern_node`).
//!
//! Selector matching reuses the cascade matcher in `lumen-ir`; there is
//! no second selector engine. A query walks each snapshot record's
//! ancestor chain (root-first) and applies
//! [`lumen_ir::css::selector_matches`]. Sibling combinators (`+`, `~`)
//! inherit the matcher's conservative-fail behavior.

use lumen_core::node::{DomIndex, DomRecord, NodeHandle, dom_index_snapshot};
use lumen_ir::css::AncestorInfo;

use bevy_ecs::entity::Entity;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::ScriptCommand;

/// Host-neutral packed node handle. `0` is reserved for "no node".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRef(pub u64);

impl NodeRef {
    /// The null handle (`0`), meaning "no node".
    pub const NONE: NodeRef = NodeRef(0);

    /// Wrap an entity as a packed handle.
    pub fn from_entity(entity: Entity) -> Self {
        Self(NodeHandle::new(entity).pack())
    }

    /// Decode to an entity, or `None` for the null / invalid handle or a
    /// reserved spawn token (which has no entity until command-drain).
    pub fn entity(self) -> Option<Entity> {
        if self.0 == 0 || lumen_core::node::is_reserved_token(self.0) {
            return None;
        }
        NodeHandle::unpack(self.0).map(|h| h.entity)
    }
}

impl From<Entity> for NodeRef {
    fn from(e: Entity) -> Self {
        Self::from_entity(e)
    }
}

/// Materialized query result: packed handles in document order. Mirrors
/// the Bevy-flavored `NodeQuery` consumers the host types wrap.
#[derive(Debug, Clone, Default)]
pub struct NodeQueryResult {
    /// Matched handles, document order.
    pub nodes: Vec<u64>,
}

impl NodeQueryResult {
    /// Number of matches.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the result set is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// First match in document order (`first()`).
    pub fn first(&self) -> Option<u64> {
        self.nodes.first().copied()
    }

    /// Match at `index` (`nth(i)`).
    pub fn nth(&self, index: usize) -> Option<u64> {
        self.nodes.get(index).copied()
    }

    /// Exactly-one match (Bevy `single()`): the sole handle, else an error
    /// naming the count.
    pub fn single(&self) -> Result<u64, String> {
        match self.nodes.len() {
            1 => Ok(self.nodes[0]),
            n => Err(format!(
                "query.single(): expected exactly 1 match, found {n}"
            )),
        }
    }

    /// Fallible one-or-none form (`get_single()`): the sole handle, or
    /// `None` for zero or many.
    pub fn get_single(&self) -> Option<u64> {
        if self.nodes.len() == 1 {
            Some(self.nodes[0])
        } else {
            None
        }
    }

    /// All handles as a slice (`iter()` / `collect()`).
    pub fn collect(&self) -> Vec<u64> {
        self.nodes.clone()
    }
}

/// Build the matcher's subject identity for a record, threading the real
/// sibling position so `:first-child` / `:nth-child` resolve.
fn subject_info(rec: &DomRecord) -> AncestorInfo {
    AncestorInfo::new(rec.tag.clone(), rec.classes.clone(), rec.id.clone())
        .with_position(rec.child_index, rec.sibling_count)
}

/// Root-first ancestor identities for `entity` (excluding itself).
pub(crate) fn ancestor_infos(index: &DomIndex, entity: Entity) -> Vec<AncestorInfo> {
    let mut chain: Vec<AncestorInfo> = index
        .ancestors(entity)
        .into_iter()
        .filter_map(|e| index.record(e).map(subject_info))
        .collect();
    chain.reverse();
    chain
}

/// Does `entity` match any selector in `selectors`?
fn matches_any(index: &DomIndex, entity: Entity, selectors: &[lumen_ir::css::SelectorBuf]) -> bool {
    let Some(rec) = index.record(entity) else {
        return false;
    };
    let subject = subject_info(rec);
    let ancestors = ancestor_infos(index, entity);
    selectors
        .iter()
        .any(|sel| lumen_ir::css::selector_matches(sel, &subject, &ancestors))
}

/// Run a selector query against `index`, returning matched entities in
/// document order. Errors carry the selector parse message.
pub fn query_entities(index: &DomIndex, selector: &str) -> Result<Vec<Entity>, String> {
    let selectors = lumen_ir::css::parse_selector_list(selector)?;
    let mut hits: Vec<(u32, Entity)> = Vec::new();
    for rec in index.records() {
        if matches_any(index, rec.entity, &selectors) {
            hits.push((rec.doc_order, rec.entity));
        }
    }
    hits.sort_by_key(|(order, _)| *order);
    Ok(hits.into_iter().map(|(_, e)| e).collect())
}

/// Walk from `start` up the ancestor chain (including `start`) and return
/// the nearest element matching `selector` (`node.closest(sel)`).
pub fn closest_entity(
    index: &DomIndex,
    start: Entity,
    selector: &str,
) -> Result<Option<Entity>, String> {
    let selectors = lumen_ir::css::parse_selector_list(selector)?;
    let mut cur = Some(start);
    while let Some(e) = cur {
        if matches_any(index, e, &selectors) {
            return Ok(Some(e));
        }
        cur = index.parent(e);
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Snapshot-backed entry points (what the hosts and the C-ABI call). Each
// reads the current per-tick snapshot and returns packed handles.
// ---------------------------------------------------------------------------

/// `query(sel)`: run against the current snapshot, packed handles.
pub fn run_query(selector: &str) -> Result<NodeQueryResult, String> {
    let index = dom_index_snapshot();
    let nodes = query_entities(&index, selector)?
        .into_iter()
        .map(|e| NodeHandle::new(e).pack())
        .collect();
    Ok(NodeQueryResult { nodes })
}

/// `get_by_id(id)`: fast id path, packed handle or `None`.
pub fn run_get_by_id(id: &str) -> Option<u64> {
    dom_index_snapshot()
        .get_by_id(id)
        .map(|e| NodeHandle::new(e).pack())
}

/// `document()`: the root element as a packed handle.
pub fn run_document() -> Option<u64> {
    dom_index_snapshot()
        .document()
        .map(|e| NodeHandle::new(e).pack())
}

fn map_relation<F>(handle: u64, f: F) -> Option<u64>
where
    F: FnOnce(&DomIndex, Entity) -> Option<Entity>,
{
    let entity = NodeRef(handle).entity()?;
    let index = dom_index_snapshot();
    f(&index, entity).map(|e| NodeHandle::new(e).pack())
}

/// `node.parent()`.
pub fn node_parent(handle: u64) -> Option<u64> {
    map_relation(handle, |i, e| i.parent(e))
}

/// `node.first_child()`.
pub fn node_first_child(handle: u64) -> Option<u64> {
    map_relation(handle, |i, e| i.first_child(e))
}

/// `node.last_child()`.
pub fn node_last_child(handle: u64) -> Option<u64> {
    map_relation(handle, |i, e| i.last_child(e))
}

/// `node.next()`.
pub fn node_next(handle: u64) -> Option<u64> {
    map_relation(handle, |i, e| i.next_sibling(e))
}

/// `node.prev()`.
pub fn node_prev(handle: u64) -> Option<u64> {
    map_relation(handle, |i, e| i.prev_sibling(e))
}

/// `node.children()`: packed handles in document order.
pub fn node_children(handle: u64) -> Vec<u64> {
    let Some(entity) = NodeRef(handle).entity() else {
        return Vec::new();
    };
    dom_index_snapshot()
        .children(entity)
        .into_iter()
        .map(|e| NodeHandle::new(e).pack())
        .collect()
}

/// `node.closest(sel)`: nearest matching ancestor-or-self, packed handle.
pub fn node_closest(handle: u64, selector: &str) -> Result<Option<u64>, String> {
    let Some(entity) = NodeRef(handle).entity() else {
        return Ok(None);
    };
    let index = dom_index_snapshot();
    Ok(closest_entity(&index, entity, selector)?.map(|e| NodeHandle::new(e).pack()))
}

/// `node.exists()` / `node_valid(h)`: whether the handle is present in the
/// current snapshot. The snapshot rebuilds each tick, so a despawned node
/// drops out; this is the read-side liveness check without a live world.
pub fn node_valid(handle: u64) -> bool {
    match NodeRef(handle).entity() {
        Some(entity) => dom_index_snapshot().record(entity).is_some(),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Per-node detail snapshot + style context (the read-back side of the
// mutation API: text / attrs / inline / computed style). DomIndex carries
// tag / id / classes / hierarchy; text, generic attributes, and inline
// style live here, published each tick alongside the stylesheet + media so
// `computed_style` can re-run the cascade host-side without a live world.
// ---------------------------------------------------------------------------

/// Per-node detail carried in the snapshot, keyed by packed handle.
#[derive(Debug, Clone, Default)]
pub struct NodeDetail {
    /// The node's text content, if any.
    pub text: Option<String>,
    /// Generic attribute map (attrs with no typed component).
    pub attributes: Vec<(String, String)>,
    /// Inline-style overrides (`element.style`), ordered.
    pub inline_style: Vec<(String, String)>,
}

/// Snapshot of per-node details plus the cascade inputs `computed_style`
/// needs. Rebuilt and published by the runtime each tick.
#[derive(Default)]
pub struct DomDetails {
    nodes: HashMap<u64, NodeDetail>,
    sheet: Option<Arc<lumen_ir::css::Stylesheet>>,
    media: lumen_ir::css::MediaContext,
}

impl DomDetails {
    /// Build from a packed-handle-keyed detail map plus the live stylesheet
    /// and media context.
    pub fn new(
        nodes: HashMap<u64, NodeDetail>,
        sheet: Option<Arc<lumen_ir::css::Stylesheet>>,
        media: lumen_ir::css::MediaContext,
    ) -> Self {
        Self {
            nodes,
            sheet,
            media,
        }
    }

    fn detail(&self, handle: u64) -> Option<&NodeDetail> {
        self.nodes.get(&handle)
    }

    /// The live stylesheet the cascade re-resolver reads, if published.
    pub fn sheet(&self) -> Option<&Arc<lumen_ir::css::Stylesheet>> {
        self.sheet.as_ref()
    }

    /// The media context the cascade resolves `@media` against.
    pub fn media(&self) -> lumen_ir::css::MediaContext {
        self.media
    }

    /// The inline-style overrides published for `handle`, if any.
    pub fn inline_style_of(&self, handle: u64) -> Vec<(String, String)> {
        self.detail(handle)
            .map(|d| d.inline_style.clone())
            .unwrap_or_default()
    }

    /// The generic attribute map published for `handle`, if any.
    pub fn attributes_of(&self, handle: u64) -> Vec<(String, String)> {
        self.detail(handle)
            .map(|d| d.attributes.clone())
            .unwrap_or_default()
    }
}

static DOM_DETAILS: OnceLock<RwLock<Arc<DomDetails>>> = OnceLock::new();

fn dom_details_cell() -> &'static RwLock<Arc<DomDetails>> {
    DOM_DETAILS.get_or_init(|| RwLock::new(Arc::new(DomDetails::default())))
}

/// Publish a freshly-built detail snapshot for cross-thread readers. The
/// runtime calls this each tick from the same system that publishes the
/// [`DomIndex`].
pub fn publish_dom_details(details: DomDetails) {
    if let Ok(mut g) = dom_details_cell().write() {
        *g = Arc::new(details);
    }
}

/// Read the current detail snapshot (cheap `Arc` clone).
pub fn dom_details_snapshot() -> Arc<DomDetails> {
    dom_details_cell()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| Arc::new(DomDetails::default()))
}

/// `node.text()`: the node's current text content, or `None`.
pub fn node_text(handle: u64) -> Option<String> {
    dom_details_snapshot()
        .detail(handle)
        .and_then(|d| d.text.clone())
}

/// `node.get_attr(name)`. KNOWN attrs resolve from the index (`id`,
/// `class`, `text`); everything else from the generic attribute map.
pub fn node_get_attr(handle: u64, name: &str) -> Option<String> {
    match name {
        "id" => {
            let entity = NodeRef(handle).entity()?;
            dom_index_snapshot()
                .record(entity)
                .and_then(|r| r.id.clone())
        }
        "class" => {
            let entity = NodeRef(handle).entity()?;
            let classes = dom_index_snapshot()
                .record(entity)
                .map(|r| r.classes.clone())?;
            if classes.is_empty() {
                None
            } else {
                Some(classes.join(" "))
            }
        }
        "text" => node_text(handle),
        _ => dom_details_snapshot().detail(handle).and_then(|d| {
            d.attributes
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }),
    }
}

/// The generic attribute map (attrs with no typed component) published for
/// `handle`, ordered. Backs the introspection `attrs()` getter.
pub fn attributes_of_handle(handle: u64) -> Vec<(String, String)> {
    dom_details_snapshot().attributes_of(handle)
}

/// `node.id()`: the node's stable id, or `None`.
pub fn node_id(handle: u64) -> Option<String> {
    node_get_attr(handle, "id")
}

/// `node.class_list().contains(class)`.
pub fn node_class_contains(handle: u64, class: &str) -> bool {
    let Some(entity) = NodeRef(handle).entity() else {
        return false;
    };
    dom_index_snapshot()
        .record(entity)
        .map(|r| r.classes.iter().any(|c| c == class))
        .unwrap_or(false)
}

/// `node.style_get(prop)`: the inline-style override for `prop`, or `None`.
pub fn node_style_get(handle: u64, property: &str) -> Option<String> {
    dom_details_snapshot().detail(handle).and_then(|d| {
        d.inline_style
            .iter()
            .find(|(k, _)| k == property)
            .map(|(_, v)| v.clone())
    })
}

/// `node.computed_style(prop)`: the value of `prop` after the full cascade
/// (stylesheet + inherited + inline). Reflects the last committed tick;
/// an inline write issued this tick is visible next tick (commands apply
/// at drain). Returns `None` for an unmodeled property or an unknown node.
pub fn node_computed_style(handle: u64, property: &str) -> Option<String> {
    lumen_ir::css::computed_property(&resolved_attributes(handle), property)
}

/// The fully-cascaded [`Attributes`](lumen_ir::layout_ir::Attributes) for
/// `handle`: the stylesheet cascade (resolved against the live ancestor
/// chain) with the element's inline-style overrides applied on top. Shared
/// by `computed_style(prop)` and the full-map `computed_style()`
/// introspection getter. Reflects the last committed tick.
pub fn resolved_attributes(handle: u64) -> lumen_ir::layout_ir::Attributes {
    let details = dom_details_snapshot();
    let entity = NodeRef(handle).entity();

    // Resolve the cascade against the live tree when we have a stylesheet
    // and the node is in the index; fall back to inline-only otherwise.
    let mut attrs = lumen_ir::layout_ir::Attributes::default();
    if let (Some(entity), Some(sheet)) = (entity, details.sheet.as_ref()) {
        let index = dom_index_snapshot();
        if let Some(rec) = index.record(entity) {
            let mut el = lumen_ir::layout_ir::Element {
                tag: rec.tag.clone(),
                ..Default::default()
            };
            el.attrs.classes = rec.classes.clone();
            el.attrs.id = rec.id.clone();
            let ancestors = ancestor_infos(&index, entity);
            let _ =
                lumen_ir::css::reapply_with_ancestors(&mut el, sheet, &details.media, &ancestors);
            attrs = el.attrs;
        }
    }
    // Overlay inline style (highest tier).
    if let Some(detail) = details.detail(handle) {
        for (prop, value) in &detail.inline_style {
            let _ = lumen_ir::css::apply_inline_declaration(prop, value, &mut attrs);
        }
    }
    attrs
}

// ---------------------------------------------------------------------------
// Focus / hover mirror for `document.focused()` / `document.hovered()`.
// The runtime publishes the current targets each tick; hosts read them as
// packed handles.
// ---------------------------------------------------------------------------

static FOCUS_STATE: OnceLock<RwLock<(Option<u64>, Option<u64>)>> = OnceLock::new();

fn focus_cell() -> &'static RwLock<(Option<u64>, Option<u64>)> {
    FOCUS_STATE.get_or_init(|| RwLock::new((None, None)))
}

/// Publish the current focus / hover targets (packed handles). The runtime
/// calls this each tick.
pub fn publish_focus(focused: Option<u64>, hovered: Option<u64>) {
    if let Ok(mut g) = focus_cell().write() {
        *g = (focused, hovered);
    }
}

/// `document.focused()`: the focused node handle, or `None`.
pub fn focused_node() -> Option<u64> {
    focus_cell().read().ok().and_then(|g| g.0)
}

/// `document.hovered()`: the hovered node handle, or `None`.
pub fn hovered_node() -> Option<u64> {
    focus_cell().read().ok().and_then(|g| g.1)
}

// ---------------------------------------------------------------------------
// External DOM command bus. Fire-and-forget mutation commands issued from
// outside a script tick (the C-ABI, the Rust SDK) queue here; the runtime
// drains them into `ScriptCommandEvent` each tick so they flow through the
// same applier as script-issued mutations. Script hosts push into their
// own per-host sink and do not use this bus.
// ---------------------------------------------------------------------------

static DOM_CMD_QUEUE: OnceLock<Mutex<Vec<ScriptCommand>>> = OnceLock::new();

fn dom_cmd_queue() -> &'static Mutex<Vec<ScriptCommand>> {
    DOM_CMD_QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Enqueue a mutation command from a non-script surface (C-ABI / SDK).
/// Returns `false` only if the process-global lock is poisoned.
pub fn push_external_dom_command(cmd: ScriptCommand) -> bool {
    match dom_cmd_queue().lock() {
        Ok(mut q) => {
            q.push(cmd);
            true
        }
        Err(_) => false,
    }
}

/// Drain all queued external DOM commands (FIFO). Called by the runtime
/// once per tick.
pub fn drain_external_dom_commands() -> Vec<ScriptCommand> {
    dom_cmd_queue()
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Host-neutral mutation builders. Each mints any reserved token and returns
// the ScriptCommand for the caller to push into its sink (script hosts) or
// the external bus (C-ABI / SDK). The returned handle is what the fluent
// chain keeps addressing.
// ---------------------------------------------------------------------------

/// `create(tag)`: mint a reserved token and the backing [`ScriptCommand`].
/// Returns `(handle, command)`; the handle is valid for the whole tick.
pub fn build_spawn(tag: &str) -> (u64, ScriptCommand) {
    let reserved = lumen_core::node::reserve_node_token();
    (
        reserved,
        ScriptCommand::Spawn {
            tag: tag.to_string(),
            reserved,
        },
    )
}

/// `source.clone_deep()`: mint a reserved token for the clone root and the
/// backing [`ScriptCommand`].
pub fn build_clone(source: u64) -> (u64, ScriptCommand) {
    let reserved = lumen_core::node::reserve_node_token();
    (reserved, ScriptCommand::CloneNode { source, reserved })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use lumen_core::node::{DomRecord, publish_dom_index};

    fn rec(
        entity: Entity,
        tag: &str,
        id: Option<&str>,
        classes: &[&str],
        parent: Option<Entity>,
        children: &[Entity],
    ) -> DomRecord {
        DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: tag.to_string(),
            id: id.map(str::to_string),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            parent,
            children: children.to_vec(),
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        }
    }

    fn fixture() -> (DomIndex, Entity, Entity, Entity, Entity) {
        let mut w = World::new();
        let root = w.spawn_empty().id();
        let card = w.spawn_empty().id();
        let save = w.spawn_empty().id();
        let cancel = w.spawn_empty().id();
        let idx = DomIndex::build(vec![
            rec(root, "root", Some("app"), &["app"], None, &[card]),
            rec(card, "div", None, &["card"], Some(root), &[save, cancel]),
            rec(save, "button", Some("save"), &["row"], Some(card), &[]),
            rec(cancel, "button", Some("cancel"), &["row"], Some(card), &[]),
        ]);
        (idx, root, card, save, cancel)
    }

    #[test]
    fn query_by_id_class_and_descendant() {
        let (idx, _root, _card, save, cancel) = fixture();
        assert_eq!(query_entities(&idx, "#save").unwrap(), vec![save]);
        assert_eq!(query_entities(&idx, ".row").unwrap(), vec![save, cancel]);
        assert_eq!(
            query_entities(&idx, ".card button").unwrap(),
            vec![save, cancel]
        );
        assert_eq!(
            query_entities(&idx, ".card > .row").unwrap(),
            vec![save, cancel]
        );
        assert!(query_entities(&idx, ".nope").unwrap().is_empty());
    }

    #[test]
    fn closest_walks_up() {
        let (idx, root, card, save, _cancel) = fixture();
        assert_eq!(closest_entity(&idx, save, ".card").unwrap(), Some(card));
        assert_eq!(closest_entity(&idx, save, ".app").unwrap(), Some(root));
        assert_eq!(closest_entity(&idx, save, ".row").unwrap(), Some(save));
        assert_eq!(closest_entity(&idx, save, ".none").unwrap(), None);
    }

    #[test]
    fn snapshot_entry_points_and_liveness() {
        let (idx, root, card, save, cancel) = fixture();
        publish_dom_index(idx);
        let q = run_query(".row").unwrap();
        assert_eq!(q.len(), 2);
        assert!(q.single().is_err());
        let one = run_query("#save").unwrap();
        assert_eq!(one.single().unwrap(), NodeHandle::new(save).pack());
        assert_eq!(
            run_get_by_id("cancel"),
            Some(NodeHandle::new(cancel).pack())
        );
        assert_eq!(run_document(), Some(NodeHandle::new(root).pack()));
        let save_h = NodeHandle::new(save).pack();
        assert_eq!(node_next(save_h), Some(NodeHandle::new(cancel).pack()));
        assert_eq!(node_parent(save_h), Some(NodeHandle::new(card).pack()));
        assert_eq!(node_children(NodeHandle::new(card).pack()).len(), 2);
        assert_eq!(
            node_closest(save_h, ".app").unwrap(),
            Some(NodeHandle::new(root).pack())
        );
        assert!(node_valid(save_h));
        assert!(!node_valid(0));
    }
}
