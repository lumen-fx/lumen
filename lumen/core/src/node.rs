//! Node handles and the per-tick DOM index for the dynamic query API.
//!
//! The read side of the scripting DOM surface (`query`, `get_by_id`,
//! traversal) addresses live elements through a [`NodeHandle`] -- an
//! `Entity` plus its generation, so a stale handle resolves to nothing
//! instead of aliasing a recycled entity. [`DomIndex`] is an immutable
//! per-tick snapshot of the selector-reachable tree that the runtime
//! rebuilds each frame and publishes into a process-shared cache; script
//! hosts and the C-ABI read that snapshot without touching the live world.
//!
//! Selector matching itself lives in `lumen-ir` (the cascade matcher) and
//! is driven from `lumen-script`, which can depend on both this crate and
//! `lumen-ir`. This module holds only the handle types, the pure snapshot
//! data, and the traversal that needs no selector engine.

use bevy_ecs::entity::{Entities, Entity};
use bevy_ecs::resource::Resource;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Opaque handle to a live element: an [`Entity`] plus the generation it
/// carried when the handle was minted. Resolving a handle validates the
/// generation, so a handle to a despawned entity returns `None` rather
/// than addressing whatever entity later reused that index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHandle {
    /// The referenced entity (index + generation).
    pub entity: Entity,
    /// The entity's generation bits at mint time, kept for the packed
    /// wire form and for cross-checking after an unpack.
    pub generation: u32,
}

impl NodeHandle {
    /// Mint a handle for `entity`, capturing its current generation.
    pub fn new(entity: Entity) -> Self {
        Self {
            entity,
            generation: entity.generation().to_bits(),
        }
    }

    /// Pack into the `u64` wire form used by the C-ABI `LumenNode` and by
    /// hosts whose value type can hold 64 bits. The layout is
    /// `Entity::to_bits` (index in the low half, generation in the high
    /// half); treat it as opaque and round-trip only through [`Self::unpack`].
    pub fn pack(self) -> u64 {
        self.entity.to_bits()
    }

    /// Reconstruct from the packed wire form. Returns `None` for bits that
    /// never came from [`Self::pack`] (invalid index), never panicking.
    pub fn unpack(bits: u64) -> Option<Self> {
        Entity::try_from_bits(bits).map(Self::new)
    }

    /// Resolve against the live entity allocator: `Some(entity)` when the
    /// exact `(index, generation)` is still alive, `None` when it has been
    /// despawned (stale handle). Never panics.
    pub fn validate(&self, entities: &Entities) -> Option<Entity> {
        if entities.contains(self.entity) {
            Some(self.entity)
        } else {
            None
        }
    }
}

/// Side-table mapping small `i32` ids to node handles, for script hosts
/// whose value type cannot hold a 64-bit handle (candela's `Value` is
/// internally `i32`). `intern` is idempotent per entity, so a handle
/// keeps the same id across a tick; the sentinel id `0` means "no node".
///
/// Registered as a [`Resource`] for embedders that want a world-scoped
/// table; the runtime also drives a process-global instance (see
/// [`intern_node`] / [`resolve_node`]) so a `Send + Sync` host with no
/// `&World` at call time can still mint ids.
#[derive(Resource, Default, Debug)]
pub struct NodeHandles {
    /// `i32` id to packed handle bits. The bits are a real
    /// [`NodeHandle::pack`] value for a live element, or a reserved spawn
    /// token (see [`reserve_node_token`]) for a not-yet-materialized node.
    forward: HashMap<i32, u64>,
    reverse: HashMap<u64, i32>,
    next: i32,
}

impl NodeHandles {
    /// Intern `(entity, generation)`, returning a stable `i32` id (>= 1).
    /// Re-interning the same entity returns the same id.
    pub fn intern(&mut self, entity: Entity, generation: u32) -> i32 {
        let _ = generation;
        self.intern_raw(entity.to_bits())
    }

    /// Intern any packed handle (a real element's [`NodeHandle::pack`] or a
    /// reserved spawn token), returning a stable `i32` id. Idempotent per
    /// packed value.
    pub fn intern_raw(&mut self, packed: u64) -> i32 {
        if let Some(&id) = self.reverse.get(&packed) {
            return id;
        }
        self.next = self.next.wrapping_add(1);
        if self.next <= 0 {
            self.next = 1;
        }
        let id = self.next;
        self.forward.insert(id, packed);
        self.reverse.insert(packed, id);
        id
    }

