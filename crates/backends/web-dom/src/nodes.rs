//! Which element an entity is, and how it got one.
//!
//! Every node of a page has a path: the chain of child indices from the page
//! root, which [`lumen_html::contract::NodePath`] defines and the emitter
//! wrote onto each element as `data-lm`. The spawner walks the same IR in the
//! same order, so the same walk over the entity tree recovers the same paths,
//! and that is the whole binding: a path names one element and one entity.
//!
//! The walk runs every time the scene gains entities, which is once at boot
//! and again whenever a reconciler mounts something. It binds what it has not
//! bound yet and leaves the rest alone, so an `<if>` branch that turns on an
//! hour later adopts the markup that was prerendered for it.
//!
//! What the first walk does take out of the page is a `<for>` row the app
//! does not have. A list has a length, so a row past it belongs to no one and
//! would sit in the page for as long as it is open.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use lumen_core::components::{
    InlineStyle, LumenAttributes, LumenClasses, LumenId, LumenTag, TextContent,
};
use lumen_html::contract::{DATA_LM, NodePath, PathStep};
use lumen_html::tags::{html_tag_for, lm_class};
use lumen_scene::spawn::ForMarker;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Document, Element, Node};

/// How much of the page the runtime took over rather than rebuilt.
///
/// A prerendered page should be all adoptions and no creations on load: an
/// element the walk had to build is one the emitter and the runtime disagreed
/// about. Rows and branches that appear later are creations by definition,
/// which is why the counts are read at boot and not judged after.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HydrationReport {
    /// Nodes bound to markup that was already in the page.
    pub adopted: u32,
    /// Nodes the walk built, because no element carried their path.
    pub created: u32,
    /// Prerendered elements taken out of the page, because they stood for
    /// `<for>` rows the app does not have. Anything other than zero means
    /// the page was rendered from a longer list than the one running.
    pub removed: u32,
}

/// The binding between the world and the document.
///
/// Not a [`Resource`]: a [`web_sys::Element`] is a JavaScript handle and
/// neither `Send` nor `Sync`, so this lives in the world as a non-send
/// resource, the way a taffy tree does on the desktop.
pub struct NodeTable {
    document: Document,
    root: Element,
    root_entity: Entity,
    by_entity: HashMap<Entity, Element>,
    /// Which entity a node path belongs to, for resolving the element a DOM
    /// event landed on back to the entity that stands for it.
    by_path: HashMap<String, Entity>,
    /// Prerendered elements no entity has claimed yet, by node path.
    unclaimed: HashMap<String, Element>,
    /// How many row slots each bound `<for>` block has, by the block's node
    /// path. A block's children are entirely the reconciler's to decide, so a
    /// prerendered row past this count belongs to no one.
    for_rows: HashMap<String, u32>,
    report: HydrationReport,
    /// True until the first walk finishes. During it, a node the page has no
    /// element for is a disagreement between the emitter and the runtime and
    /// is reported; after it, building is what mounting a row means.
    hydrating: bool,
}

impl NodeTable {
    /// Bind `root_entity` to `root`, taking every prerendered element under
    /// it as a candidate for adoption.
    ///
    /// # Panics
    ///
    /// `root` is not in a document.
    pub fn adopting(root: Element, root_entity: Entity) -> Self {
        let document = root.owner_document().expect("the root is in a document");
        let mut unclaimed = HashMap::new();
        if let Some(path) = root.get_attribute(DATA_LM) {
            unclaimed.insert(path, root.clone());
        }
        if let Ok(list) = root.query_selector_all(&format!("[{DATA_LM}]")) {
            for i in 0..list.length() {
                let Some(element) = list.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
                    continue;
                };
                if let Some(path) = element.get_attribute(DATA_LM) {
                    unclaimed.insert(path, element);
                }
            }
        }
        Self {
            document,
            root,
            root_entity,
            by_entity: HashMap::new(),
            by_path: HashMap::new(),
            unclaimed,
            for_rows: HashMap::new(),
            report: HydrationReport::default(),
            hydrating: true,
        }
    }

    /// The element `entity` is, if it has been bound.
    pub fn element(&self, entity: Entity) -> Option<&Element> {
        self.by_entity.get(&entity)
    }

    /// The entity the element at `path` stands for.
    pub fn entity_at(&self, path: &str) -> Option<Entity> {
        self.by_path.get(path).copied()
    }

    /// What the walk has adopted and what it has built.
    pub fn report(&self) -> HydrationReport {
        self.report
    }
}

