//! The workspace model behind Lumen's dock module.
//!
//! A [`Workspace`] is everything a user arranges: each `<dock>` area of the
//! page as a tree of [`Split`]s and tab [`Stack`]s, the panels popped out
//! into windows of their own, and where every hidden or popped-out panel
//! goes back to. It is plain data with no engine types. The dock module
//! realizes it into elements; this crate only says what the arrangement is
//! and how each operation changes it.
//!
//! Every operation is a method that either changes the workspace and reports
//! what happened as a list of [`Effect`]s, or refuses with an [`OpError`] and
//! leaves it as it was. After any operation the invariants
//! [`Workspace::check`] tests hold:
//!
//! - each panel appears at most once across every dock and window;
//! - no split has fewer than two children;
//! - no stack is empty, except a `center` stack and a dock's root;
//! - every window holds at least one panel;
//! - every stack's `active` panel is one of its panels;
//! - stack, split and window ids are unique.
//!
//! The format a workspace saves to is JSON ([`Workspace::to_json`],
//! [`Workspace::from_json`]); [`place`] turns a saved window rectangle into a
//! position on the monitors present now.

#![warn(missing_docs)]

mod format;
mod place;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

pub use format::{FORMAT_VERSION, LayoutFile, RestoreError};
pub use place::{Monitor, Placement, place, record};

/// A panel's identity: its element id.
pub type PanelId = String;

/// The direction a split lays its children out along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// Side by side.
    Row,
    /// One above the other.
    Column,
}

/// When a stack shows its tab strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabsMode {
    /// A strip for two or more panels; one panel shows a header instead.
    #[default]
    Auto,
    /// Always a strip.
    Always,
    /// Never a strip.
    Never,
}

impl TabsMode {
    fn is_auto(&self) -> bool {
        *self == TabsMode::Auto
    }

    /// Read a `tabs` attribute value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// A side of a stack, a split or a whole dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The left edge.
    Left,
    /// The right edge.
    Right,
    /// The top edge.
    Top,
    /// The bottom edge.
    Bottom,
}

impl Side {
    /// The axis a split along this side lays out on.
    pub fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Row,
            Side::Top | Side::Bottom => Axis::Column,
        }
    }

    /// Whether the new node goes before the one it is beside.
    pub fn before(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }

    /// Read a side name: `left`, `right`, `top`, `bottom`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            "top" => Some(Side::Top),
            "bottom" => Some(Side::Bottom),
            _ => None,
        }
    }
}

/// A tab stack: panels that share one area, one of them showing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    /// The stack's id, stable across saves.
    #[serde(rename = "stack")]
    pub id: String,
    /// Its panels, in tab order.
    #[serde(default)]
    pub panels: Vec<PanelId>,
    /// The panel showing; `None` only while the stack is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<PanelId>,
    /// The center stack: never removed when it empties, and the one that
    /// absorbs a resize.
    #[serde(default, skip_serializing_if = "is_false")]
    pub center: bool,
    /// Shows only its header.
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapsed: bool,
    /// When it shows a tab strip.
    #[serde(default, skip_serializing_if = "TabsMode::is_auto")]
    pub tabs: TabsMode,
}

impl Stack {
    /// An empty stack called `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            panels: Vec::new(),
            active: None,
            center: false,
            collapsed: false,
            tabs: TabsMode::Auto,
        }
    }

    /// A stack called `id` holding `panels`, the first one showing.
    pub fn with(
        id: impl Into<String>,
        panels: impl IntoIterator<Item = impl Into<PanelId>>,
    ) -> Self {
        let mut stack = Self::new(id);
        stack.panels = panels.into_iter().map(Into::into).collect();
        stack.active = stack.panels.first().cloned();
        stack
    }

    fn fix_active(&mut self) {
        if !self
            .active
            .as_ref()
            .is_some_and(|a| self.panels.contains(a))
        {
            self.active = self.panels.first().cloned();
        }
    }
}

/// Children laid out along an axis, with a gutter between every two.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    /// The axis.
    #[serde(rename = "split")]
    pub axis: Axis,
    /// The split's id, stable across saves.
    pub id: String,
    /// The children, in order.
    pub children: Vec<Child>,
}

/// One child of a split and its size in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Child {
    /// Its size along the split's axis in logical pixels; `None` sizes it
    /// from its content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<f32>,
    /// How much of the split's spare room it takes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub grow: f32,
    /// The node.
    pub node: Node,
}

impl Child {
    /// A child that takes `grow` of the spare room from no basis.
    pub fn grow(node: Node, grow: f32) -> Self {
        Self {
            basis: None,
            grow,
            node,
        }
    }
}

/// A node of a dock's tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Node {
    /// A split.
    Split(Split),
    /// A tab stack.
    Stack(Stack),
}

impl Node {
    /// The node's id.
    pub fn id(&self) -> &str {
        match self {
            Node::Split(s) => &s.id,
            Node::Stack(s) => &s.id,
        }
    }

    /// Every stack under this node, the node included, in tree order.
    pub fn stacks(&self) -> Vec<&Stack> {
        let mut out = Vec::new();
        self.walk(&mut |n| {
            if let Node::Stack(s) = n {
                out.push(s);
            }
        });
        out
    }

    fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        f(self);
        if let Node::Split(split) = self {
            for c in &split.children {
                c.node.walk(f);
            }
        }
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Node::Split(split) => split.children.iter_mut().find_map(|c| c.node.find_mut(id)),
            Node::Stack(_) => None,
        }
    }

    fn find(&self, id: &str) -> Option<&Node> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Node::Split(split) => split.children.iter().find_map(|c| c.node.find(id)),
            Node::Stack(_) => None,
        }
    }

    /// Every panel under the node, in tree order.
    pub fn panels(&self) -> Vec<&PanelId> {
        self.stacks().into_iter().flat_map(|s| &s.panels).collect()
    }

    /// The split holding the node `id` and the node's slot in it.
    fn parent_of(&self, id: &str) -> Option<(&Split, usize)> {
        let Node::Split(split) = self else {
            return None;
        };
        for (i, c) in split.children.iter().enumerate() {
            if c.node.id() == id {
                return Some((split, i));
            }
            if let Some(found) = c.node.parent_of(id) {
                return Some(found);
            }
        }
        None
    }

    /// Drop empty stacks that may go and splits left with one child.
    /// Returns false when the node itself is an empty stack that may go.
    fn prune(&mut self) -> bool {
        match self {
            Node::Stack(s) => s.center || !s.panels.is_empty(),
            Node::Split(split) => {
                split.children.retain_mut(|c| c.node.prune());
                match split.children.len() {
                    0 => false,
                    1 => {
                        let only = split.children.pop().expect("one child").node;
                        *self = only;
                        true
                    }
                    _ => true,
                }
            }
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// One `<dock>` area of the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockArea {
    /// Its tree.
    pub root: Node,
    /// The panel filling the dock, if one is maximized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<PanelId>,
}

impl DockArea {
    /// A dock whose tree is `root`.
    pub fn new(root: Node) -> Self {
        Self {
            root,
            maximized: None,
        }
    }
}

/// A rectangle in logical pixels: the position of a window's outer top-left
/// corner from its monitor's origin, and its inner size.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    /// From the monitor's left edge.
    pub x: f32,
    /// From the monitor's top edge.
    pub y: f32,
    /// Inner width.
    pub w: f32,
    /// Inner height.
    pub h: f32,
}

impl Serialize for Rect {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        [self.x, self.y, self.w, self.h].serialize(s)
    }
}

impl<'de> Deserialize<'de> for Rect {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let [x, y, w, h] = <[f32; 4]>::deserialize(d)?;
        Ok(Rect { x, y, w, h })
    }
}

/// The monitor a window was on: its name, where it sat on the desktop in
/// physical pixels, and its scale, so a restore on a changed set of monitors
/// can still place the window sensibly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorKey {
    /// The platform's name for it.
    pub name: String,
    /// Its top-left corner in physical desktop pixels.
    pub origin: [i32; 2],
    /// Its logical to physical multiplier.
    pub scale: f32,
}

/// Where a window was: its rectangle, its monitor and whether the platform
/// had it maximized.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowPlace {
    /// Its rectangle; `None` lets the dock pick one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    /// Its monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MonitorKey>,
    /// Maximized by the platform.
    #[serde(default, skip_serializing_if = "is_false")]
    pub maximized: bool,
}

/// A window of its own holding panels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
    /// The window's id in the layout.
    pub id: String,
    /// The dock its panels came from, under which the window's content sits
    /// in the document; `None` for a window of a panel declared outside any
    /// dock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// Where it is.
    #[serde(flatten)]
    pub place: WindowPlace,
    /// Its tree.
    pub root: Node,
}

/// A spot in a dock a panel goes back to: its stack and its tab index, and,
/// for when that stack is gone, the split it was in and its slot there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    /// The dock.
    pub dock: String,
    /// The stack.
    pub stack: String,
    /// The tab index in it.
    #[serde(default)]
    pub index: usize,
    /// The split the stack was in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<String>,
    /// The stack's slot in that split.
    #[serde(default)]
    pub slot: usize,
}

/// Where a panel that is hidden or popped out goes back to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Home {
    /// Its spot in a dock: where `attach` and a dock-back put it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dock: Option<Spot>,
    /// The window it was hidden from: `show` opens a window there again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowPlace>,
}

/// What an app keeps about a panel it creates at run time, so a restore can
/// ask the app to create it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelMeta {
    /// The app's name for what the panel is.
    #[serde(default)]
    pub kind: String,
    /// The app's state for it, as a string.
    #[serde(default)]
    pub state: String,
}

/// Where a panel goes the first time it shows, when no layout places it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// The center stack of its dock.
    #[default]
    Center,
    /// A new stack along an edge of its dock.
    Edge(Side),
    /// A window of its own.
    Window,
    /// A tab beside another panel.
    Beside(PanelId),
}

impl Place {
    /// Read a `place` attribute value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "center" => Some(Place::Center),
            "window" => Some(Place::Window),
            other => Side::parse(other).map(Place::Edge).or_else(|| {
                other
                    .strip_prefix("beside:")
                    .map(|p| Place::Beside(p.to_string()))
            }),
        }
    }
}

/// What the model needs to know about a panel to place it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelInfo {
    /// The dock it was declared in; `None` for a panel declared outside any
    /// dock, which shows only in a window of its own.
    pub dock: Option<String>,
    /// Where it goes when nothing else places it.
    pub place: Place,
    /// Whether it docks. A panel that does not shows only in a window of its
    /// own that takes no other panel.
    pub dockable: bool,
}

/// Where a panel is shown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Where {
    /// In a dock.
    Dock(String),
    /// In a window of its own.
    Window(String),
    /// Not shown.
    Hidden,
}

impl Where {
    /// The name the `panelmove` event gives it: `dock`, `window` or
    /// `hidden`.
    pub fn kind(&self) -> &'static str {
        match self {
            Where::Dock(_) => "dock",
            Where::Window(_) => "window",
            Where::Hidden => "hidden",
        }
    }
}

/// Something an operation did, for the events the dock raises.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// The panel entered view.
    Shown(PanelId),
    /// The panel left view.
    Hidden(PanelId),
    /// The panel became its stack's showing tab.
    Activated(PanelId),
    /// The panel moved to another dock or window.
    Moved {
        /// The panel.
        panel: PanelId,
        /// Where it was.
        from: Where,
        /// Where it is now.
        to: Where,
    },
    /// A window was added to the layout.
    WindowOpened(String),
    /// A window left the layout.
    WindowClosed(String),
}

