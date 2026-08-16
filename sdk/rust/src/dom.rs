//! Dynamic DOM handles for the Rust SDK.
//!
//! A [`Node`] is a safe wrapper over a live element in the running app. It
//! mirrors the DOM-parity surface every script host and the C-ABI expose
//! (design 4.1-4.8): query and traverse the tree, read and write attributes /
//! classes / text / inline style, build and rearrange nodes, inspect
//! post-layout geometry and computed style, and bind event handlers.
//!
//! The Rust SDK reaches the same host-neutral surface the C-ABI binds
//! (`lumen_script`) directly, so handles marshal as native `Option` / `Result`
//! / `Vec` / `HashMap` with no C string or pointer juggling. A [`Node`] is a
//! packed handle (index + generation); a stale handle reads back `None` /
//! `false` rather than panicking.
//!
//! Mutations are fire-and-forget: each queues on the same command bus the app
//! drains once per tick, so a `spawn` plus its chained edits materialize
//! together on the next tick. Read a value back after the app has ticked.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{lumen_core, lumen_script};

use std::collections::HashMap;
use std::sync::Arc;

use lumen_script::ScriptCommand;
use lumen_script::event as ev;
use lumen_script::introspect as ins;
use lumen_script::node_query as nq;

/// A post-layout box, `getBoundingClientRect`-class. `x` / `y` are local to
/// the parent origin; `client_*` are window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Local x (relative to the parent origin).
    pub x: f32,
    /// Local y.
    pub y: f32,
    /// Box width.
    pub width: f32,
    /// Box height.
    pub height: f32,
    /// Window-space x.
    pub client_x: f32,
    /// Window-space y.
    pub client_y: f32,
}

impl From<ins::NodeRect> for Rect {
    fn from(r: ins::NodeRect) -> Self {
        Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            client_x: r.client_x,
            client_y: r.client_y,
        }
    }
}

/// Scroll offsets and their travel limits for a scroll container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scroll {
    /// Current horizontal offset.
    pub x: f32,
    /// Current vertical offset.
    pub y: f32,
    /// Maximum horizontal offset.
    pub max_x: f32,
    /// Maximum vertical offset.
    pub max_y: f32,
}

impl From<ins::NodeScroll> for Scroll {
    fn from(s: ins::NodeScroll) -> Self {
        Scroll {
            x: s.x,
            y: s.y,
            max_x: s.max_x,
            max_y: s.max_y,
        }
    }
}

/// One matched stylesheet rule with cascade provenance (`matched_rules`).
#[derive(Debug, Clone)]
pub struct MatchedRule {
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

/// A result set from [`Node::query`] / [`query`], with Bevy-flavored
/// consumers. Cheap to hold; it is a snapshot of the matched handles.
#[derive(Debug, Clone)]
pub struct NodeQuery {
    nodes: Vec<u64>,
}

impl NodeQuery {
    fn new(nodes: Vec<u64>) -> Self {
        NodeQuery { nodes }
    }

    /// Number of matches.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Exactly one match, else an error (Bevy `single()` semantics).
    pub fn single(&self) -> Result<Node, String> {
        match self.nodes.as_slice() {
            [one] => Ok(Node(*one)),
            other => Err(format!("query matched {} nodes, expected 1", other.len())),
        }
    }

    /// The single match, or `None` for zero / many (fallible `single`).
    pub fn get_single(&self) -> Option<Node> {
        match self.nodes.as_slice() {
            [one] => Some(Node(*one)),
            _ => None,
        }
    }

    /// The first match in document order.
    pub fn first(&self) -> Option<Node> {
        self.nodes.first().copied().map(Node)
    }

    /// The match at `index`.
    pub fn nth(&self, index: usize) -> Option<Node> {
        self.nodes.get(index).copied().map(Node)
    }

    /// Iterate the matches.
    pub fn iter(&self) -> impl Iterator<Item = Node> + '_ {
        self.nodes.iter().copied().map(Node)
    }

    /// Materialize the matches to a `Vec<Node>`.
    pub fn collect(&self) -> Vec<Node> {
        self.nodes.iter().copied().map(Node).collect()
    }
}

