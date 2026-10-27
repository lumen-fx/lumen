//! Rearranging by pointer: dragging a tab (or a stack's strip) to another
//! place in a dock, tearing it off into a window of its own, and dragging a
//! window's group back into a dock.
//!
//! A press on a tab that moves past the core drag threshold starts a drag.
//! While it runs nothing in the layout moves: a ghost of the tab follows the
//! pointer and a preview shows the region the drop would fill. The release
//! applies one model operation; Escape, or a release where nothing would
//! land, puts everything back as it was.
//!
//! Across windows the drag goes through the core's surface-drag seam: the
//! module begins a drag in the pressed window and the window backend says
//! which of the app's windows the pointer is over. Leaving the window by the
//! tear-off distance pops the tab out into a window that follows the
//! pointer; over another window that window shows its preview and the one
//! being dragged hides. A backend that cannot move a window with the pointer
//! (a Wayland compositor without the toplevel-drag protocol) gets no
//! following window: a release over no window opens the pop-out there. An
//! app whose backend carries no seam drags within the window only.

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_dock_model::{Container, PanelId, Rect, Side, Target, WindowPlace, Workspace};
use lumen_module::lumen_core::components::{
    A11yAnnouncementQueue, A11yLive, Length, Style, Transform,
};
use lumen_module::lumen_core::input::PointerEvents;
use lumen_module::lumen_core::render_world::OverlayLayer;
use lumen_module::lumen_core::surface_drag::{
    SurfaceDragEvent, SurfaceDragRequest, SurfaceDragSupport,
};
use lumen_module::lumen_core::window::{Window, WindowPartitions, WindowViewport};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element};
use lumen_module::lumen_primitives::{TabStrip, TabStripButton};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::adopt::{DockChrome, hide, spawn_chrome, unhide};
use crate::frame::{DockFloat, DockFloatHandle, Edges, FLOAT_MIN, clamp_rect, place_float};
use crate::plugin::{DockCloseButton, text};
use crate::realize::{
    Built, DockStack, DockTab, DockWindow, alive, children_of, primary, realize, set_classes,
    set_inline,
};
use crate::state::{Core, Reason, Shared};

/// How far past the window's edge the pointer goes before a tab tears off,
/// in logical pixels, when the stylesheet does not say
/// (`--lumen-dock-tearoff-distance`).
pub const TEAROFF_DISTANCE: f32 = 24.0;

/// The band along a dock's own edges that docks at the root, in logical
/// pixels.
const ROOT_BAND: f32 = 24.0;

/// The part of a stack along each edge that splits it, as a fraction of the
/// stack's size, and the most it takes in logical pixels.
const EDGE_FRACTION: f32 = 0.25;
const EDGE_CAP: f32 = 120.0;

/// The width of the marker a drop between two tabs shows.
const TAB_MARKER: f32 = 3.0;

/// Where a drop lands.
#[derive(Clone, Debug, PartialEq)]
pub enum Zone {
    /// Between two tabs of a stack's strip, at a tab index.
    Tab {
        /// The stack.
        stack: String,
        /// The index the tab takes.
        index: usize,
    },
    /// The body of a stack: a tab at its end.
    Center {
        /// The stack.
        stack: String,
    },
    /// An edge of a stack: a new stack beside it.
    Edge {
        /// The stack.
        stack: String,
        /// The side.
        side: Side,
    },
    /// An edge of a whole dock: a new stack along it.
    RootEdge {
        /// The dock.
        dock: String,
        /// The side.
        side: Side,
    },
    /// Over no stack of a dock's window: a new float over the dock.
    Float {
        /// The dock.
        dock: String,
        /// Where, from the dock's top-left corner.
        rect: Rect,
    },
}

/// What lies under the pointer for a drop.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    /// A zone, and the region its preview covers in window coordinates.
    Zone(Zone, (Vec2, Vec2)),
    /// Where the payload already is: a drop changes nothing.
    Same,
    /// No stack and no dock edge.
    Nowhere,
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

impl Zone {
    /// The class the preview takes for this zone: `dock-drop-tab`,
    /// `dock-drop-center`, `dock-drop-<side>`, `dock-drop-root-<side>`.
    pub fn class(&self) -> String {
        match self {
            Zone::Tab { .. } => "dock-drop-tab".into(),
            Zone::Center { .. } => "dock-drop-center".into(),
            Zone::Edge { side, .. } => format!("dock-drop-{}", side_name(*side)),
            Zone::RootEdge { side, .. } => format!("dock-drop-root-{}", side_name(*side)),
            Zone::Float { .. } => "dock-drop-float".into(),
        }
    }

    /// The model target a drop here moves to.
    pub fn target(&self, ws: &Workspace) -> Target {
        match self {
            Zone::Tab { stack, index } => Target::Tab {
                stack: stack.clone(),
                index: *index,
            },
            Zone::Center { stack } => Target::Tab {
                stack: stack.clone(),
                index: ws.stack(stack).map_or(0, |s| s.panels.len()),
            },
            Zone::Edge { stack, side } => Target::Edge {
                node: stack.clone(),
                side: *side,
            },
            Zone::RootEdge { dock, side } => Target::RootEdge {
                dock: dock.clone(),
                side: *side,
            },
            Zone::Float { dock, rect } => Target::NewFloat {
                dock: dock.clone(),
                rect: *rect,
            },
        }
    }
}

const ZONE_CLASSES: [&str; 11] = [
    "dock-drop-float",
    "dock-drop-tab",
    "dock-drop-center",
    "dock-drop-left",
    "dock-drop-right",
    "dock-drop-top",
    "dock-drop-bottom",
    "dock-drop-root-left",
    "dock-drop-root-right",
    "dock-drop-root-top",
    "dock-drop-root-bottom",
];

/// What a drag carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    /// One panel, by its tab.
    Panel(PanelId),
    /// A whole stack, by its strip, or a window's only tab.
    Group(String),
}

/// A float a drag moves with the pointer: its whole group dragged by its
/// strip.
#[derive(Debug, Clone)]
struct FloatMove {
    /// The float's id.
    id: String,
    /// Its element.
    element: Entity,
    /// Its dock's element.
    dock: Entity,
    /// Where the pointer holds it, from its corner.
    grab: Vec2,
    /// Where it is now, from the dock's corner.
    rect: Rect,
}