/// Everything the walk reads off an entity to build an element for it.
type NodeQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static LumenTag,
        Option<&'static LumenId>,
        Option<&'static LumenClasses>,
        Option<&'static TextContent>,
        Option<&'static LumenAttributes>,
        Option<&'static InlineStyle>,
    ),
>;

/// Give every entity of the scene an element: the one the page already has
/// for it, or a new one.
pub fn bind_new_nodes(
    mut table: NonSendMut<NodeTable>,
    nodes: NodeQuery<'_, '_>,
    children: Query<&Children>,
    rows: Query<&ForMarker>,
    added: Query<(), Added<LumenTag>>,
) {
    if added.iter().next().is_none() {
        return;
    }
    let root_entity = table.root_entity;
    let root = table.root.clone();
    // The page root is the one node whose element is known without a lookup:
    // it is the element the app was installed into.
    if !table.by_entity.contains_key(&root_entity) {
        let path = NodePath::root().to_string();
        table.unclaimed.remove(&path);
        table.report.adopted += 1;
        bind(&mut table, root_entity, path, root.clone());
    }
    bind_children(
        &mut table,
        root_entity,
        &NodePath::root(),
        &root,
        &nodes,
        &children,
        &rows,
    );
    if table.hydrating {
        table.hydrating = false;
        // After the walk, not before: an element is only an orphan once the
        // reconcilers have said what the page holds.
        sweep_orphan_rows(&mut table);
        let HydrationReport {
            adopted,
            created,
            removed,
        } = table.report;
        web_sys::console::info_1(&JsValue::from_str(&format!(
            "lumen: hydrated {adopted} nodes, built {created}, removed {removed}"
        )));
    }
}

/// Take out of the page every prerendered element standing for a `<for>` row
/// the app does not have.
///
/// A document rendered from a three-row list and adopted by an app holding
/// two would otherwise show the third row for as long as the page is open:
/// nothing claims it, so nothing ever takes it away.
///
/// Only rows. An `<if>` branch that is not mounted yet keeps its markup,
/// because a branch has no count to be past: the signal can turn true at any
/// point and the runtime adopts what the page already has.
fn sweep_orphan_rows(table: &mut NodeTable) {
    let orphans: Vec<String> = table
        .unclaimed
        .keys()
        .filter(|path| is_orphan_row(table, path))
        .cloned()
        .collect();
    for path in orphans {
        let Some(element) = table.unclaimed.remove(&path) else {
            continue;
        };
        element.remove();
        table.report.removed += 1;
    }
}

/// True when `path` is a row slot, or sits inside one, that its `<for>` block
/// does not have.
fn is_orphan_row(table: &NodeTable, path: &str) -> bool {
    let Ok(path) = path.parse::<NodePath>() else {
        return false;
    };
    let mut prefix = NodePath::root();
    // The first step is the page root, which `prefix` already is.
    for step in path.steps().iter().skip(1) {
        match step {
            PathStep::Child(index) => prefix = prefix.child(*index),
            PathStep::Row(index) => {
                if table
                    .for_rows
                    .get(&prefix.to_string())
                    .is_some_and(|slots| index >= slots)
                {
                    return true;
                }
                prefix = prefix.row(*index);
            }
        }
    }
    false
}

/// Bind every child of `entity`, then their children, depth first.
fn bind_children(
    table: &mut NodeTable,
    entity: Entity,
    path: &NodePath,
    element: &Element,
    nodes: &NodeQuery<'_, '_>,
    children: &Query<&Children>,
    rows: &Query<&ForMarker>,
) {
    // A `<for>` block's children are its rows, and a row's path says so.
    // Everything else counts children.
    let is_for = rows.get(entity).is_ok();
    let kids = children.get(entity).ok();
    if is_for {
        // Recorded even when the block has no children at all, which is a
        // block whose list is empty and whose prerendered rows are all
        // orphans.
        let slots = kids.map_or(0, |kids| kids.len());
        table.for_rows.insert(path.to_string(), slots as u32);
    }
    let Some(kids) = kids else {
        return;
    };
    // Where a newly built element goes: after the last sibling that has one.
    let mut previous: Option<Element> = None;
    for (index, child) in kids.iter().enumerate() {
        let index = index as u32;
        let child_path = if is_for {
            path.row(index)
        } else {
            path.child(index)
        };
        // An entity with no tag stands for no element, but it still counts:
        // dropping it would renumber every sibling after it.
        if nodes.get(child).is_err() {
            continue;
        }
        let Some(child_element) = bind_one(
            table,
            child,
            &child_path,
            element,
            previous.as_ref(),
            nodes,
            children,
        ) else {
            continue;
        };
        bind_children(
            table,
            child,
            &child_path,
            &child_element,
            nodes,
            children,
            rows,
        );
        previous = Some(child_element);
    }
}