impl IntoIterator for NodeQuery {
    type Item = Node;
    type IntoIter = std::vec::IntoIter<Node>;
    fn into_iter(self) -> Self::IntoIter {
        self.nodes
            .into_iter()
            .map(Node)
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// A live element handle. Copy-cheap; addresses one node by packed handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node(u64);

impl Node {
    /// Wrap a raw packed handle (round-trips [`Node::handle`]). Intended for
    /// handles obtained from this API; an arbitrary value reads back as
    /// invalid.
    pub fn from_handle(handle: u64) -> Self {
        Node(handle)
    }

    /// The raw packed handle (index + generation), for debugging / FFI
    /// round-trip.
    pub fn handle(self) -> u64 {
        self.0
    }

    /// Whether this handle still names a live node.
    pub fn is_valid(self) -> bool {
        nq::node_valid(self.0)
    }

    // -- query / traversal (design 4.1, 4.2) -------------------------------

    /// Run a CSS selector query over the whole tree.
    pub fn query(selector: &str) -> NodeQuery {
        query(selector)
    }

    /// Fast id lookup (`getElementById`).
    pub fn get_by_id(id: &str) -> Option<Node> {
        nq::run_get_by_id(id).map(Node)
    }

    /// The parent element.
    pub fn parent(self) -> Option<Node> {
        nq::node_parent(self.0).map(Node)
    }

    /// The ordered child elements.
    pub fn children(self) -> Vec<Node> {
        nq::node_children(self.0).into_iter().map(Node).collect()
    }

    /// The first child.
    pub fn first_child(self) -> Option<Node> {
        nq::node_first_child(self.0).map(Node)
    }

    /// The last child.
    pub fn last_child(self) -> Option<Node> {
        nq::node_last_child(self.0).map(Node)
    }

    /// The next sibling.
    pub fn next(self) -> Option<Node> {
        nq::node_next(self.0).map(Node)
    }

    /// The previous sibling.
    pub fn prev(self) -> Option<Node> {
        nq::node_prev(self.0).map(Node)
    }

    /// The nearest ancestor-or-self matching `selector`.
    pub fn closest(self, selector: &str) -> Option<Node> {
        nq::node_closest(self.0, selector).ok().flatten().map(Node)
    }

    // -- attributes / class / text (design 4.4) -----------------------------

    /// Read an attribute.
    pub fn get_attr(self, name: &str) -> Option<String> {
        nq::node_get_attr(self.0, name)
    }

    /// Set an attribute (chainable). Known names route to typed components.
    pub fn set_attr(self, name: &str, value: &str) -> Self {
        self.push(ScriptCommand::SetAttr {
            node: self.0,
            name: name.to_string(),
            value: value.to_string(),
        })
    }

    /// Remove an attribute (chainable).
    pub fn remove_attr(self, name: &str) -> Self {
        self.push(ScriptCommand::RemoveAttr {
            node: self.0,
            name: name.to_string(),
        })
    }

    /// Read the element id.
    pub fn id(self) -> Option<String> {
        nq::node_id(self.0)
    }

    /// Set the element id (chainable).
    pub fn set_id(self, id: &str) -> Self {
        self.set_attr("id", id)
    }

    /// Read the text content.
    pub fn text(self) -> Option<String> {
        nq::node_text(self.0)
    }

    /// Set the text content (chainable).
    pub fn set_text(self, text: &str) -> Self {
        self.push(ScriptCommand::SetNodeText {
            node: self.0,
            text: text.to_string(),
        })
    }

    /// Whether the class list contains `class`.
    pub fn has_class(self, class: &str) -> bool {
        nq::node_class_contains(self.0, class)
    }

    /// The full class list.
    pub fn classes(self) -> Vec<String> {
        ins::node_classes(self.0)
    }

    /// Add a class (chainable).
    pub fn add_class(self, class: &str) -> Self {
        self.push(ScriptCommand::ClassAdd {
            node: self.0,
            class: class.to_string(),
        })
    }

    /// Remove a class (chainable).
    pub fn remove_class(self, class: &str) -> Self {
        self.push(ScriptCommand::ClassRemove {
            node: self.0,
            class: class.to_string(),
        })
    }

    /// Toggle a class (chainable).
    pub fn toggle_class(self, class: &str) -> Self {
        self.push(ScriptCommand::ClassToggle {
            node: self.0,
            class: class.to_string(),
        })
    }

    /// Replace the whole class set (chainable).
    pub fn set_class(self, classes: &str) -> Self {
        self.set_attr("class", classes)
    }

    /// Serialize this node's children to `.lmn`-ish text (`innerHTML` read).
    pub fn inner_markup(self) -> String {
        ins::inner_markup(self.0)
    }

    /// Replace this node's children with the subtree parsed from `markup`
    /// (`innerHTML` write, chainable).
    ///
    /// Guarded: parsing needs the injected markup front-end, present on the
    /// from-source run path and a no-op on the precompiled-artifact path. Do
    /// NOT feed untrusted content -- this injects live markup (XSS-adjacent).
    pub fn set_inner_markup(self, markup: &str) -> Self {
        self.push(ScriptCommand::SetInnerMarkup {
            node: self.0,
            markup: markup.to_string(),
        })
    }

    // -- inline style (design 4.5) ------------------------------------------

    /// Read an inline style property (`element.style`).
    pub fn style_get(self, name: &str) -> Option<String> {
        nq::node_style_get(self.0, name)
    }

    /// Set an inline style property (chainable).
    pub fn set_style(self, name: &str, value: &str) -> Self {
        self.push(ScriptCommand::SetStyleProp {
            node: self.0,
            name: name.to_string(),
            value: value.to_string(),
        })
    }

    /// Remove an inline style property (chainable).
    pub fn remove_style(self, name: &str) -> Self {
        self.push(ScriptCommand::RemoveStyleProp {
            node: self.0,
            name: name.to_string(),
        })
    }

    /// The resolved value of one CSS property after the cascade.
    pub fn computed_style_of(self, name: &str) -> Option<String> {
        nq::node_computed_style(self.0, name)
    }

    // -- structure (design 4.3) ---------------------------------------------

    /// Append `child` under this node (`appendChild`, chainable).
    pub fn append(self, child: Node) -> Self {
        self.push(ScriptCommand::Insert {
            parent: self.0,
            node: child.0,
            before: 0,
        })
    }

    /// Insert `child` before `reference` under this node (`insertBefore`,
    /// chainable).
    pub fn insert_before(self, child: Node, reference: Node) -> Self {
        self.push(ScriptCommand::Insert {
            parent: self.0,
            node: child.0,
            before: reference.0,
        })
    }

    /// Attach this node under `parent` (reparent, chainable).
    pub fn set_parent(self, parent: Node) -> Self {
        self.push(ScriptCommand::Insert {
            parent: parent.0,
            node: self.0,
            before: 0,
        })
    }

    /// Alias for [`Node::set_parent`].
    pub fn move_to(self, parent: Node) -> Self {
        self.set_parent(parent)
    }

    /// Replace this node with `new` in the parent, despawning this subtree.
    /// Returns `new`.
    pub fn replace_with(self, new: Node) -> Node {
        self.push(ScriptCommand::ReplaceWith {
            old: self.0,
            new: new.0,
        });
        new
    }

    /// Detach and despawn this node and its subtree (`remove`). Terminal.
    pub fn remove(self) {
        self.push(ScriptCommand::RemoveNode { node: self.0 });
    }

    /// Deep-clone this subtree into a fresh detached node (`cloneNode(true)`).
    pub fn clone_deep(self) -> Node {
        let (handle, cmd) = nq::build_clone(self.0);
        nq::push_external_dom_command(cmd);
        Node(handle)
    }

    // -- introspection (design 4.7) -----------------------------------------

    /// Post-layout border-box, local + client (`getBoundingClientRect`).
    pub fn rect(self) -> Option<Rect> {
        ins::node_rect(self.0).map(Rect::from)
    }

    /// Content-box rect (inner box minus padding + border).
    pub fn content_rect(self) -> Option<Rect> {
        ins::node_content_rect(self.0).map(Rect::from)
    }

    /// Scroll offsets and their limits.
    pub fn scroll(self) -> Option<Scroll> {
        ins::node_scroll(self.0).map(Scroll::from)
    }

    /// Effective visibility after `Visible(false)` / `display:none`.
    pub fn is_visible(self) -> bool {
        ins::node_is_visible(self.0)
    }

    /// Resolved stacking order.
    pub fn z_index(self) -> i32 {
        ins::node_z_index(self.0)
    }

    /// Every resolved CSS property, keyed by name.
    pub fn computed_style(self) -> HashMap<String, String> {
        ins::node_computed_style_map(self.0).into_iter().collect()
    }

    /// The stylesheet rules that matched this node, with provenance.
    pub fn matched_rules(self) -> Vec<MatchedRule> {
        ins::node_matched_rules(self.0)
            .into_iter()
            .map(|m| MatchedRule {
                selector: m.selector,
                specificity: m.specificity,
                source: m.source,
                source_order: m.source_order,
                declarations: m.declarations,
            })
            .collect()
    }

    /// The `element.style` override map.
    pub fn inline_style(self) -> HashMap<String, String> {
        ins::node_inline_style(self.0).into_iter().collect()
    }

    /// The full attribute map.
    pub fn attrs(self) -> HashMap<String, String> {
        ins::node_attrs(self.0).into_iter().collect()
    }

    /// The raw `(index, generation)` for debugging / handle round-trip.
    pub fn entity_id(self) -> Option<(u32, u32)> {
        ins::node_entity_id(self.0)
    }

    /// Names of the whitelisted Lumen components present on this node.
    pub fn components(self) -> Vec<String> {
        ins::node_components(self.0)
    }

    /// One component's public fields as a map. `Err` names an unknown /
    /// non-whitelisted component; `Ok(None)` means whitelisted but absent.
    pub fn component(self, name: &str) -> Result<Option<HashMap<String, String>>, String> {
        ins::node_component(self.0, name).map(|opt| opt.map(|v| v.into_iter().collect()))
    }

    /// Serialize this subtree to `.lmn`-ish text (`outerHTML` read).
    pub fn outer_markup(self) -> String {
        ins::outer_markup(self.0)
    }

    // -- events (design 4.6) ------------------------------------------------

    /// Bind `handler` to this node for `event_type` (bubble / target phase).
    /// Returns a [`Listener`]; call [`Listener::off`] to unbind.
    pub fn on(
        self,
        event_type: &str,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> Listener {
        self.bind(event_type, false, handler)
    }

    /// Bind a capture-phase listener.
    pub fn on_capture(
        self,
        event_type: &str,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> Listener {
        self.bind(event_type, true, handler)
    }

    fn bind(
        self,
        event_type: &str,
        capture: bool,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> Listener {
        let cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || handler(&Event));
        let token = ev::register_native_binding(self.0, event_type.to_string(), capture, cb);
        Listener { token }
    }

    fn push(self, cmd: ScriptCommand) -> Self {
        nq::push_external_dom_command(cmd);
        self
    }
}

/// A bound event listener. Drop it or call [`Listener::off`] to unbind
/// (`removeEventListener`).
#[derive(Debug)]
pub struct Listener {
    token: u64,
}

impl Listener {
    /// Unbind the listener.
    pub fn off(self) {
        ev::unregister_binding(self.token);
    }

    /// The raw off token.
    pub fn token(&self) -> u64 {
        self.token
    }
}

/// The event passed to an [`Node::on`] handler. Reads the event being
/// dispatched; valid only for the duration of the handler call.
#[derive(Debug, Clone, Copy)]
pub struct Event;

impl Event {
    /// The node the event was dispatched to.
    pub fn target(&self) -> Node {
        Node(ev::event_target())
    }

    /// The node whose listener is currently running.
    pub fn current_target(&self) -> Node {
        Node(ev::event_current_target())
    }

    /// The event type (`"click"`, `"keydown"`, ...).
    pub fn event_type(&self) -> String {
        ev::event_type()
    }

    /// Pointer position local to the target `(x, y)`.
    pub fn position(&self) -> (f64, f64) {
        ev::event_position_local()
    }

    /// Pointer position in window coordinates `(x, y)`.
    pub fn client_position(&self) -> (f64, f64) {
        ev::event_position_client()
    }

    /// The key for key events.
    pub fn key(&self) -> String {
        ev::event_key()
    }

    /// The value for input / change events.
    pub fn value(&self) -> String {
        ev::event_value()
    }

    /// The button for pointer events (0 primary, 1 middle, 2 secondary, -1
    /// none).
    pub fn button(&self) -> i64 {
        ev::event_button()
    }

    /// Wheel delta `(dx, dy)`.
    pub fn delta(&self) -> (f64, f64) {
        ev::event_delta()
    }

    /// Modifier state `(shift, ctrl, alt, super)`.
    pub fn modifiers(&self) -> (bool, bool, bool, bool) {
        ev::event_modifiers()
    }

    /// Cancel the event's default action.
    pub fn prevent_default(&self) {
        ev::event_prevent_default();
    }

    /// Stop propagation to the next node.
    pub fn stop_propagation(&self) {
        ev::event_stop_propagation();
    }

    /// Stop the remaining handlers everywhere.
    pub fn stop_immediate_propagation(&self) {
        ev::event_stop_immediate_propagation();
    }
}

// -- free entry points (design 4.1, 4.7) -----------------------------------

/// Run a CSS selector query over the whole tree.
pub fn query(selector: &str) -> NodeQuery {
    let nodes = nq::run_query(selector)
        .map(|r| r.collect())
        .unwrap_or_default();
    NodeQuery::new(nodes)
}

/// Fast id lookup (`getElementById`).
pub fn get_by_id(id: &str) -> Option<Node> {
    nq::run_get_by_id(id).map(Node)
}

/// Create a fresh detached element with markup `tag` (`createElement`).
/// Attach it with [`Node::append`] / [`Node::set_parent`].
pub fn spawn(tag: &str) -> Node {
    let (handle, cmd) = nq::build_spawn(tag);
    nq::push_external_dom_command(cmd);
    Node(handle)
}

/// The current focus target.
pub fn focused_node() -> Option<Node> {
    nq::focused_node().map(Node)
}

/// The current hover target.
pub fn hovered_node() -> Option<Node> {
    nq::hovered_node().map(Node)
}

/// Whole-tree structural dump (id / tag / classes / rect). An inspection call.
pub fn dump_tree() -> String {
    ins::dump_tree()
}

/// The whole signal set as `(name, value)` pairs. An inspection call.
pub fn signals_all() -> HashMap<String, String> {
    ins::signals_all().into_iter().collect()
}

/// The `document` namespace (design 4.8): document-scoped entry points.
pub mod document {
    use super::{Node, NodeQuery, get_by_id as g, lumen_script, query as q, spawn as s};

    /// The root element (`document.documentElement`).
    pub fn root() -> Option<Node> {
        lumen_script::node_query::run_document().map(Node::from_handle)
    }

    /// Run a selector query.
    pub fn query(selector: &str) -> NodeQuery {
        q(selector)
    }

    /// Fast id lookup.
    pub fn get_by_id(id: &str) -> Option<Node> {
        g(id)
    }

    /// Create a fresh detached element.
    pub fn spawn(tag: &str) -> Node {
        s(tag)
    }

    /// The current focus target.
    pub fn focused() -> Option<Node> {
        super::focused_node()
    }

    /// The current hover target.
    pub fn hovered() -> Option<Node> {
        super::hovered_node()
    }
}

/// The `window` namespace (design 4.8): navigation + window state.
pub mod window {
    use super::lumen_core;

    /// Navigate to a page path (`window.location.href = ...`).
    pub fn set_href(path: &str) {
        lumen_core::nav::navigate(path);
    }

    /// The current page path.
    pub fn href() -> String {
        lumen_core::nav::current()
    }

    /// Re-navigate to the current page.
    pub fn reload() {
        lumen_core::nav::navigate(lumen_core::nav::current());
    }

    /// The window title.
    pub fn title() -> String {
        lumen_core::window_state::title()
    }

    /// Set the window title.
    pub fn set_title(title: &str) {
        lumen_core::window_state::set_title(title);
    }

    /// The window size in logical pixels `(width, height)`.
    pub fn size() -> (f32, f32) {
        lumen_core::window_state::size()
    }

    /// Resize the window (logical pixels).
    pub fn set_size(width: f32, height: f32) {
        lumen_core::window_state::set_size(width, height);
    }

    /// The device-pixel ratio.
    pub fn dpr() -> f32 {
        lumen_core::window_state::dpr()
    }
}

/// The `history` namespace (design 4.8).
pub mod history {
    use super::lumen_core;

    /// Step back one entry.
    pub fn back() {
        lumen_core::nav::back();
    }

    /// Step forward one entry.
    pub fn forward() {
        lumen_core::nav::forward();
    }

    /// Step `delta` entries (negative back, positive forward).
    pub fn go(delta: i32) {
        for _ in 0..delta.unsigned_abs() {
            if delta < 0 {
                lumen_core::nav::back();
            } else {
                lumen_core::nav::forward();
            }
        }
    }
}