/// A float being resized by one of its handles.
#[derive(Debug, Clone)]
struct Resize {
    float: String,
    element: Entity,
    dock: Entity,
    edges: Edges,
    /// Where the press was, in window coordinates.
    start: Vec2,
    /// The float's rectangle when the press began.
    from: Rect,
    /// Its rectangle now.
    rect: Rect,
}

impl Resize {
    /// The float's rectangle with the pointer at `p`: the handle's edges
    /// follow the pointer, never past the smallest a float gets.
    fn at(&self, p: Vec2) -> Rect {
        let d = p - self.start;
        let mut r = self.from;
        if self.edges.left {
            let x = (r.x + d.x).min(r.x + r.w - FLOAT_MIN.x);
            r.w += r.x - x;
            r.x = x;
        }
        if self.edges.right {
            r.w = (r.w + d.x).max(FLOAT_MIN.x);
        }
        if self.edges.top {
            let y = (r.y + d.y).min(r.y + r.h - FLOAT_MIN.y);
            r.h += r.y - y;
            r.y = y;
        }
        if self.edges.bottom {
            r.h = (r.h + d.y).max(FLOAT_MIN.y);
        }
        r
    }
}

/// A drag in progress.
#[derive(Debug)]
struct Session {
    payload: Payload,
    /// The window the press happened in.
    source: Entity,
    /// Where the pointer holds the dragged tab, from the tab's corner, or
    /// the dragged window, from the window's corner.
    grab: Vec2,
    /// The arrangement before the drag, which a cancel puts back.
    before: Workspace,
    /// The drag runs through the window backend's seam.
    seam: bool,
    /// The window and point the pointer was last over.
    over: Option<(Entity, Vec2)>,
    /// Where a drop would land, and the window it is in.
    zone: Option<(Entity, Zone)>,
    /// The model id of a window the drag moves with the pointer: one torn
    /// off during this drag, or the pop-out whose group is dragged.
    moving: Option<String>,
    /// The moving window was torn off during this drag.
    torn: bool,
    /// The moving window is hidden while another window shows a preview.
    moving_hidden: bool,
    /// A release over no window opens the pop-out there (no window can
    /// follow the pointer).
    open_on_release: bool,
    /// The float the drag moves, when its whole group is dragged.
    float: Option<FloatMove>,
    /// The size a new float made by this drag takes.
    float_size: Vec2,
    /// Whether what the drag carries may float.
    floatable: bool,
    ghost: Option<Entity>,
    preview: Option<Entity>,
}

/// The drag the dock is running, if any.
#[derive(Resource, Default, Debug)]
pub struct DockDrag {
    session: Option<Session>,
    resize: Option<Resize>,
}

impl DockDrag {
    /// Whether a drag is in progress: a tab, a strip, or a float's resize
    /// handle.
    pub fn active(&self) -> bool {
        self.session.is_some() || self.resize.is_some()
    }

    /// Whether a float is being resized.
    pub fn resizing(&self) -> bool {
        self.resize.is_some()
    }

    /// What the drag carries.
    pub fn payload(&self) -> Option<&Payload> {
        self.session.as_ref().map(|s| &s.payload)
    }

    /// Where a drop would land now.
    pub fn zone(&self) -> Option<&Zone> {
        self.session
            .as_ref()
            .and_then(|s| s.zone.as_ref())
            .map(|(_, z)| z)
    }
}

/// What the dock heard about a drag this tick, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum DragInput {
    /// A press moved past the threshold on `entity`, pressed at `start`.
    Start {
        /// The element pressed.
        entity: Entity,
        /// Where, in its window's logical pixels.
        start: Vec2,
    },
    /// The pointer moved within the pressed window (no seam).
    Moved(Vec2),
    /// The press ended within the pressed window (no seam).
    Ended(Vec2),
    /// The seam's report.
    Seam(SurfaceDragEvent),
    /// Escape.
    Escape,
}

fn parent(world: &World, e: Entity) -> Option<Entity> {
    world.get::<ChildOf>(e).map(ChildOf::parent)
}

fn rect(world: &World, e: Entity) -> Option<(Vec2, Vec2)> {
    let t = world.get::<Transform>(e)?;
    (t.size.x > 0.0 && t.size.y > 0.0).then_some((t.absolute, t.size))
}

fn inside(p: Vec2, (o, s): (Vec2, Vec2)) -> bool {
    p.x >= o.x && p.y >= o.y && p.x < o.x + s.x && p.y < o.y + s.y
}

fn window_of(world: &mut World, e: Entity) -> Option<Entity> {
    world
        .get_resource::<WindowPartitions>()
        .and_then(|p| p.window_of(e))
        .or_else(|| primary(world))
}

/// Whether `e` shows: it and no ancestor is hidden by the module.
fn shown(world: &World, e: Entity) -> bool {
    let mut cur = Some(e);
    while let Some(x) = cur {
        if world.get::<crate::adopt::DockHides>(x).is_some() {
            return false;
        }
        cur = parent(world, x);
    }
    true
}

/// The model id of the window `window` is, when the dock opened it.
fn model_window(world: &World, window: Entity) -> Option<String> {
    world.get::<DockWindow>(window).map(|w| w.id.clone())
}