/// Why an operation refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpError {
    /// No panel by that id is shown, or the model does not know it.
    UnknownPanel(String),
    /// The target names no stack, split, dock or panel.
    UnknownTarget(String),
    /// The panel does not dock.
    NotDockable(String),
    /// The operation needs the panel in a window and it is not in one.
    NotInWindow(String),
    /// The operation needs the panel in a dock and it is not in one.
    NotInDock(String),
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpError::UnknownPanel(p) => write!(f, "no panel '{p}' is shown"),
            OpError::UnknownTarget(t) => write!(f, "nothing called '{t}' to put it beside"),
            OpError::NotDockable(p) => write!(f, "panel '{p}' does not dock"),
            OpError::NotInWindow(p) => write!(f, "panel '{p}' is not in a window of its own"),
            OpError::NotInDock(p) => write!(f, "panel '{p}' is not in a dock"),
        }
    }
}

impl std::error::Error for OpError {}

/// Where an operation puts a panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A tab of a stack, at an index (clamped to its length).
    Tab {
        /// The stack.
        stack: String,
        /// The tab index.
        index: usize,
    },
    /// A new stack beside a stack or a split, on one side.
    Edge {
        /// The stack or split.
        node: String,
        /// The side.
        side: Side,
    },
    /// A new stack along one edge of a whole dock.
    RootEdge {
        /// The dock.
        dock: String,
        /// The side.
        side: Side,
    },
    /// A window of its own.
    NewWindow(WindowPlace),
}

/// The container a stack is in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    /// A dock, by id.
    Dock(String),
    /// A window, by id.
    Window(String),
}

impl Container {
    fn to_where(&self) -> Where {
        match self {
            Container::Dock(d) => Where::Dock(d.clone()),
            Container::Window(w) => Where::Window(w.clone()),
        }
    }
}

/// Where a panel is in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The dock or window.
    pub container: Container,
    /// The stack.
    pub stack: String,
    /// Its tab index.
    pub index: usize,
}

/// Everything a user has arranged: the docks, the windows, and where hidden
/// and popped-out panels go back to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// The app's layout version, which a saved layout must match to be used.
    #[serde(rename = "layout-version", default)]
    pub layout_version: u32,
    /// The named layout this one came from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The docks, by `<dock>` id.
    #[serde(default)]
    pub docks: BTreeMap<String, DockArea>,
    /// The windows of their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowLayout>,
    /// Where hidden and popped-out panels go back to.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub homes: BTreeMap<PanelId, Home>,
    /// Panels the app creates at run time, with what recreates them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panels: BTreeMap<PanelId, PanelMeta>,
}

impl Workspace {
    // ---- reading ----------------------------------------------------------

    /// Where `panel` is in the tree, if it is shown.
    pub fn find(&self, panel: &str) -> Option<Location> {
        for (id, dock) in &self.docks {
            if let Some((stack, index)) = locate(&dock.root, panel) {
                return Some(Location {
                    container: Container::Dock(id.clone()),
                    stack,
                    index,
                });
            }
        }
        for w in &self.windows {
            if let Some((stack, index)) = locate(&w.root, panel) {
                return Some(Location {
                    container: Container::Window(w.id.clone()),
                    stack,
                    index,
                });
            }
        }
        None
    }

    /// Where `panel` is shown.
    pub fn where_is(&self, panel: &str) -> Where {
        self.find(panel)
            .map(|l| l.container.to_where())
            .unwrap_or(Where::Hidden)
    }

    /// Whether `panel` is in the tree.
    pub fn is_shown(&self, panel: &str) -> bool {
        self.find(panel).is_some()
    }

    /// The stack called `id`, wherever it is.
    pub fn stack(&self, id: &str) -> Option<&Stack> {
        self.roots().find_map(|(_, root)| match root.find(id) {
            Some(Node::Stack(s)) => Some(s),
            _ => None,
        })
    }

    fn stack_mut(&mut self, id: &str) -> Option<&mut Stack> {
        self.roots_mut()
            .find_map(|(_, root)| match root.find_mut(id) {
                Some(Node::Stack(s)) => Some(s),
                _ => None,
            })
    }

    /// The window called `id`.
    pub fn window(&self, id: &str) -> Option<&WindowLayout> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Every container's root, docks first by id, then windows in order.
    pub fn roots(&self) -> impl Iterator<Item = (Container, &Node)> {
        self.docks
            .iter()
            .map(|(id, d)| (Container::Dock(id.clone()), &d.root))
            .chain(
                self.windows
                    .iter()
                    .map(|w| (Container::Window(w.id.clone()), &w.root)),
            )
    }

    fn roots_mut(&mut self) -> impl Iterator<Item = (Container, &mut Node)> {
        self.docks
            .iter_mut()
            .map(|(id, d)| (Container::Dock(id.clone()), &mut d.root))
            .chain(
                self.windows
                    .iter_mut()
                    .map(|w| (Container::Window(w.id.clone()), &mut w.root)),
            )
    }

    /// Every shown panel, docks first, in tree order.
    pub fn shown_panels(&self) -> Vec<PanelId> {
        self.roots()
            .flat_map(|(_, r)| r.panels().into_iter().cloned().collect::<Vec<_>>())
            .collect()
    }

    /// The stacks of a container in tree order: what F6 cycles through.
    pub fn stacks_of(&self, container: &Container) -> Vec<&Stack> {
        let root = match container {
            Container::Dock(d) => self.docks.get(d).map(|d| &d.root),
            Container::Window(w) => self.window(w).map(|w| &w.root),
        };
        root.map(Node::stacks).unwrap_or_default()
    }