    /// Resolve an interned id back to a live-element handle. Returns `None`
    /// for `0`, an unknown id, or a reserved spawn token (which has no
    /// entity until command-drain materializes it).
    pub fn resolve(&self, id: i32) -> Option<NodeHandle> {
        let raw = self.resolve_raw(id)?;
        if is_reserved_token(raw) {
            return None;
        }
        NodeHandle::unpack(raw)
    }

    /// Resolve an interned id to its packed bits (real handle or reserved
    /// spawn token). `0` / unknown yields `None`.
    pub fn resolve_raw(&self, id: i32) -> Option<u64> {
        if id == 0 {
            return None;
        }
        self.forward.get(&id).copied()
    }
}

// ---------------------------------------------------------------------------
// Reserved spawn tokens
// ---------------------------------------------------------------------------
//
// `spawn(tag)` must return a handle SYNCHRONOUSLY so a fluent chain
// (`spawn("div").set_class("row").append_to(parent)`) addresses one node
// across a whole tick, while the real ECS entity only materializes at the
// next command-drain. Script hosts hold no `&World` at call time, so they
// cannot reserve a live `Entity`. Instead a spawn mints a process-global
// reserved TOKEN -- a `u64` with the top bit set so it never aliases a real
// `Entity::to_bits` value. Each structural command carries this token; the
// runtime's command applier maps token -> freshly spawned entity in FIFO
// order, so the queued mutations land on the right node.

/// Top bit marking a `u64` as a reserved spawn token rather than a packed
/// [`NodeHandle`]. A real `Entity::to_bits` sets this bit only after a
/// single index is recycled ~2^31 times, which does not happen in a UI
/// session; handles are opaque and round-trip only through the provided
/// helpers, per the wire-format contract.
pub const RESERVED_TOKEN_FLAG: u64 = 1 << 63;

static SPAWN_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Mint a fresh reserved spawn token. Unique within the process for the
/// life of the run; consumed by the runtime's command applier, which maps
/// it onto the entity it spawns.
pub fn reserve_node_token() -> u64 {
    let n = SPAWN_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed) & !RESERVED_TOKEN_FLAG;
    RESERVED_TOKEN_FLAG | n
}

/// Whether `handle` is a reserved spawn token (top bit set) rather than a
/// packed live-element handle.
pub fn is_reserved_token(handle: u64) -> bool {
    handle & RESERVED_TOKEN_FLAG != 0
}

/// Intern any packed handle (real or reserved token) in the process-global
/// side-table, returning its `i32` id. Used by the candela host, whose
/// values cannot carry a 64-bit handle.
pub fn intern_node_raw(packed: u64) -> i32 {
    node_handles()
        .lock()
        .map(|mut h| h.intern_raw(packed))
        .unwrap_or(0)
}

/// Resolve an `i32` id to its packed bits (real handle or reserved token)
/// against the process-global side-table.
pub fn resolve_node_raw(id: i32) -> Option<u64> {
    node_handles().lock().ok().and_then(|h| h.resolve_raw(id))
}

static NODE_HANDLES: OnceLock<Mutex<NodeHandles>> = OnceLock::new();

fn node_handles() -> &'static Mutex<NodeHandles> {
    NODE_HANDLES.get_or_init(|| Mutex::new(NodeHandles::default()))
}

/// Intern a handle in the process-global side-table, returning its `i32`
/// id. Used by the candela host, whose closures have no `&World`.
pub fn intern_node(entity: Entity, generation: u32) -> i32 {
    node_handles()
        .lock()
        .map(|mut h| h.intern(entity, generation))
        .unwrap_or(0)
}

/// Resolve an `i32` id against the process-global side-table.
pub fn resolve_node(id: i32) -> Option<NodeHandle> {
    node_handles().lock().ok().and_then(|h| h.resolve(id))
}