/// What a press on `entity` would drag: a tab's panel, a strip's stack, or
/// the group of a window whose only tab it is; `None` for a close button or
/// anything that is not the dock's. A float's strip is its title bar: the
/// drag moves the float.
fn payload_at(world: &World, core: &Core, entity: Entity) -> Option<(Payload, Entity)> {
    let mut cur = Some(entity);
    while let Some(e) = cur {
        if world.get::<DockCloseButton>(e).is_some() {
            return None;
        }
        if let Some(b) = world.get::<TabStripButton>(e) {
            let panel = world.get::<DockTab>(b.tab)?.panel.clone();
            let rec = core.panels.get(&panel)?;
            if !rec.attrs.movable {
                return None;
            }
            // The only tab of a window is the window's handle.
            if let Some(loc) = core.ws.find(&panel)
                && let Container::Window(w) = &loc.container
                && core
                    .ws
                    .window(w)
                    .is_some_and(|w| w.root.panels().len() == 1)
            {
                return Some((Payload::Group(loc.stack), e));
            }
            return Some((Payload::Panel(panel), e));
        }
        if world.get::<TabStrip>(e).is_some() {
            let stack = parent(world, e).and_then(|s| world.get::<DockStack>(s))?;
            let group = core.ws.stack(&stack.id)?;
            let movable = group
                .panels
                .iter()
                .all(|p| core.panels.get(p).is_none_or(|r| r.attrs.movable));
            if !movable || group.panels.is_empty() {
                return None;
            }
            return Some((Payload::Group(stack.id.clone()), e));
        }
        cur = parent(world, e);
    }
    None
}

/// The stack and the panels a payload takes along.
fn payload_stack(ws: &Workspace, payload: &Payload) -> Option<(String, usize)> {
    match payload {
        Payload::Panel(p) => {
            let loc = ws.find(p)?;
            let n = ws.stack(&loc.stack)?.panels.len();
            Some((loc.stack, n))
        }
        Payload::Group(s) => Some((s.clone(), ws.stack(s)?.panels.len())),
    }
}

/// What a drop at `p` in `window` would do with `payload`.
fn zone_at(
    world: &mut World,
    core: &Core,
    built: &Built,
    (window, p): (Entity, Vec2),
    payload: &Payload,
) -> Hit {
    let in_window = |world: &mut World, e: Entity| window_of(world, e) == Some(window);
    // The innermost shown stack under the pointer; a float's over the
    // dock's tree, the top-most float's over the rest.
    let mut best: Option<(String, Entity, (Vec2, Vec2), u32)> = None;
    for (id, &e) in &built.nodes {
        if world.get::<DockStack>(e).is_none() || !alive(world, e) || !in_window(world, e) {
            continue;
        }
        if !shown(world, e) {
            continue;
        }
        let Some(r) = rect(world, e) else { continue };
        let layer = float_layer(world, core, e);
        if inside(p, r)
            && best
                .as_ref()
                .is_none_or(|b| layer > b.3 || (layer == b.3 && r.1.x * r.1.y < b.2.1.x * b.2.1.y))
        {
            best = Some((id.clone(), e, r, layer));
        }
    }
    let (own, own_len) = payload_stack(&core.ws, payload).unwrap_or_default();
    let Some((stack, e, (o, s), _)) = best else {
        return root_edge(world, core, window, p, payload)
            .map_or(Hit::Nowhere, |(z, r)| Hit::Zone(z, r));
    };
    let is_group = matches!(payload, Payload::Group(_));
    if own == stack && (is_group || own_len <= 1) {
        // Onto itself: nothing to do.
        return Hit::Same;
    }
    // The strip: between two tabs.
    let strip = children_of(world, e)
        .into_iter()
        .find(|&k| world.get::<TabStrip>(k).is_some());
    if let Some(strip) = strip
        && let Some(sr) = rect(world, strip)
        && inside(p, sr)
    {
        let buttons: Vec<(Entity, (Vec2, Vec2))> = children_of(world, strip)
            .into_iter()
            .filter(|&b| world.get::<TabStripButton>(b).is_some())
            .filter_map(|b| rect(world, b).map(|r| (b, r)))
            .collect();
        let mut index = buttons
            .iter()
            .filter(|(_, (bo, bs))| bo.x + bs.x / 2.0 < p.x)
            .count();
        let marker_x = match buttons.get(index) {
            Some((_, (bo, _))) => bo.x,
            None => buttons.last().map_or(sr.0.x, |(_, (bo, bs))| bo.x + bs.x),
        };
        if own == stack
            && let Payload::Panel(panel) = payload
            && let Some(at) = core.ws.find(panel).map(|l| l.index)
        {
            // Its own tab leaves its place first.
            if at < index {
                index -= 1;
            }
            if at == index {
                return Hit::Same;
            }
        }
        let region = (
            Vec2::new(marker_x - TAB_MARKER / 2.0, sr.0.y),
            Vec2::new(TAB_MARKER, sr.1.y),
        );
        return Hit::Zone(Zone::Tab { stack, index }, region);
    }
    // A whole dock's edges, outside its strips and its floats.
    if float_layer(world, core, e) == 0
        && let Some((z, r)) = root_edge(world, core, window, p, payload)
    {
        return Hit::Zone(z, r);
    }
    // The body: an edge band, else the middle.
    let band = Vec2::new(
        (s.x * EDGE_FRACTION).min(EDGE_CAP),
        (s.y * EDGE_FRACTION).min(EDGE_CAP),
    );
    let near = [
        (p.x - o.x, band.x, Side::Left),
        (o.x + s.x - p.x, band.x, Side::Right),
        (p.y - o.y, band.y, Side::Top),
        (o.y + s.y - p.y, band.y, Side::Bottom),
    ]
    .into_iter()
    .filter(|(d, b, _)| d < b)
    .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, _, side)) = near {
        let half = s / 2.0;
        let region = match side {
            Side::Left => (o, Vec2::new(half.x, s.y)),
            Side::Right => (Vec2::new(o.x + half.x, o.y), Vec2::new(half.x, s.y)),
            Side::Top => (o, Vec2::new(s.x, half.y)),
            Side::Bottom => (Vec2::new(o.x, o.y + half.y), Vec2::new(s.x, half.y)),
        };
        return Hit::Zone(Zone::Edge { stack, side }, region);
    }
    if own == stack {
        // Its own stack's middle is where it already is.
        return Hit::Same;
    }
    let below = strip
        .and_then(|k| rect(world, k))
        .map_or(0.0, |(_, ss)| ss.y);
    Hit::Zone(
        Zone::Center { stack },
        (
            Vec2::new(o.x, o.y + below),
            Vec2::new(s.x, (s.y - below).max(1.0)),
        ),
    )
}