    /// The panel showing in the stack that holds `panel`.
    pub fn active_of(&self, panel: &str) -> Option<&PanelId> {
        let loc = self.find(panel)?;
        self.stack(&loc.stack)?.active.as_ref()
    }

    // ---- invariants -------------------------------------------------------

    /// Check the invariants every operation keeps. `Err` names the first
    /// that does not hold.
    pub fn check(&self) -> Result<(), String> {
        let mut panels = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for w in &self.windows {
            if !ids.insert(format!("window:{}", w.id)) {
                return Err(format!("window id '{}' twice", w.id));
            }
            if w.root.panels().is_empty() {
                return Err(format!("window '{}' holds no panel", w.id));
            }
        }
        for (container, root) in self.roots() {
            let mut bad = None;
            root.walk(&mut |n| {
                if bad.is_some() {
                    return;
                }
                if !ids.insert(n.id().to_string()) {
                    bad = Some(format!("node id '{}' twice", n.id()));
                    return;
                }
                match n {
                    Node::Split(s) if s.children.len() < 2 => {
                        bad = Some(format!("split '{}' has fewer than two children", s.id));
                    }
                    Node::Stack(s) => {
                        let is_root = std::ptr::eq(n, root);
                        let may_be_empty =
                            s.center || (is_root && matches!(container, Container::Dock(_)));
                        if s.panels.is_empty() && !may_be_empty {
                            bad = Some(format!("stack '{}' is empty", s.id));
                        } else if s.panels.is_empty() != s.active.is_none()
                            || s.active.as_ref().is_some_and(|a| !s.panels.contains(a))
                        {
                            bad = Some(format!("stack '{}' shows a panel it does not hold", s.id));
                        } else {
                            for p in &s.panels {
                                if !panels.insert(p.clone()) {
                                    bad = Some(format!("panel '{p}' is shown twice"));
                                }
                            }
                        }
                    }
                    Node::Split(_) => {}
                }
            });
            if let Some(bad) = bad {
                return Err(bad);
            }
        }
        for (id, dock) in &self.docks {
            if let Some(m) = &dock.maximized
                && !locate(&dock.root, m).is_some()
            {
                return Err(format!(
                    "dock '{id}' maximizes '{m}', which it does not hold"
                ));
            }
        }
        Ok(())
    }

    // ---- operations -------------------------------------------------------