/// One selector-reachable element in a [`DomIndex`] snapshot. Positional
/// fields (`child_index`, `sibling_count`, `doc_order`) are computed by
/// [`DomIndex::build`] from the hierarchy; callers constructing records
/// leave them at zero.
#[derive(Debug, Clone)]
pub struct DomRecord {
    /// The element entity.
    pub entity: Entity,
    /// Its generation bits, mirrored into minted handles.
    pub generation: u32,
    /// Markup tag (`button`, `label`, ...). Empty for a tagless container.
    pub tag: String,
    /// Stable `id="..."`, if any.
    pub id: Option<String>,
    /// Class list from `class="..."`.
    pub classes: Vec<String>,
    /// Parent element, if this record has one inside the snapshot.
    pub parent: Option<Entity>,
    /// Child elements in document order.
    pub children: Vec<Entity>,
    /// 1-based position among siblings (computed).
    pub child_index: i32,
    /// Total sibling count including self (computed).
    pub sibling_count: i32,
    /// Depth-first pre-order rank across the whole snapshot (computed).
    pub doc_order: u32,
}

/// Immutable per-tick snapshot of the selector-reachable element tree.
/// Traversal and `get_by_id` read this directly; selector queries run in
/// `lumen-script` over the same records.
#[derive(Debug, Default)]
pub struct DomIndex {
    records: Vec<DomRecord>,
    by_entity: HashMap<u64, usize>,
    by_id: HashMap<String, Entity>,
    roots: Vec<Entity>,
}

impl DomIndex {
    /// Build a snapshot from unordered records. Each record must carry its
    /// `parent` and ordered `children`; this computes sibling positions,
    /// depth-first document order, the entity and id lookup maps, and the
    /// root list. The last `id` wins on a duplicate (matching cascade
    /// "first match by document order" is applied by `get_by_id` reading
    /// the first-in-doc-order entry).
    pub fn build(mut records: Vec<DomRecord>) -> Self {
        let mut by_entity: HashMap<u64, usize> = HashMap::with_capacity(records.len());
        for (i, r) in records.iter().enumerate() {
            by_entity.insert(r.entity.to_bits(), i);
        }

        // Sibling positions. A record whose parent is absent from the
        // snapshot is treated as a root; roots are ordered by entity bits
        // for determinism.
        let mut roots: Vec<Entity> = Vec::new();
        for r in &records {
            let in_index = r
                .parent
                .is_some_and(|p| by_entity.contains_key(&p.to_bits()));
            if !in_index {
                roots.push(r.entity);
            }
        }
        roots.sort_by_key(|e| e.to_bits());

        // child_index / sibling_count from each parent's children list;
        // roots take their position among the sorted root list. Indexed so
        // the write to `records[i]` can also read sibling records.
        #[allow(clippy::needless_range_loop)]
        for i in 0..records.len() {
            let (idx, count) = match records[i].parent {
                Some(p) if by_entity.contains_key(&p.to_bits()) => {
                    let pi = by_entity[&p.to_bits()];
                    let siblings = &records[pi].children;
                    let pos = siblings
                        .iter()
                        .position(|c| *c == records[i].entity)
                        .map(|z| z as i32 + 1)
                        .unwrap_or(1);
                    (pos, siblings.len().max(1) as i32)
                }
                _ => {
                    let pos = roots
                        .iter()
                        .position(|c| *c == records[i].entity)
                        .map(|z| z as i32 + 1)
                        .unwrap_or(1);
                    (pos, roots.len().max(1) as i32)
                }
            };
            records[i].child_index = idx;
            records[i].sibling_count = count;
        }

        // Depth-first document order from the roots.
        let mut order: u32 = 0;
        let mut stack: Vec<Entity> = roots.iter().rev().copied().collect();
        let mut doc: HashMap<u64, u32> = HashMap::with_capacity(records.len());
        while let Some(e) = stack.pop() {
            let bits = e.to_bits();
            if doc.contains_key(&bits) {
                continue;
            }
            doc.insert(bits, order);
            order += 1;
            if let Some(&ri) = by_entity.get(&bits) {
                for child in records[ri].children.iter().rev() {
                    stack.push(*child);
                }
            }
        }
        for r in &mut records {
            r.doc_order = doc.get(&r.entity.to_bits()).copied().unwrap_or(u32::MAX);
        }

        // id -> entity, first in document order wins.
        let mut ordered: Vec<usize> = (0..records.len()).collect();
        ordered.sort_by_key(|&i| records[i].doc_order);
        let mut by_id: HashMap<String, Entity> = HashMap::new();
        for &i in &ordered {
            if let Some(id) = &records[i].id {
                by_id.entry(id.clone()).or_insert(records[i].entity);
            }
        }

        Self {
            records,
            by_entity,
            by_id,
            roots,
        }
    }