/// How high the stack element `e` lies: 0 in a dock's tree, else one more
/// than the floats under its float.
fn float_layer(world: &World, core: &Core, e: Entity) -> u32 {
    let mut cur = Some(e);
    while let Some(x) = cur {
        if let Some(f) = world.get::<DockFloat>(x) {
            return core
                .ws
                .docks
                .values()
                .find_map(|d| d.floats.iter().position(|g| g.id == f.id))
                .map_or(1, |i| i as u32 + 1);
        }
        cur = parent(world, x);
    }
    0
}

/// The edge of a whole dock in `window` under `p`, when the pointer is in
/// the band along it.
fn root_edge(
    world: &mut World,
    core: &Core,
    window: Entity,
    p: Vec2,
    payload: &Payload,
) -> Option<(Zone, (Vec2, Vec2))> {
    let in_window = |world: &mut World, e: Entity| window_of(world, e) == Some(window);
    for (id, dock) in &core.docks {
        let e = dock.entity;
        if !alive(world, e) || !in_window(world, e) {
            continue;
        }
        let Some(r) = rect(world, e) else { continue };
        if !inside(p, r) {
            continue;
        }
        let (o, s) = r;
        let near = [
            (p.x - o.x, Side::Left),
            (o.x + s.x - p.x, Side::Right),
            (p.y - o.y, Side::Top),
            (o.y + s.y - p.y, Side::Bottom),
        ]
        .into_iter()
        .filter(|(d, _)| *d < ROOT_BAND)
        .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, side)) = near
            && !matches!(payload, Payload::Group(g) if core.ws.stack(g).is_some_and(|s| s.center))
        {
            let band = match side {
                Side::Left | Side::Right => (s.x / 3.0).min(300.0),
                Side::Top | Side::Bottom => (s.y / 3.0).min(300.0),
            };
            let region = match side {
                Side::Left => (o, Vec2::new(band, s.y)),
                Side::Right => (Vec2::new(o.x + s.x - band, o.y), Vec2::new(band, s.y)),
                Side::Top => (o, Vec2::new(s.x, band)),
                Side::Bottom => (Vec2::new(o.x, o.y + s.y - band), Vec2::new(s.x, band)),
            };
            return Some((
                Zone::RootEdge {
                    dock: id.clone(),
                    side,
                },
                region,
            ));
        }
    }
    None
}

/// The dock of `window` under `p`, else its first: the dock a drop over no
/// stack floats over. `None` for a window with no dock (a pop-out).
fn dock_at(world: &mut World, core: &Core, window: Entity, p: Vec2) -> Option<(String, Entity)> {
    let mut first = None;
    for (id, d) in &core.docks {
        let e = d.entity;
        if !alive(world, e) || window_of(world, e) != Some(window) || !shown(world, e) {
            continue;
        }
        if rect(world, e).is_some_and(|r| inside(p, r)) {
            return Some((id.clone(), e));
        }
        first.get_or_insert((id.clone(), e));
    }
    first
}

/// The zone a drop over no stack of `window` makes: a new float over the
/// window's dock with its corner where the grabbed tab's would be.
fn float_zone(
    world: &mut World,
    core: &Core,
    window: Entity,
    p: Vec2,
    grab: Vec2,
    size: Vec2,
) -> Option<(Zone, (Vec2, Vec2))> {
    // Only inside the window: off it a drag tears off, or lands nowhere.
    let view = world.get::<WindowViewport>(window).map(|v| v.size)?;
    if !inside(p, (Vec2::ZERO, view)) {
        return None;
    }
    let (dock, e) = dock_at(world, core, window, p)?;
    let (o, s) = rect(world, e)?;
    let at = p - grab - o;
    let r = clamp_rect(
        Rect {
            x: at.x,
            y: at.y,
            w: size.x,
            h: size.y,
        },
        s,
    );
    let region = (o + Vec2::new(r.x, r.y), Vec2::new(r.w, r.h));
    Some((Zone::Float { dock, rect: r }, region))
}

/// The element the overlays sit in for `window`: the dock under `p` in the
/// main window, else its first dock; a pop-out's root.
fn container(
    world: &mut World,
    core: &Core,
    built: &Built,
    window: Entity,
    p: Vec2,
) -> Option<Entity> {
    if let Some(id) = model_window(world, window) {
        return built
            .windows
            .get(&id)
            .map(|(_, root)| *root)
            .filter(|&r| alive(world, r));
    }
    let docks: Vec<Entity> = core
        .docks
        .values()
        .map(|d| d.entity)
        .filter(|&e| alive(world, e))
        .collect();
    let mut first = None;
    for e in docks {
        if window_of(world, e) != Some(window) {
            continue;
        }
        first.get_or_insert(e);
        if rect(world, e).is_some_and(|r| inside(p, r)) {
            return Some(e);
        }
    }
    first
}

/// Put `overlay` under `parent` covering `(origin, size)` in window
/// coordinates.
fn place_overlay(world: &mut World, overlay: Entity, parent: Entity, (o, s): (Vec2, Vec2)) {
    if world.get::<ChildOf>(overlay).map(ChildOf::parent) != Some(parent) {
        world.entity_mut(overlay).insert(ChildOf(parent));
    }
    let base = world
        .get::<Transform>(parent)
        .map_or(Vec2::ZERO, |t| t.absolute);
    let at = o - base;
    set_inline(
        world,
        overlay,
        "inset",
        Some(format!("{} auto auto {}", at.y.round(), at.x.round())),
    );
    set_inline(world, overlay, "width", Some(format!("{}px", s.x.round())));
    set_inline(world, overlay, "height", Some(format!("{}px", s.y.round())));
    unhide(world, overlay);
}

/// Spawn one of the drag's overlays: out of the document, above
/// everything, never under the pointer.
fn spawn_overlay(world: &mut World, class: &str, title: Option<String>) -> Entity {
    let e = spawn_chrome(world, "column", &[class], None);
    world
        .entity_mut(e)
        .insert((OverlayLayer, PointerEvents::None));
    set_inline(world, e, "position", Some("absolute".into()));
    if let Some(title) = title {
        let el = Element {
            tag: "label".into(),
            attrs: Box::new(Attributes {
                classes: vec!["dock-drag-ghost-label".into()],
                text: Some(title),
                ..Attributes::default()
            }),
            ..Element::default()
        };
        let label = spawn_subtree(world, &el, Some(e), Placeholders::Resolved);
        world.entity_mut(label).insert((
            DockChrome,
            lumen_module::lumen_core::components::DomHidden,
            PointerEvents::None,
        ));
    }
    hide(world, e);
    e
}

