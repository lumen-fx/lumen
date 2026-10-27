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
use lumen_dock_model::{Container, PanelId, Side, Target, WindowPlace, Workspace};
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
        }
    }
}

const ZONE_CLASSES: [&str; 10] = [
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
    ghost: Option<Entity>,
    preview: Option<Entity>,
}

/// The drag the dock is running, if any.
#[derive(Resource, Default, Debug)]
pub struct DockDrag {
    session: Option<Session>,
}

impl DockDrag {
    /// Whether a drag is in progress.
    pub fn active(&self) -> bool {
        self.session.is_some()
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
/// a single-panel window's group; `None` for a close button or anything that
/// is not the dock's.
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

/// The dock zone under `p` in `window`, for `payload`; `None` where a drop
/// would change nothing or lands nowhere.
fn zone_at(
    world: &mut World,
    core: &Core,
    built: &Built,
    window: Entity,
    p: Vec2,
    payload: &Payload,
) -> Option<(Zone, (Vec2, Vec2))> {
    let in_window = |world: &mut World, e: Entity| window_of(world, e) == Some(window);
    // The innermost shown stack under the pointer.
    let mut best: Option<(String, Entity, (Vec2, Vec2))> = None;
    for (id, &e) in &built.nodes {
        if world.get::<DockStack>(e).is_none() || !alive(world, e) || !in_window(world, e) {
            continue;
        }
        if !shown(world, e) {
            continue;
        }
        let Some(r) = rect(world, e) else { continue };
        if inside(p, r)
            && best
                .as_ref()
                .is_none_or(|b| r.1.x * r.1.y < b.2.1.x * b.2.1.y)
        {
            best = Some((id.clone(), e, r));
        }
    }
    let (own, own_len) = payload_stack(&core.ws, payload).unwrap_or_default();
    let (stack, e, (o, s)) = match best {
        Some(b) => b,
        None => return root_edge(world, core, window, p, payload),
    };
    let is_group = matches!(payload, Payload::Group(_));
    if own == stack && (is_group || own_len <= 1) {
        // Onto itself: nothing to do.
        return None;
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
                return None;
            }
        }
        let region = (
            Vec2::new(marker_x - TAB_MARKER / 2.0, sr.0.y),
            Vec2::new(TAB_MARKER, sr.1.y),
        );
        return Some((Zone::Tab { stack, index }, region));
    }
    // A whole dock's edges, outside its strips.
    if let Some(found) = root_edge(world, core, window, p, payload) {
        return Some(found);
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
        return Some((Zone::Edge { stack, side }, region));
    }
    if own == stack {
        // Its own stack's middle is where it already is.
        return None;
    }
    let below = strip
        .and_then(|k| rect(world, k))
        .map_or(0.0, |(_, ss)| ss.y);
    Some((
        Zone::Center { stack },
        (
            Vec2::new(o.x, o.y + below),
            Vec2::new(s.x, (s.y - below).max(1.0)),
        ),
    ))
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
                if world.resource::<DockDrag>().active() {
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

fn start_drag(world: &mut World, shared: &Shared, entity: Entity, start: Vec2) {
    if world.resource::<DockDrag>().active() || !alive(world, entity) {
        return;
    }
    let core = shared.lock();
    let Some((payload, handle)) = payload_at(world, &core, entity) else {
        return;
    };
    let Some(source) = window_of(world, entity) else {
        return;
    };
    let before = core.ws.clone();
    let built = world.remove_resource::<Built>().unwrap_or_default();
    // A window's group moves its window; a tab keeps the point it was
    // grabbed at.
    let moving = match &payload {
        Payload::Group(stack) => match core.ws.find(
            core.ws
                .stack(stack)
                .and_then(|s| s.panels.first())
                .map(String::as_str)
                .unwrap_or_default(),
        ) {
            Some(loc) => match loc.container {
                Container::Window(w) => Some(w),
                Container::Dock(_) => None,
            },
            None => None,
        },
        Payload::Panel(_) => None,
    };
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
    let title = match &payload {
        Payload::Panel(p) => core.panels.get(p).map(|r| r.attrs.title.clone()),
        Payload::Group(s) => core
            .ws
            .stack(s)
            .and_then(|s| s.active.clone().or_else(|| s.panels.first().cloned()))
            .and_then(|p| core.panels.get(&p).map(|r| r.attrs.title.clone())),
    };
    let ghost = moving
        .is_none()
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
        ghost,
        preview,
    });
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
            let found = zone_at(world, &core, &built, w, p, &session.payload);
            // A window dragged by its group lands nowhere in itself.
            let found = found
                .filter(|_| !(session.moving.is_some() && w == session.source && !session.torn));
            let target = container(world, &core, &built, w, p);
            match (&found, target, session.preview) {
                (Some((_, region)), Some(parent), Some(preview)) => {
                    let (zone, _) = found.clone().expect("matched");
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
                                Container::Dock(_) => None,
                            });
                        session.torn = session.moving.is_some();
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