    /// Make `panel` its stack's showing tab.
    pub fn activate(&mut self, panel: &str) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        let stack = self.stack_mut(&loc.stack).expect("located stack");
        if stack.active.as_deref() == Some(panel) {
            return Ok(Vec::new());
        }
        stack.active = Some(panel.to_string());
        Ok(vec![Effect::Activated(panel.to_string())])
    }

    /// Take `panel` out of view, remembering where it was.
    pub fn hide(&mut self, panel: &str) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        let mut effects = Vec::new();
        self.remember(panel, &loc);
        self.take_out(panel, &loc);
        effects.push(Effect::Hidden(panel.to_string()));
        effects.extend(self.prune());
        Ok(effects)
    }

    /// Bring `panel` into view: where it was hidden from, else where
    /// `info` places it. A shown panel is made its stack's showing tab.
    pub fn show(&mut self, panel: &str, info: &PanelInfo) -> Result<Vec<Effect>, OpError> {
        if self.is_shown(panel) {
            return self.activate(panel);
        }
        let home = self.homes.remove(panel).unwrap_or_default();
        let to = if let Some(place) = home.window.clone() {
            Where::Window(self.open_window(panel, info, place))
        } else if !info.dockable || info.dock.is_none() {
            Where::Window(self.open_window(panel, info, WindowPlace::default()))
        } else if let Some(dock) = home
            .dock
            .as_ref()
            .and_then(|spot| self.put_at_spot(panel, spot))
        {
            Where::Dock(dock)
        } else {
            self.put_by_place(panel, info)?
        };
        if let Where::Window(_) = to
            && let Some(spot) = home.dock
        {
            // A panel shown in a window keeps the dock spot it docks back to.
            self.homes.insert(
                panel.to_string(),
                Home {
                    dock: Some(spot),
                    window: None,
                },
            );
        }
        let mut effects = vec![Effect::Shown(panel.to_string())];
        if let Where::Window(id) = &to {
            effects.push(Effect::WindowOpened(id.clone()));
        }
        effects.push(Effect::Moved {
            panel: panel.to_string(),
            from: Where::Hidden,
            to,
        });
        Ok(effects)
    }

    /// Move `panel` to `target`.
    pub fn move_to(
        &mut self,
        panel: &str,
        target: &Target,
        info: &PanelInfo,
    ) -> Result<Vec<Effect>, OpError> {
        let from = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        if !info.dockable && !matches!(target, Target::NewWindow(_)) {
            return Err(OpError::NotDockable(panel.into()));
        }
        // Validate the target before anything moves.
        match target {
            Target::Tab { stack, .. } => {
                if self.stack(stack).is_none() {
                    return Err(OpError::UnknownTarget(stack.clone()));
                }
            }
            Target::Edge { node, .. } => {
                if !self.roots().any(|(_, r)| r.find(node).is_some()) {
                    return Err(OpError::UnknownTarget(node.clone()));
                }
                // Beside its own one-panel stack: nothing to do.
                if *node == from.stack && self.stack(node).is_some_and(|s| s.panels.len() == 1) {
                    return Ok(Vec::new());
                }
            }
            Target::RootEdge { dock, .. } => {
                if !self.docks.contains_key(dock) {
                    return Err(OpError::UnknownTarget(dock.clone()));
                }
            }
            Target::NewWindow(_) => {}
        }
        let from_where = from.container.to_where();
        if let Target::Tab { stack, index } = target
            && *stack == from.stack
        {
            let s = self.stack_mut(stack).expect("validated");
            let p = s.panels.remove(from.index);
            let at = (*index).min(s.panels.len());
            s.panels.insert(at, p);
            s.active = Some(panel.to_string());
            return Ok(vec![Effect::Activated(panel.to_string())]);
        }
        if let Container::Dock(_) = from.container {
            self.remember_spot(panel, &from);
        }
        self.take_out(panel, &from);
        let to = match target {
            Target::Tab { stack, index } => {
                let s = self.stack_mut(stack).expect("validated");
                let at = (*index).min(s.panels.len());
                s.panels.insert(at, panel.to_string());
                s.active = Some(panel.to_string());
                self.container_of_node(stack).expect("validated").to_where()
            }
            Target::Edge { node, side } => {
                let id = self.fresh("s");
                let stack = Stack::with(id, [panel]);
                let container = self.container_of_node(node).expect("validated");
                self.insert_beside(&container, node, *side, Node::Stack(stack));
                container.to_where()
            }
            Target::RootEdge { dock, side } => {
                let id = self.fresh("s");
                let stack = Stack::with(id, [panel]);
                self.insert_at_root_edge(dock, *side, Node::Stack(stack));
                Where::Dock(dock.clone())
            }
            Target::NewWindow(place) => {
                let home = info.dock.clone().or_else(|| match &from.container {
                    Container::Dock(d) => Some(d.clone()),
                    Container::Window(w) => self.window(w).and_then(|w| w.home.clone()),
                });
                let id = self.fresh("w");
                let stack_id = self.fresh("s");
                self.windows.push(WindowLayout {
                    id: id.clone(),
                    home,
                    place: place.clone(),
                    root: Node::Stack(Stack::with(stack_id, [panel])),
                });
                Where::Window(id)
            }
        };
        if matches!(to, Where::Dock(_)) {
            // Docked again: the spot it would go back to is where it is.
            self.homes.remove(panel);
        }
        let mut effects = Vec::new();
        if let Where::Window(id) = &to {
            effects.push(Effect::WindowOpened(id.clone()));
        }
        effects.extend(self.prune());
        effects.push(Effect::Moved {
            panel: panel.to_string(),
            from: from_where,
            to,
        });
        Ok(effects)
    }

    /// Pop `panel` out into a window of its own at `place`. A panel already
    /// alone in a window stays there.
    pub fn detach(
        &mut self,
        panel: &str,
        place: WindowPlace,
        info: &PanelInfo,
    ) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        if let Container::Window(w) = &loc.container
            && self.window(w).is_some_and(|w| w.root.panels().len() == 1)
        {
            return Ok(Vec::new());
        }
        self.move_to(panel, &Target::NewWindow(place), info)
    }

    /// Pop the whole stack holding `panel` out into a window of its own. The
    /// center stack stays where it is and only its panels move.
    pub fn detach_group(
        &mut self,
        panel: &str,
        place: WindowPlace,
    ) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        let Container::Dock(dock) = loc.container.clone() else {
            return Ok(Vec::new());
        };
        let stack = self.stack(&loc.stack).expect("located").clone();
        for (i, p) in stack.panels.iter().enumerate() {
            let l = Location {
                container: loc.container.clone(),
                stack: stack.id.clone(),
                index: i,
            };
            self.remember_spot(p, &l);
        }
        let moved = {
            let s = self.stack_mut(&stack.id).expect("located");
            let panels = std::mem::take(&mut s.panels);
            s.active = None;
            panels
        };
        let id = self.fresh("w");
        let stack_id = self.fresh("s");
        let mut new_stack = Stack::with(stack_id, moved.iter().cloned());
        new_stack.active = stack.active.clone();
        new_stack.tabs = stack.tabs;
        self.windows.push(WindowLayout {
            id: id.clone(),
            home: Some(dock.clone()),
            place,
            root: Node::Stack(new_stack),
        });
        let mut effects = vec![Effect::WindowOpened(id.clone())];
        effects.extend(self.prune());
        for p in moved {
            effects.push(Effect::Moved {
                panel: p,
                from: Where::Dock(dock.clone()),
                to: Where::Window(id.clone()),
            });
        }
        Ok(effects)
    }

    /// Dock `panel` back from its window: to the spot it came from, else
    /// where `info` places it.
    pub fn attach(&mut self, panel: &str, info: &PanelInfo) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        let Container::Window(window) = loc.container.clone() else {
            return Err(OpError::NotInWindow(panel.into()));
        };
        if !info.dockable
            || (info.dock.is_none()
                && self
                    .homes
                    .get(panel)
                    .and_then(|h| h.dock.as_ref())
                    .is_none())
        {
            return Err(OpError::NotDockable(panel.into()));
        }
        self.take_out(panel, &loc);
        let home = self.homes.remove(panel).unwrap_or_default();
        let dock = match home
            .dock
            .as_ref()
            .and_then(|spot| self.put_at_spot(panel, spot))
        {
            Some(d) => Where::Dock(d),
            None => {
                let mut info = info.clone();
                if info.dock.is_none() {
                    info.dock = home.dock.as_ref().map(|s| s.dock.clone());
                }
                if info.place == Place::Window {
                    info.place = Place::Center;
                }
                self.put_by_place(panel, &info)?
            }
        };
        let mut effects = self.prune();
        effects.push(Effect::Moved {
            panel: panel.to_string(),
            from: Where::Window(window),
            to: dock,
        });
        Ok(effects)
    }

    /// Empty the window `id`, as closing it does: each panel docks back when
    /// `dock_back` says so and it docks, and hides otherwise.
    pub fn close_window(
        &mut self,
        id: &str,
        dock_back: bool,
        info: &dyn Fn(&str) -> PanelInfo,
    ) -> Vec<Effect> {
        let Some(w) = self.window(id) else {
            return Vec::new();
        };
        let panels: Vec<PanelId> = w.root.panels().into_iter().cloned().collect();
        let mut effects = Vec::new();
        for p in panels {
            let i = info(&p);
            let done = if dock_back && i.dockable {
                self.attach(&p, &i).ok()
            } else {
                None
            };
            match done {
                Some(e) => effects.extend(e),
                None => {
                    if let Ok(e) = self.hide(&p) {
                        effects.extend(e);
                    }
                }
            }
        }
        effects
    }

    /// Collapse or expand the stack called `target`, or the stack holding
    /// the panel called `target`.
    pub fn collapse(&mut self, target: &str, collapsed: bool) -> Result<Vec<Effect>, OpError> {
        let stack_id = match self.find(target) {
            Some(loc) => loc.stack,
            None if self.stack(target).is_some() => target.to_string(),
            None => return Err(OpError::UnknownTarget(target.into())),
        };
        let s = self.stack_mut(&stack_id).expect("found");
        s.collapsed = collapsed;
        Ok(Vec::new())
    }

    /// Fill `panel`'s dock with its stack, showing it.
    pub fn maximize(&mut self, panel: &str) -> Result<Vec<Effect>, OpError> {
        let loc = self
            .find(panel)
            .ok_or_else(|| OpError::UnknownPanel(panel.into()))?;
        let Container::Dock(dock) = loc.container else {
            return Err(OpError::NotInDock(panel.into()));
        };
        let effects = self.activate(panel)?;
        let area = self.docks.get_mut(&dock).expect("located");
        area.maximized = Some(panel.to_string());
        if let Some(s) = self.stack_mut(&loc.stack) {
            s.collapsed = false;
        }
        Ok(effects)
    }

    /// Give the dock `dock` its whole tree back. Returns whether it had a
    /// maximized panel.
    pub fn restore_maximized(&mut self, dock: &str) -> bool {
        self.docks
            .get_mut(dock)
            .is_some_and(|d| d.maximized.take().is_some())
    }

    /// Hold the children of the split `id` at `sizes`, in logical pixels.
    pub fn resize(&mut self, id: &str, sizes: &[f32]) -> bool {
        for (_, root) in self.roots_mut() {
            if let Some(Node::Split(split)) = root.find_mut(id) {
                for (c, s) in split.children.iter_mut().zip(sizes) {
                    c.basis = Some(s.max(0.0));
                }
                return true;
            }
        }
        false
    }

    /// Forget `panel` entirely: it leaves the tree, its home and its meta.
    pub fn forget(&mut self, panel: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(loc) = self.find(panel) {
            self.take_out(panel, &loc);
            effects.push(Effect::Hidden(panel.to_string()));
            effects.extend(self.prune());
        }
        self.homes.remove(panel);
        self.panels.remove(panel);
        effects
    }

    /// Make this workspace the named layout `named`, which places some
    /// panels: they go where it says, a dock it does not name keeps its own
    /// tree, and every panel `default` shows that `named` does not name goes
    /// to its spot in `default` (or the center stack), so a panel added after
    /// a layout was written joins it.
    pub fn apply_named(&mut self, named: &Workspace, default: &Workspace) -> Vec<Effect> {
        let before: BTreeMap<PanelId, Where> = self
            .shown_panels()
            .into_iter()
            .map(|p| {
                let w = self.where_is(&p);
                (p, w)
            })
            .collect();
        let mut next = named.clone();
        next.homes = BTreeMap::new();
        next.panels = self.panels.clone();
        next.layout_version = self.layout_version;
        // Docks the layout does not name keep the tree they have.
        for (id, dock) in &self.docks {
            next.docks.entry(id.clone()).or_insert_with(|| dock.clone());
        }
        let named_panels: BTreeSet<PanelId> = named.shown_panels().into_iter().collect();
        for p in default.shown_panels() {
            if named_panels.contains(&p) || next.is_shown(&p) {
                continue;
            }
            let Some(loc) = default.find(&p) else {
                continue;
            };
            let Container::Dock(dock) = &loc.container else {
                continue;
            };
            if next.stack(&loc.stack).is_some() {
                let s = next.stack_mut(&loc.stack).expect("found");
                let at = loc.index.min(s.panels.len());
                s.panels.insert(at, p.clone());
                s.fix_active();
            } else if let Some(center) = next.center_of(dock) {
                let s = next.stack_mut(&center).expect("found");
                s.panels.push(p.clone());
                s.fix_active();
            }
        }
        // Panels shown now that the result does not show are hidden, and
        // remember where they were.
        for p in before.keys() {
            if !next.is_shown(p)
                && let Some(loc) = self.find(p)
            {
                let mut home = self.homes.get(p).cloned().unwrap_or_default();
                if let Container::Dock(d) = &loc.container {
                    home.dock = Some(self.spot_of(d, &loc));
                }
                next.homes.insert(p.clone(), home);
            }
        }
        for (p, h) in &self.homes {
            if !next.is_shown(p) {
                next.homes.entry(p.clone()).or_insert_with(|| h.clone());
            }
        }
        next.prune();
        self.replace_with(next)
    }

    /// Make this workspace `next`, reporting each panel that entered or left
    /// view or moved between docks and windows.
    pub fn replace_with(&mut self, next: Workspace) -> Vec<Effect> {
        let before: BTreeMap<PanelId, Where> = self
            .shown_panels()
            .into_iter()
            .map(|p| {
                let w = self.where_is(&p);
                (p, w)
            })
            .collect();
        *self = next;
        let mut effects = Vec::new();
        for (p, was) in &before {
            let now = self.where_is(p);
            if now == Where::Hidden {
                effects.push(Effect::Hidden(p.clone()));
            } else if now != *was {
                effects.push(Effect::Moved {
                    panel: p.clone(),
                    from: was.clone(),
                    to: now,
                });
            }
        }
        for p in self.shown_panels() {
            if !before.contains_key(&p) {
                effects.push(Effect::Shown(p.clone()));
                let to = self.where_is(&p);
                effects.push(Effect::Moved {
                    panel: p,
                    from: Where::Hidden,
                    to,
                });
            }
        }
        effects
    }

    /// Fit a restored workspace to the panels the app has now: a panel it
    /// does not name takes its place from `default`, a dock it does not name
    /// takes `default`'s tree, and a panel it names that the app no longer
    /// has stays as a placeholder (returned) until the app adds it again or
    /// forgets it.
    pub fn reconcile(&mut self, default: &Workspace, known: &BTreeSet<PanelId>) -> Vec<PanelId> {
        for (id, dock) in &default.docks {
            self.docks.entry(id.clone()).or_insert_with(|| dock.clone());
        }
        self.docks.retain(|id, _| default.docks.contains_key(id));
        let mut mentioned: BTreeSet<PanelId> = self.shown_panels().into_iter().collect();
        mentioned.extend(self.homes.keys().cloned());
        for p in default.shown_panels() {
            if mentioned.contains(&p) {
                continue;
            }
            let Some(loc) = default.find(&p) else {
                continue;
            };
            let Container::Dock(dock) = &loc.container else {
                continue;
            };
            let target = if self.stack(&loc.stack).is_some() {
                Some(loc.stack.clone())
            } else {
                self.center_of(dock)
            };
            if let Some(target) = target {
                let s = self.stack_mut(&target).expect("found");
                let at = loc.index.min(s.panels.len());
                s.panels.insert(at, p.clone());
                s.fix_active();
            }
        }
        for (p, h) in &default.homes {
            if !mentioned.contains(p) && !self.is_shown(p) {
                self.homes.insert(p.clone(), h.clone());
            }
        }
        self.prune();
        self.shown_panels()
            .into_iter()
            .filter(|p| !known.contains(p))
            .collect()
    }

    // ---- internals --------------------------------------------------------

    fn center_of(&self, dock: &str) -> Option<String> {
        let area = self.docks.get(dock)?;
        let stacks = area.root.stacks();
        stacks
            .iter()
            .find(|s| s.center)
            .or_else(|| stacks.first())
            .map(|s| s.id.clone())
    }

    fn container_of_node(&self, id: &str) -> Option<Container> {
        self.roots()
            .find(|(_, r)| r.find(id).is_some())
            .map(|(c, _)| c)
    }

    fn root_mut(&mut self, container: &Container) -> Option<&mut Node> {
        match container {
            Container::Dock(d) => self.docks.get_mut(d).map(|d| &mut d.root),
            Container::Window(w) => self
                .windows
                .iter_mut()
                .find(|x| x.id == *w)
                .map(|w| &mut w.root),
        }
    }

    fn spot_of(&self, dock: &str, loc: &Location) -> Spot {
        let (split, slot) = self
            .docks
            .get(dock)
            .and_then(|d| d.root.parent_of(&loc.stack))
            .map(|(s, i)| (Some(s.id.clone()), i))
            .unwrap_or((None, 0));
        Spot {
            dock: dock.to_string(),
            stack: loc.stack.clone(),
            index: loc.index,
            split,
            slot,
        }
    }

    /// Remember where a panel leaving view was.
    fn remember(&mut self, panel: &str, loc: &Location) {
        let mut home = self.homes.get(panel).cloned().unwrap_or_default();
        match &loc.container {
            Container::Dock(d) => {
                home.dock = Some(self.spot_of(d, loc));
                home.window = None;
            }
            Container::Window(w) => {
                home.window = Some(self.window(w).map(|w| w.place.clone()).unwrap_or_default());
            }
        }
        self.homes.insert(panel.to_string(), home);
    }

    /// Remember the dock spot of a panel leaving a dock for a window.
    fn remember_spot(&mut self, panel: &str, loc: &Location) {
        let Container::Dock(d) = &loc.container else {
            return;
        };
        let spot = self.spot_of(d, loc);
        let home = self.homes.entry(panel.to_string()).or_default();
        home.dock = Some(spot);
        home.window = None;
    }

    /// Take the panel out of its stack, leaving the tree unpruned.
    fn take_out(&mut self, panel: &str, loc: &Location) {
        if let Some(s) = self.stack_mut(&loc.stack) {
            s.panels.retain(|p| p != panel);
            s.fix_active();
        }
        for dock in self.docks.values_mut() {
            if dock.maximized.as_deref() == Some(panel) {
                dock.maximized = None;
            }
        }
    }

    /// Restore the invariants after a removal: drop empty stacks and
    /// one-child splits, empty windows, and a maximize whose panel left.
    fn prune(&mut self) -> Vec<Effect> {
        for dock in self.docks.values_mut() {
            if !dock.root.prune() {
                // A dock always has a root: keep the empty stack.
                if let Node::Stack(s) = &mut dock.root {
                    s.active = None;
                }
            }
            if let Some(m) = dock.maximized.clone()
                && locate(&dock.root, &m).is_none()
            {
                dock.maximized = None;
            }
        }
        let mut effects = Vec::new();
        self.windows.retain_mut(|w| {
            let keep = w.root.prune() && !w.root.panels().is_empty();
            if !keep {
                effects.push(Effect::WindowClosed(w.id.clone()));
            }
            keep
        });
        effects
    }

    /// A fresh id with `prefix` that no node or window has.
    fn fresh(&self, prefix: &str) -> String {
        let mut used = BTreeSet::new();
        for (_, r) in self.roots() {
            r.walk(&mut |n| {
                used.insert(n.id().to_string());
            });
        }
        for w in &self.windows {
            used.insert(w.id.clone());
        }
        for h in self.homes.values() {
            if let Some(s) = &h.dock {
                used.insert(s.stack.clone());
                if let Some(x) = &s.split {
                    used.insert(x.clone());
                }
            }
        }
        (1..)
            .map(|n| format!("{prefix}{n}"))
            .find(|id| !used.contains(id))
            .expect("an unused id")
    }

    fn open_window(&mut self, panel: &str, info: &PanelInfo, place: WindowPlace) -> String {
        let id = self.fresh("w");
        let stack_id = self.fresh("s");
        self.windows.push(WindowLayout {
            id: id.clone(),
            home: info.dock.clone(),
            place,
            root: Node::Stack(Stack::with(stack_id, [panel])),
        });
        id
    }

    /// Put the panel back at `spot` if the dock still has its stack or its
    /// split. Returns the dock.
    fn put_at_spot(&mut self, panel: &str, spot: &Spot) -> Option<String> {
        let dock = self.docks.get_mut(&spot.dock)?;
        if let Some(Node::Stack(s)) = dock.root.find_mut(&spot.stack) {
            let at = spot.index.min(s.panels.len());
            s.panels.insert(at, panel.to_string());
            s.active = Some(panel.to_string());
            return Some(spot.dock.clone());
        }
        let split_id = spot.split.clone()?;
        let taken = self.roots().any(|(_, r)| r.find(&spot.stack).is_some());
        let stack_id = if taken {
            self.fresh("s")
        } else {
            spot.stack.clone()
        };
        let dock = self.docks.get_mut(&spot.dock)?;
        if let Some(Node::Split(split)) = dock.root.find_mut(&split_id) {
            let at = spot.slot.min(split.children.len());
            split.children.insert(
                at,
                Child::grow(Node::Stack(Stack::with(stack_id, [panel])), 1.0),
            );
            return Some(spot.dock.clone());
        }
        None
    }

    /// Put the panel where `info` places it in a dock.
    fn put_by_place(&mut self, panel: &str, info: &PanelInfo) -> Result<Where, OpError> {
        let dock = info
            .dock
            .clone()
            .filter(|d| self.docks.contains_key(d))
            .or_else(|| self.docks.keys().next().cloned())
            .ok_or_else(|| OpError::NotDockable(panel.into()))?;
        match &info.place {
            Place::Edge(side) => {
                let id = self.fresh("s");
                self.insert_at_root_edge(&dock, *side, Node::Stack(Stack::with(id, [panel])));
            }
            Place::Beside(other) if self.is_shown(other) => {
                let loc = self.find(other).expect("shown");
                let s = self.stack_mut(&loc.stack).expect("located");
                s.panels.insert(loc.index + 1, panel.to_string());
                s.active = Some(panel.to_string());
                return Ok(loc.container.to_where());
            }
            Place::Window => {
                let id = self.open_window(panel, info, WindowPlace::default());
                return Ok(Where::Window(id));
            }
            _ => {
                let center = self.center_of(&dock).expect("a dock has a stack");
                let s = self.stack_mut(&center).expect("found");
                s.panels.push(panel.to_string());
                s.active = Some(panel.to_string());
            }
        }
        Ok(Where::Dock(dock))
    }

    fn insert_at_root_edge(&mut self, dock: &str, side: Side, node: Node) {
        let id = self.fresh("x");
        let Some(area) = self.docks.get_mut(dock) else {
            return;
        };
        if let Node::Stack(s) = &area.root
            && s.panels.is_empty()
            && !s.center
        {
            area.root = node;
            return;
        }
        let new = Child::grow(node, 0.0);
        match &mut area.root {
            Node::Split(split) if split.axis == side.axis() => {
                if side.before() {
                    split.children.insert(0, new);
                } else {
                    split.children.push(new);
                }
            }
            root => {
                let old = std::mem::replace(root, Node::Stack(Stack::new("")));
                let old = Child::grow(old, 1.0);
                let children = if side.before() {
                    vec![new, old]
                } else {
                    vec![old, new]
                };
                *root = Node::Split(Split {
                    axis: side.axis(),
                    id,
                    children,
                });
            }
        }
    }

    fn insert_beside(&mut self, container: &Container, node: &str, side: Side, new: Node) {
        let split_id = self.fresh("x");
        let Some(root) = self.root_mut(container) else {
            return;
        };
        if root.id() == node {
            let old = std::mem::replace(root, Node::Stack(Stack::new("")));
            let (a, b) = (Child::grow(old, 1.0), Child::grow(new, 1.0));
            *root = Node::Split(Split {
                axis: side.axis(),
                id: split_id,
                children: if side.before() {
                    vec![b, a]
                } else {
                    vec![a, b]
                },
            });
            return;
        }
        let parent_id = root.parent_of(node).map(|(s, _)| s.id.clone());
        let Some(parent_id) = parent_id else {
            return;
        };
        let Some(Node::Split(parent)) = root.find_mut(&parent_id) else {
            return;
        };
        let slot = parent
            .children
            .iter()
            .position(|c| c.node.id() == node)
            .expect("parent holds node");
        if parent.axis == side.axis() {
            let at = if side.before() { slot } else { slot + 1 };
            parent.children.insert(at, Child::grow(new, 1.0));
        } else {
            let child = &mut parent.children[slot];
            let old = std::mem::replace(&mut child.node, Node::Stack(Stack::new("")));
            let (a, b) = (Child::grow(old, 1.0), Child::grow(new, 1.0));
            child.node = Node::Split(Split {
                axis: side.axis(),
                id: split_id,
                children: if side.before() {
                    vec![b, a]
                } else {
                    vec![a, b]
                },
            });
        }
    }
}

/// The stack and tab index of `panel` under `node`.
fn locate(node: &Node, panel: &str) -> Option<(String, usize)> {
    node.stacks().into_iter().find_map(|s| {
        s.panels
            .iter()
            .position(|p| p == panel)
            .map(|i| (s.id.clone(), i))
    })
}

#[cfg(test)]
mod tests;