/// The tear-off distance the stylesheet sets
/// (`--lumen-dock-tearoff-distance`), read off a probe the module's
/// stylesheet sizes with it.
fn tearoff_distance(world: &mut World, core: &Core) -> f32 {
    let Some(dock) = core
        .docks
        .values()
        .map(|d| d.entity)
        .find(|&e| alive(world, e))
    else {
        return TEAROFF_DISTANCE;
    };
    let probe = children_of(world, dock)
        .into_iter()
        .find(|&k| world.get::<TearoffProbe>(k).is_some());
    let probe = probe.unwrap_or_else(|| {
        let e = spawn_chrome(world, "column", &["dock-tearoff-probe"], Some(dock));
        world.entity_mut(e).insert(TearoffProbe);
        hide(world, e);
        e
    });
    match world.get::<Style>(probe).map(|s| s.width) {
        Some(Length::Px(v)) if v >= 0.0 => v,
        _ => TEAROFF_DISTANCE,
    }
}

/// On the hidden element whose width the stylesheet sets to the tear-off
/// distance.
#[derive(Component, Clone, Copy, Debug)]
pub struct TearoffProbe;

/// How far `p` (the source window's logical pixels) lies outside a window of
/// `size`.
fn outside_by(p: Vec2, size: Vec2) -> f32 {
    let dx = (-p.x).max(p.x - size.x).max(0.0);
    let dy = (-p.y).max(p.y - size.y).max(0.0);
    dx.max(dy)
}

fn set_dragging(world: &mut World, core: &Core, on: bool) {
    let docks: Vec<Entity> = core.docks.values().map(|d| d.entity).collect();
    for d in docks {
        if alive(world, d) {
            set_classes(
                world,
                d,
                if on { &["dock-dragging"] } else { &[] },
                &["dock-dragging"],
            );
        }
    }
}

fn window_entity(world: &World, built: &Built, id: &str) -> Option<Entity> {
    built
        .windows
        .get(id)
        .map(|(w, _)| *w)
        .filter(|&w| alive(world, w))
}

fn set_window_visible(world: &mut World, built: &Built, id: &str, visible: bool) {
    if let Some(w) = window_entity(world, built, id)
        && let Some(mut window) = world.get_mut::<Window>(w)
        && window.visible != visible
    {
        window.visible = visible;
    }
}

/// Run the drag on what the dock heard this tick.
pub fn drag_step(world: &mut World, inputs: Vec<DragInput>) {
    let shared = world.resource::<Shared>().clone();
    for input in inputs {
        match input {
            DragInput::Start { entity, start } => start_drag(world, &shared, entity, start),
            DragInput::Moved(p) => {
                if world.resource::<DockDrag>().resize.is_some() {
                    resize_to(world, p);
                    continue;
                }
                let source = world
                    .resource::<DockDrag>()
                    .session
                    .as_ref()
                    .filter(|s| !s.seam)
                    .map(|s| s.source);
                if let Some(source) = source {
                    over(world, &shared, Some(source), p);
                }
            }
            DragInput::Ended(p) => {
                if world.resource::<DockDrag>().resize.is_some() {
                    resize_end(world, &shared, Some(p));
                    continue;
                }
                let source = world
                    .resource::<DockDrag>()
                    .session
                    .as_ref()
                    .filter(|s| !s.seam)
                    .map(|s| s.source);
                if let Some(source) = source {
                    drop_at(world, &shared, Some(source), p);
                }
            }
            DragInput::Seam(ev) => {
                let seam = world
                    .resource::<DockDrag>()
                    .session
                    .as_ref()
                    .is_some_and(|s| s.seam);
                if !seam {
                    continue;
                }
                match ev {
                    SurfaceDragEvent::Over { window, position } => {
                        over(world, &shared, window, position);
                    }
                    SurfaceDragEvent::Dropped { window, position } => {
                        drop_at(world, &shared, window, position);
                    }
                    SurfaceDragEvent::Cancelled => cancel(world, &shared, false),
                }
            }
            DragInput::Escape => {
                if world.resource::<DockDrag>().resize.is_some() {
                    resize_end(world, &shared, None);
                    announce_cancel(world);
                } else if world.resource::<DockDrag>().active() {
                    cancel(world, &shared, true);
                }
            }
        }
    }
    // A drop or a cancel changed the model after this tick's realize gate
    // looked: make the elements follow now, so nothing waits for another
    // event to wake the loop.
    if shared.lock().dirty {
        realize(world);
    }
}

/// The float element `e` is, or lies in, with its model id.
fn float_of(world: &World, e: Entity) -> Option<(Entity, String)> {
    let mut cur = Some(e);
    while let Some(x) = cur {
        if let Some(f) = world.get::<DockFloat>(x) {
            return Some((x, f.id.clone()));
        }
        cur = parent(world, x);
    }
    None
}

/// Put the float `id` on top of the others of its dock.
fn raise(core: &mut Core, id: &str) {
    if core.ws.raise_float(id) {
        core.dirty = true;
        core.save_due = true;
    }
}

/// A press on a float's resize handle moved past the threshold.
fn start_resize(world: &mut World, shared: &Shared, handle: Entity, start: Vec2) {
    let Some(h) = world.get::<DockFloatHandle>(handle).cloned() else {
        return;
    };
    let Some((element, _)) = float_of(world, handle) else {
        return;
    };
    let Some(dock) = parent(world, element) else {
        return;
    };
    let mut core = shared.lock();
    let Some((_, f)) = core.ws.float(&h.float) else {
        return;
    };
    let from = clamp_rect(f.rect, crate::frame::laid_out_size(world, dock));
    raise(&mut core, &h.float);
    drop(core);
    world.resource_mut::<DockDrag>().resize = Some(Resize {
        float: h.float,
        element,
        dock,
        edges: h.edges,
        start,
        from,
        rect: from,
    });
}