/// The element for one entity: the one already bound, the prerendered one
/// that carries its path, or a new one built from the entity and inserted
/// after `previous`.
fn bind_one(
    table: &mut NodeTable,
    entity: Entity,
    path: &NodePath,
    parent: &Element,
    previous: Option<&Element>,
    nodes: &NodeQuery<'_, '_>,
    children: &Query<&Children>,
) -> Option<Element> {
    if let Some(element) = table.by_entity.get(&entity) {
        return Some(element.clone());
    }
    let text = path.to_string();
    if let Some(element) = table.unclaimed.remove(&text) {
        table.report.adopted += 1;
        bind(table, entity, text, element.clone());
        return Some(element);
    }

    if table.hydrating {
        // The page was rendered from the same IR, so a node with no element
        // means the two halves disagree about the tree. It is not fatal: the
        // subtree is built from the entity instead, which is what a page
        // emitted without a prerender does for every node.
        web_sys::console::warn_1(&JsValue::from_str(&format!(
            "lumen: no prerendered element for node {text}; building it instead"
        )));
    }
    let element = build(table, entity, &text, nodes, children)?;
    // An element goes after the last sibling that has one, or ahead of every
    // sibling element. Ahead of, not first: an element's own text is the node
    // before its children, and it stays there.
    let anchor: Option<Node> = match previous {
        Some(previous) => previous.next_sibling(),
        None => parent.first_element_child().map(Node::from),
    };
    if parent.insert_before(&element, anchor.as_ref()).is_err() {
        return None;
    }
    table.report.created += 1;
    bind(table, entity, text, element.clone());
    Some(element)
}

/// Record the binding both ways.
fn bind(table: &mut NodeTable, entity: Entity, path: String, element: Element) {
    table.by_path.insert(path, entity);
    table.by_entity.insert(entity, element);
}

/// Build the element an entity should be, with everything the entity already
/// says about it. The projection systems keep it in step from here.
fn build(
    table: &NodeTable,
    entity: Entity,
    path: &str,
    nodes: &NodeQuery<'_, '_>,
    children: &Query<&Children>,
) -> Option<Element> {
    let (tag, id, classes, text, attributes, style) = nodes.get(entity).ok()?;
    let html = html_tag_for(&tag.0)?;
    let element = table.document.create_element(html.name).ok()?;
    for (name, value) in html.fixed {
        let _ = element.set_attribute(name, value);
    }
    let _ = element.set_attribute("class", &class_value(&tag.0, classes));
    if let Some(id) = id {
        let _ = element.set_attribute("id", &id.0);
    }
    if let Some(attributes) = attributes {
        for (name, value) in &attributes.0 {
            let _ = element.set_attribute(name, value);
        }
    }
    if let Some(style) = style {
        let _ = element.set_attribute("style", &crate::project::style_value(style));
    }
    // Text goes in before any child element, which is where the emitter puts
    // it and so where the projection expects to find it.
    if let Some(text) = text.filter(|t| !t.0.is_empty() && !html.void) {
        let node = table.document.create_text_node(&text.0);
        let _ = element.append_child(&node);
    }
    let _ = element.set_attribute(DATA_LM, path);
    // Children of their own get bound by the walk that called this; nothing
    // to do here beyond leaving room for them.
    let _ = children;
    Some(element)
}

/// The `class` value for an entity: its tag class, then its own classes.
pub(crate) fn class_value(tag: &str, classes: Option<&LumenClasses>) -> String {
    let mut out = lm_class(tag);
    for class in classes.iter().flat_map(|c| c.0.iter()) {
        if class.is_empty() {
            continue;
        }
        out.push(' ');
        out.push_str(class);
    }
    out
}

/// Take out of the page every element whose entity is gone.
///
/// This is what unmounts an `<if mode="render">` branch and a `<for>` row the
/// reconciler dropped: the entity goes, and the element goes with it.
pub fn release_dead_nodes(
    mut table: NonSendMut<NodeTable>,
    mut removed: RemovedComponents<LumenTag>,
) {
    for entity in removed.read() {
        let Some(element) = table.by_entity.remove(&entity) else {
            continue;
        };
        if let Some(path) = element.get_attribute(DATA_LM) {
            table.by_path.remove(&path);
        }
        element.remove();
    }
}