    /// All records, unspecified order. Query callers sort by
    /// [`DomRecord::doc_order`].
    pub fn records(&self) -> &[DomRecord] {
        &self.records
    }

    /// Look up a record by entity.
    pub fn record(&self, entity: Entity) -> Option<&DomRecord> {
        self.by_entity
            .get(&entity.to_bits())
            .map(|&i| &self.records[i])
    }

    /// Fast id lookup (`get_by_id`), first match in document order.
    pub fn get_by_id(&self, id: &str) -> Option<Entity> {
        self.by_id.get(id).copied()
    }

    /// Root elements, ordered.
    pub fn roots(&self) -> &[Entity] {
        &self.roots
    }

    /// The document root (`document()`), the first root in order.
    pub fn document(&self) -> Option<Entity> {
        self.roots.first().copied()
    }

    /// Parent of `entity`, if any (`node.parent()`).
    pub fn parent(&self, entity: Entity) -> Option<Entity> {
        self.record(entity).and_then(|r| r.parent)
    }

    /// Ordered children of `entity` (`node.children()`).
    pub fn children(&self, entity: Entity) -> Vec<Entity> {
        self.record(entity)
            .map(|r| r.children.clone())
            .unwrap_or_default()
    }

    /// First child (`node.first_child()`).
    pub fn first_child(&self, entity: Entity) -> Option<Entity> {
        self.record(entity)
            .and_then(|r| r.children.first().copied())
    }

    /// Last child (`node.last_child()`).
    pub fn last_child(&self, entity: Entity) -> Option<Entity> {
        self.record(entity).and_then(|r| r.children.last().copied())
    }

    /// The sibling list `entity` belongs to (its parent's children, or the
    /// root list when it has no parent in the snapshot).
    fn siblings_of(&self, entity: Entity) -> &[Entity] {
        match self.parent(entity) {
            Some(p) => self.record(p).map(|r| r.children.as_slice()).unwrap_or(&[]),
            None => &self.roots,
        }
    }

    /// Next sibling in document order (`node.next()`).
    pub fn next_sibling(&self, entity: Entity) -> Option<Entity> {
        let sibs = self.siblings_of(entity);
        let pos = sibs.iter().position(|e| *e == entity)?;
        sibs.get(pos + 1).copied()
    }

    /// Previous sibling in document order (`node.prev()`).
    pub fn prev_sibling(&self, entity: Entity) -> Option<Entity> {
        let sibs = self.siblings_of(entity);
        let pos = sibs.iter().position(|e| *e == entity)?;
        if pos == 0 {
            None
        } else {
            sibs.get(pos - 1).copied()
        }
    }