/// The pointer moved to `p` while a float is resized.
fn resize_to(world: &mut World, p: Vec2) {
    let Some(mut r) = world.resource_mut::<DockDrag>().resize.take() else {
        return;
    };
    let size = crate::frame::laid_out_size(world, r.dock);
    r.rect = clamp_rect(r.at(p), size);
    if alive(world, r.element) {
        place_float(world, r.element, r.rect);
    }
    world.resource_mut::<DockDrag>().resize = Some(r);
}

/// The resize ended at `p`, or was cancelled (`None`).
fn resize_end(world: &mut World, shared: &Shared, p: Option<Vec2>) {
    if let Some(p) = p {
        resize_to(world, p);
    }
    let Some(r) = world.resource_mut::<DockDrag>().resize.take() else {
        return;
    };
    let mut core = shared.lock();
    match p {
        Some(_) => {
            core.ws.set_float_rect(&r.float, r.rect);
            core.apply(Vec::new(), Reason::User);
        }
        // Back where it was: the elements follow the model again.
        None => core.dirty = true,
    }
}

fn start_drag(world: &mut World, shared: &Shared, entity: Entity, start: Vec2) {
    if world.resource::<DockDrag>().active() || !alive(world, entity) {
        return;
    }
    // A float's resize handle.
    let mut cur = Some(entity);
    while let Some(e) = cur {
        if world.get::<DockFloatHandle>(e).is_some() {
            start_resize(world, shared, e, start);
            return;
        }
        if world.get::<DockFloat>(e).is_some() {
            break;
        }
        cur = parent(world, e);
    }
    let mut core = shared.lock();
    let Some((payload, handle)) = payload_at(world, &core, entity) else {
        return;
    };
    let Some(source) = window_of(world, entity) else {
        return;
    };
    let before = core.ws.clone();
    let built = world.remove_resource::<Built>().unwrap_or_default();
    // A window's group moves its window, a float's whole group its float; a
    // tab keeps the point it was grabbed at.
    let first = first_panel(&core.ws, &payload);
    let loc = first.as_deref().and_then(|p| core.ws.find(p));
    let moving = match (&payload, &loc) {
        (Payload::Group(_), Some(l)) => match &l.container {
            Container::Window(w) => Some(w.clone()),
            _ => None,
        },
        _ => None,
    };
    let float = match (&payload, &loc) {
        (Payload::Group(stack), Some(l)) => match &l.container {
            Container::Float { float, .. }
                if core
                    .ws
                    .float(float)
                    .is_some_and(|(_, f)| f.root.id() == stack) =>
            {
                let element = built.frame.floats.get(float).copied();
                let dock = element.and_then(|e| parent(world, e));
                match (element, dock, core.ws.float(float)) {
                    (Some(element), Some(dock), Some((_, f))) => {
                        let origin = rect(world, element).map_or(Vec2::ZERO, |(o, _)| o);
                        Some(FloatMove {
                            id: float.clone(),
                            element,
                            dock,
                            grab: start - origin,
                            rect: clamp_rect(f.rect, crate::frame::laid_out_size(world, dock)),
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    };
    if let Some((_, id)) = float_of(world, entity) {
        raise(&mut core, &id);
    }
    let grab = if moving.is_some() {
        start
    } else {
        rect(world, handle).map_or(Vec2::new(16.0, 12.0), |(o, _)| start - o)
    };
    let seam = world.get_resource::<SurfaceDragSupport>().is_some();
    let follow = moving
        .as_ref()
        .and_then(|id| window_entity(world, &built, id))
        .filter(|_| {
            world
                .get_resource::<SurfaceDragSupport>()
                .is_some_and(|s| s.follow)
        });
    if seam {
        world.write_message(SurfaceDragRequest::Begin {
            source,
            follow,
            grab_offset: grab,
        });
    }
    let panels: Vec<PanelId> = match &payload {
        Payload::Panel(p) => vec![p.clone()],
        Payload::Group(s) => core
            .ws
            .stack(s)
            .map(|s| s.panels.clone())
            .unwrap_or_default(),
    };
    let floatable = panels.iter().all(|p| {
        core.panels
            .get(p)
            .is_none_or(|r| r.attrs.floatable && r.info.dockable)
    });
    // A new float a little smaller than the stack the drag starts from.
    let float_size = payload_stack(&core.ws, &payload)
        .and_then(|(stack, _)| built.nodes.get(&stack).copied())
        .and_then(|e| rect(world, e))
        .map_or(Vec2::new(320.0, 240.0), |(_, s)| s * 0.75)
        .clamp(Vec2::new(160.0, 120.0), Vec2::new(360.0, 260.0));
    let title = match &payload {
        Payload::Panel(p) => core.panels.get(p).map(|r| r.attrs.title.clone()),
        Payload::Group(s) => core
            .ws
            .stack(s)
            .and_then(|s| s.active.clone().or_else(|| s.panels.first().cloned()))
            .and_then(|p| core.panels.get(&p).map(|r| r.attrs.title.clone())),
    };
    let ghost = (moving.is_none() && float.is_none())
        .then(|| spawn_overlay(world, "dock-drag-ghost", title));
    let preview = Some(spawn_overlay(world, "dock-drop-preview", None));
    set_dragging(world, &core, true);
    drop(core);
    world.insert_resource(built);
    world.resource_mut::<DockDrag>().session = Some(Session {
        payload,
        source,
        grab,
        before,
        seam,
        over: Some((source, start)),
        zone: None,
        moving,
        torn: false,
        moving_hidden: false,
        open_on_release: false,
        float,
        float_size,
        floatable,
        ghost,
        preview,
    });
}

/// Move the float a drag carries so the pointer at `p` holds it where it
/// was grabbed.
fn move_float(world: &mut World, fm: &mut FloatMove, p: Vec2) {
    let Some((o, size)) = rect(world, fm.dock) else {
        return;
    };
    let at = p - fm.grab - o;
    fm.rect = clamp_rect(
        Rect {
            x: at.x,
            y: at.y,
            ..fm.rect
        },
        size,
    );
    if alive(world, fm.element) {
        place_float(world, fm.element, fm.rect);
    }
}

/// The pointer is over `window` at `p`, or over no window of the app.
fn over(world: &mut World, shared: &Shared, window: Option<Entity>, p: Vec2) {
    let Some(mut session) = world.resource_mut::<DockDrag>().session.take() else {
        return;
    };
    let built = world.remove_resource::<Built>().unwrap_or_default();
    let mut core = shared.lock();
    let moving_entity = session
        .moving
        .as_ref()
        .and_then(|id| window_entity(world, &built, id));
    match window.filter(|w| Some(*w) != moving_entity) {
        Some(w) => {
            session.over = Some((w, p));
            if let Some(fm) = session.float.as_mut()
                && w == session.source
            {
                move_float(world, fm, p);
            }
            // A float dragged by its title bar only moves; its tabs dock.
            let hit = if session.float.is_some() {
                Hit::Same
            } else {
                zone_at(world, &core, &built, (w, p), &session.payload)
            };
            let found = match hit {
                Hit::Zone(z, r) => Some((z, r)),
                // Over no stack, a tab or a docked group floats; a float only
                // moves.
                Hit::Nowhere if session.float.is_none() && session.floatable => {
                    float_zone(world, &core, w, p, session.grab, session.float_size)
                }
                Hit::Nowhere | Hit::Same => None,
            };
            // A window dragged by its group lands nowhere in itself.
            let found = found
                .filter(|_| !(session.moving.is_some() && w == session.source && !session.torn));
            let target = container(world, &core, &built, w, p);
            match (&found, target, session.preview) {
                (Some((zone, region)), Some(parent), Some(preview)) => {
                    set_classes(world, preview, &[zone.class().as_str()], &ZONE_CLASSES);
                    place_overlay(world, preview, parent, *region);
                }
                (_, _, Some(preview)) => hide(world, preview),
                _ => {}
            }
            if let (Some(ghost), Some(parent)) = (session.ghost, target) {
                if session.moving.is_some() && !session.moving_hidden {
                    hide(world, ghost);
                } else {
                    let size = world
                        .get::<Transform>(ghost)
                        .map(|t| t.size)
                        .filter(|s| s.x > 1.0)
                        .unwrap_or(Vec2::new(120.0, 28.0));
                    place_overlay(world, ghost, parent, (p - session.grab, size));
                }
            }
            // Over a window that would take the drop, the window being
            // dragged gets out of the way.
            if let Some(id) = session.moving.clone() {
                let hide_it =
                    found.is_some() && w != session.source || found.is_some() && session.torn;
                if hide_it != session.moving_hidden {
                    set_window_visible(world, &built, &id, !hide_it);
                    session.moving_hidden = hide_it;
                }
            }
            session.zone = found.map(|(z, _)| (w, z));
        }
        None => {
            session.zone = None;
            if let Some(preview) = session.preview {
                hide(world, preview);
            }
            if let Some(ghost) = session.ghost {
                hide(world, ghost);
            }
            if let Some(id) = session.moving.clone()
                && session.moving_hidden
            {
                set_window_visible(world, &built, &id, true);
                session.moving_hidden = false;
            }
            // Far enough off the window, a tab tears off.
            let size = world
                .get::<WindowViewport>(session.source)
                .map_or(Vec2::ZERO, |v| v.size);
            let distance = tearoff_distance(world, &core);
            if session.seam
                && session.moving.is_none()
                && !session.open_on_release
                && outside_by(p, size) >= distance
            {
                let follow = world
                    .get_resource::<SurfaceDragSupport>()
                    .is_some_and(|s| s.follow);
                if follow {
                    if let Ok(effects) = pop_out(&mut core, &session.payload) {
                        core.apply(effects, Reason::User);
                        session.moving = core
                            .ws
                            .find(
                                first_panel(&core.ws, &session.payload)
                                    .as_deref()
                                    .unwrap_or_default(),
                            )
                            .and_then(|l| match l.container {
                                Container::Window(w) => Some(w),
                                _ => None,
                            });
                        session.torn = session.moving.is_some();
                        session.float = None;
                        if let Some(ghost) = session.ghost {
                            hide(world, ghost);
                        }
                        // The window exists from now on, so it can follow.
                        drop(core);
                        world.insert_resource(built);
                        realize(world);
                        let built = world.resource::<Built>();
                        let follow = session
                            .moving
                            .as_ref()
                            .and_then(|id| built.windows.get(id).map(|(w, _)| *w));
                        if let Some(w) = follow {
                            world.write_message(SurfaceDragRequest::Follow(Some(w)));
                        }
                        world.resource_mut::<DockDrag>().session = Some(session);
                        return;
                    }
                } else {
                    session.open_on_release = true;
                }
            }
        }
    }
    drop(core);
    world.insert_resource(built);
    world.resource_mut::<DockDrag>().session = Some(session);
}

/// Move the stack `stack` to `target` with what the module knows of each
/// of its panels.
fn move_group(
    core: &mut Core,
    stack: &str,
    target: &Target,
) -> Result<Vec<lumen_dock_model::Effect>, lumen_dock_model::OpError> {
    let infos: std::collections::BTreeMap<PanelId, lumen_dock_model::PanelInfo> = core
        .ws
        .stack(stack)
        .map(|s| s.panels.iter().map(|p| (p.clone(), core.info(p))).collect())
        .unwrap_or_default();
    let fallback = core.info("");
    core.ws.move_group(stack, target, &|p| {
        infos.get(p).cloned().unwrap_or_else(|| fallback.clone())
    })
}

/// Pop what the drag carries out into a window of its own.
fn pop_out(
    core: &mut Core,
    payload: &Payload,
) -> Result<Vec<lumen_dock_model::Effect>, lumen_dock_model::OpError> {
    match payload {
        Payload::Panel(panel) => {
            let info = core.info(panel);
            core.ws.detach(panel, WindowPlace::default(), &info)
        }
        Payload::Group(stack) => {
            move_group(core, stack, &Target::NewWindow(WindowPlace::default()))
        }
    }
}

fn first_panel(ws: &Workspace, payload: &Payload) -> Option<PanelId> {
    match payload {
        Payload::Panel(p) => Some(p.clone()),
        Payload::Group(s) => ws.stack(s).and_then(|s| s.panels.first().cloned()),
    }
}

/// The button was released over `window` at `p`, or over no window.
fn drop_at(world: &mut World, shared: &Shared, window: Option<Entity>, p: Vec2) {
    // Bring the zone up to date with where the release happened.
    over(world, shared, window, p);
    let Some(session) = world.resource_mut::<DockDrag>().session.take() else {
        return;
    };
    let mut core = shared.lock();
    let mut moved = false;
    if let Some((_, zone)) = &session.zone {
        // The group, or the panel, wherever the drag has it now.
        let group = match &session.payload {
            Payload::Group(s) => Some(s.clone()),
            Payload::Panel(panel) if session.torn => core.ws.find(panel).map(|l| l.stack),
            Payload::Panel(_) => None,
        };
        let target = zone.target(&core.ws);
        let done = match (&session.payload, group) {
            (_, Some(stack)) => move_group(&mut core, &stack, &target),
            (Payload::Group(stack), None) => move_group(&mut core, stack, &target),
            (Payload::Panel(panel), None) => {
                let info = core.info(panel);
                core.ws.move_to(panel, &target, &info)
            }
        };
        match done {
            Ok(effects) => {
                core.apply(effects, Reason::User);
                moved = true;
            }
            Err(e) => lumen_module::lumen_core::warn_line!("lumen-dock: {e}"),
        }
    } else if let Some(fm) = &session.float {
        // A float dropped over no zone stays where it was taken.
        if core.ws.set_float_rect(&fm.id, fm.rect) {
            core.apply(Vec::new(), Reason::User);
        }
    } else if window.is_none() && session.seam && session.moving.is_none() {
        // Released over no window with nothing following the pointer (no
        // window can follow, or the pointer left faster than the drag
        // could tear off): the pop-out opens where the drop was, which the
        // platform decides.
        if let Ok(effects) = pop_out(&mut core, &session.payload) {
            core.apply(effects, Reason::User);
            moved = true;
        }
    }
    let built = world.remove_resource::<Built>().unwrap_or_default();
    if let Some(id) = &session.moving {
        // A window that was hidden over a target shows again where it went.
        if !moved || core.ws.window(id).is_some() {
            set_window_visible(world, &built, id, true);
        }
    }
    finish(world, &core, &session);
    drop(core);
    world.insert_resource(built);
}

/// Put everything back as it was before the drag.
fn cancel(world: &mut World, shared: &Shared, by_key: bool) {
    let Some(session) = world.resource_mut::<DockDrag>().session.take() else {
        return;
    };
    let mut core = shared.lock();
    if core.ws != session.before {
        let effects = core.ws.replace_with(session.before.clone());
        core.apply(effects, Reason::User);
    }
    if session.float.is_some() {
        // The float goes back where the model has it.
        core.dirty = true;
    }
    let built = world.remove_resource::<Built>().unwrap_or_default();
    if let Some(id) = &session.moving
        && !session.torn
    {
        set_window_visible(world, &built, id, true);
    }
    finish(world, &core, &session);
    if by_key {
        announce_cancel(world);
    }
    drop(core);
    world.insert_resource(built);
}

/// Take the drag's overlays away and tell the backend it is over.
fn finish(world: &mut World, core: &Core, session: &Session) {
    for e in [session.ghost, session.preview].into_iter().flatten() {
        if let Ok(ent) = world.get_entity_mut(e) {
            ent.despawn();
        }
    }
    set_dragging(world, core, false);
    if session.seam {
        world.write_message(SurfaceDragRequest::End);
    }
}

/// Tell a screen reader where a drag that changed nothing left things.
pub fn announce_cancel(world: &mut World) {
    let said = text("lumen-dock-drag-cancelled", "Move cancelled");
    world
        .resource_mut::<A11yAnnouncementQueue>()
        .pending
        .push((said, A11yLive::Polite));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_name_their_classes_and_targets() {
        let mut ws = Workspace::default();
        ws.docks.insert(
            "d".into(),
            lumen_dock_model::DockArea::new(lumen_dock_model::Node::Stack(
                lumen_dock_model::Stack::with("s", ["a", "b"]),
            )),
        );
        let cases = [
            (
                Zone::Tab {
                    stack: "s".into(),
                    index: 1,
                },
                "dock-drop-tab",
                Target::Tab {
                    stack: "s".into(),
                    index: 1,
                },
            ),
            (
                Zone::Center { stack: "s".into() },
                "dock-drop-center",
                Target::Tab {
                    stack: "s".into(),
                    index: 2,
                },
            ),
            (
                Zone::Edge {
                    stack: "s".into(),
                    side: Side::Left,
                },
                "dock-drop-left",
                Target::Edge {
                    node: "s".into(),
                    side: Side::Left,
                },
            ),
            (
                Zone::RootEdge {
                    dock: "d".into(),
                    side: Side::Bottom,
                },
                "dock-drop-root-bottom",
                Target::RootEdge {
                    dock: "d".into(),
                    side: Side::Bottom,
                },
            ),
        ];
        for (zone, class, target) in cases {
            assert_eq!(zone.class(), class);
            assert!(ZONE_CLASSES.contains(&class));
            assert_eq!(zone.target(&ws), target);
        }
        for (side, name) in [(Side::Right, "right"), (Side::Top, "top")] {
            assert_eq!(side_name(side), name);
        }
    }

    #[test]
    fn the_distance_off_a_window_counts_the_farthest_edge() {
        let size = Vec2::new(100.0, 50.0);
        assert_eq!(outside_by(Vec2::new(10.0, 10.0), size), 0.0);
        assert_eq!(outside_by(Vec2::new(-30.0, 10.0), size), 30.0);
        assert_eq!(outside_by(Vec2::new(120.0, 90.0), size), 40.0);
        assert!(inside(Vec2::new(1.0, 1.0), (Vec2::ZERO, Vec2::ONE * 2.0)));
        assert!(!inside(Vec2::new(2.0, 1.0), (Vec2::ZERO, Vec2::ONE * 2.0)));
    }
}