    /// The ancestor chain of `entity`, closest-parent-first (not including
    /// `entity`). Used by `lumen-script` to build the root-first
    /// `AncestorInfo` slice the selector matcher consumes.
    pub fn ancestors(&self, entity: Entity) -> Vec<Entity> {
        let mut out = Vec::new();
        let mut cur = entity;
        for _ in 0..256 {
            match self.parent(cur) {
                Some(p) => {
                    out.push(p);
                    cur = p;
                }
                None => break,
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Process-shared snapshot cache
// ---------------------------------------------------------------------------

static DOM_INDEX_CACHE: OnceLock<RwLock<Arc<DomIndex>>> = OnceLock::new();

fn dom_index_cache() -> &'static RwLock<Arc<DomIndex>> {
    DOM_INDEX_CACHE.get_or_init(|| RwLock::new(Arc::new(DomIndex::default())))
}

/// Publish a freshly-built snapshot for cross-thread readers. The runtime
/// calls this each tick from `build_dom_index`, before event dispatch, so
/// a query issued inside a handler sees the current tree.
pub fn publish_dom_index(index: DomIndex) {
    if let Ok(mut guard) = dom_index_cache().write() {
        *guard = Arc::new(index);
    }
}

/// Read the current snapshot. Returns a cheap `Arc` clone so the caller
/// drops the lock immediately. Script hosts and the C-ABI read here; they
/// hold no `&World` at call time.
pub fn dom_index_snapshot() -> Arc<DomIndex> {
    dom_index_cache()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| Arc::new(DomIndex::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;

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

    #[test]
    fn handle_packs_and_validates() {
        let mut w = World::new();
        let e = w.spawn_empty().id();
        let h = NodeHandle::new(e);
        let packed = h.pack();
        let back = NodeHandle::unpack(packed).unwrap();
        assert_eq!(back.entity, e);
        assert_eq!(h.validate(w.entities()), Some(e));
        w.despawn(e);
        // Stale handle: same bits, but the entity is gone.
        assert_eq!(h.validate(w.entities()), None);
    }

    #[test]
    fn unpack_rejects_garbage_without_panicking() {
        assert!(NodeHandle::unpack(0).is_none());
    }

    #[test]
    fn side_table_is_idempotent() {
        let mut w = World::new();
        let e = w.spawn_empty().id();
        let mut t = NodeHandles::default();
        let a = t.intern(e, e.generation().to_bits());
        let b = t.intern(e, e.generation().to_bits());
        assert_eq!(a, b);
        assert!(a >= 1);
        assert_eq!(t.resolve(a).unwrap().entity, e);
        assert!(t.resolve(0).is_none());
    }

    #[test]
    fn index_computes_tree_shape() {
        let mut w = World::new();
        let root = w.spawn_empty().id();
        let a = w.spawn_empty().id();
        let b = w.spawn_empty().id();
        let recs = vec![
            rec(root, "root", Some("app"), &[], None, &[a, b]),
            rec(a, "button", Some("save"), &["row"], Some(root), &[]),
            rec(b, "button", Some("cancel"), &["row"], Some(root), &[]),
        ];
        let idx = DomIndex::build(recs);
        assert_eq!(idx.document(), Some(root));
        assert_eq!(idx.get_by_id("save"), Some(a));
        assert_eq!(idx.parent(a), Some(root));
        assert_eq!(idx.children(root), vec![a, b]);
        assert_eq!(idx.first_child(root), Some(a));
        assert_eq!(idx.last_child(root), Some(b));
        assert_eq!(idx.next_sibling(a), Some(b));
        assert_eq!(idx.prev_sibling(b), Some(a));
        assert_eq!(idx.next_sibling(b), None);
        assert_eq!(idx.ancestors(a), vec![root]);
        // Positions.
        assert_eq!(idx.record(a).unwrap().child_index, 1);
        assert_eq!(idx.record(b).unwrap().child_index, 2);
        assert_eq!(idx.record(b).unwrap().sibling_count, 2);
        // Document order: root, a, b.
        assert!(idx.record(root).unwrap().doc_order < idx.record(a).unwrap().doc_order);
        assert!(idx.record(a).unwrap().doc_order < idx.record(b).unwrap().doc_order);
    }

    #[test]
    fn reserved_tokens_are_distinct_and_flagged() {
        let a = reserve_node_token();
        let b = reserve_node_token();
        assert_ne!(a, b, "each reserved token is unique");
        assert!(is_reserved_token(a));
        assert!(is_reserved_token(b));
        // A real entity handle is never mistaken for a reserved token.
        let mut w = World::new();
        let e = w.spawn_empty().id();
        assert!(!is_reserved_token(NodeHandle::new(e).pack()));
    }

    #[test]
    fn raw_intern_round_trips_real_and_reserved() {
        let mut w = World::new();
        let e = w.spawn_empty().id();
        let real = NodeHandle::new(e).pack();
        let token = reserve_node_token();
        let mut t = NodeHandles::default();
        let id_real = t.intern_raw(real);
        let id_tok = t.intern_raw(token);
        assert_ne!(id_real, id_tok);
        assert_eq!(t.resolve_raw(id_real), Some(real));
        assert_eq!(t.resolve_raw(id_tok), Some(token));
        // `resolve` (element-only) yields the handle for the real node and
        // nothing for the reserved token.
        assert_eq!(t.resolve(id_real).unwrap().entity, e);
        assert!(t.resolve(id_tok).is_none());
    }

    #[test]
    fn global_cache_round_trips() {
        let mut w = World::new();
        let root = w.spawn_empty().id();
        publish_dom_index(DomIndex::build(vec![rec(
            root,
            "root",
            Some("r"),
            &[],
            None,
            &[],
        )]));
        assert_eq!(dom_index_snapshot().get_by_id("r"), Some(root));
    }
}
