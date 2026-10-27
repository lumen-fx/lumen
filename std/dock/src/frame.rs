//! The parts of a dock around its tree: the groups floating over it and the
//! panels pinned to its edges.
//!
//! A float is an element of the dock's, laid over the tree at the
//! rectangle the model gives it, holding the float's stack (or small tree)
//! and a resize frame along its edges and corners. Its stack's strip is its
//! title bar.
//!
//! A dock with pinned panels lays out as a frame: a strip of buttons along
//! each edge that has panels pinned to it, and the tree in the middle. The
//! panel slid out of a strip lies over the middle along that edge, under a
//! header with its title and a button that docks it back.

use std::collections::{BTreeMap, HashMap, HashSet};

use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_dock_model::{DockArea, Float, PanelId, Side};
use lumen_module::lumen_core::components::{
    A11yLabel, A11yRole, A11yState, TextContent, Transform,
};
use lumen_module::lumen_core::render_world::{OverlayLayer, OverlayOpenOrder};

use crate::adopt::spawn_chrome;
use crate::realize::{alive, children_of, reorder, set_classes, set_inline};

/// On a float's element: the float's id in the model.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockFloat {
    /// The float's id.
    pub id: String,
}

/// Which edges of a float a resize handle moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    /// The left edge.
    pub left: bool,
    /// The right edge.
    pub right: bool,
    /// The top edge.
    pub top: bool,
    /// The bottom edge.
    pub bottom: bool,
}

/// On one of a float's resize handles.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockFloatHandle {
    /// The float.
    pub float: String,
    /// The edges it moves.
    pub edges: Edges,
}

/// The eight handles of a float's resize frame: the class suffix and the
/// edges each moves.
const HANDLES: [(&str, Edges); 8] = [
    (
        "n",
        Edges {
            left: false,
            right: false,
            top: true,
            bottom: false,
        },
    ),
    (
        "s",
        Edges {
            left: false,
            right: false,
            top: false,
            bottom: true,
        },
    ),
    (
        "w",
        Edges {
            left: true,
            right: false,
            top: false,
            bottom: false,
        },
    ),
    (
        "e",
        Edges {
            left: false,
            right: true,
            top: false,
            bottom: false,
        },
    ),
    (
        "nw",
        Edges {
            left: true,
            right: false,
            top: true,
            bottom: false,
        },
    ),
    (
        "ne",
        Edges {
            left: false,
            right: true,
            top: true,
            bottom: false,
        },
    ),
    (
        "sw",
        Edges {
            left: true,
            right: false,
            top: false,
            bottom: true,
        },
    ),
    (
        "se",
        Edges {
            left: false,
            right: true,
            top: false,
            bottom: true,
        },
    ),
];

/// The smallest a float gets, in logical pixels.
pub const FLOAT_MIN: Vec2 = Vec2::new(120.0, 80.0);

/// On a pinned panel's button in its dock's edge strip.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockPinButton {
    /// The panel.
    pub panel: String,
}

/// On the element a pinned panel slides out in.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockSlideOut {
    /// The dock.
    pub dock: String,
    /// The panel it shows.
    pub panel: String,
}

/// On the button in a slid-out panel's header that docks it back.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockUnpinButton {
    /// The panel.
    pub panel: String,
}

/// On the caption of a slid-out panel's header.
#[derive(Component, Clone, Copy, Debug)]
pub struct DockSlideOutCaption;

/// What the dock's frame built, kept across realizations.
#[derive(Default)]
pub struct FrameParts {
    /// Each float's element, by float id.
    pub floats: HashMap<String, Entity>,
    /// Each dock's middle row, when it has pinned panels.
    pub middles: HashMap<String, Entity>,
    /// Each dock edge's strip.
    pub strips: HashMap<(String, Side), Entity>,
    /// Each pinned panel's button.
    pub pins: HashMap<PanelId, Entity>,
    /// Each dock's slide-out.
    pub slide_outs: HashMap<String, Entity>,
}

/// What one realization of the frame used, so the rest can go.
#[derive(Default)]
pub struct FrameUse {
    floats: HashSet<String>,
    middles: HashSet<String>,
    strips: HashSet<(String, Side)>,
    pins: HashSet<PanelId>,
    slide_outs: HashSet<String>,
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

const PINNED_CLASSES: [&str; 4] = [
    "dock-pinned-left",
    "dock-pinned-right",
    "dock-pinned-top",
    "dock-pinned-bottom",
];

/// A float's rectangle kept inside a dock of `size`: never larger than the
/// dock, never past its edges. A size of zero (not laid out yet) keeps it.
pub fn clamp_rect(rect: lumen_dock_model::Rect, size: Vec2) -> lumen_dock_model::Rect {
    if size.x < 1.0 || size.y < 1.0 {
        return rect;
    }
    let w = rect.w.clamp(FLOAT_MIN.x.min(size.x), size.x);
    let h = rect.h.clamp(FLOAT_MIN.y.min(size.y), size.y);
    lumen_dock_model::Rect {
        x: rect.x.clamp(0.0, size.x - w),
        y: rect.y.clamp(0.0, size.y - h),
        w,
        h,
    }
}

/// Write a float element's place: `rect` from its dock's top-left corner.
pub fn place_float(world: &mut World, e: Entity, rect: lumen_dock_model::Rect) {
    set_inline(world, e, "position", Some("absolute".into()));
    set_inline(
        world,
        e,
        "inset",
        Some(format!("{} auto auto {}", rect.y.round(), rect.x.round())),
    );
    set_inline(world, e, "width", Some(format!("{}px", rect.w.round())));
    set_inline(world, e, "height", Some(format!("{}px", rect.h.round())));
}

/// The size `e` was last laid out at.
pub fn laid_out_size(world: &World, e: Entity) -> Vec2 {
    world.get::<Transform>(e).map_or(Vec2::ZERO, |t| t.size)
}

/// Build or update the element of `float`, holding `node`. Returns it.
pub fn build_float(
    world: &mut World,
    parts: &mut FrameParts,
    used: &mut FrameUse,
    dock: Entity,
    float: &Float,
    node: Entity,
) -> Entity {
    used.floats.insert(float.id.clone());
    let e = match parts
        .floats
        .get(&float.id)
        .copied()
        .filter(|&e| alive(world, e))
    {
        Some(e) => e,
        None => {
            // In the top layer, as it paints and takes the pointer over the
            // dock's tree whatever lies deeper there.
            let e = spawn_chrome(world, "column", &["dock-float"], Some(dock));
            world.entity_mut(e).insert((
                DockFloat {
                    id: float.id.clone(),
                },
                A11yRole::Group,
                OverlayLayer,
            ));
            for (suffix, edges) in HANDLES {
                let class = format!("dock-float-resize-{suffix}");
                let h = spawn_chrome(world, "column", &["dock-float-resize", &class], Some(e));
                // Top-layer roots of their own, stacked over the float, so
                // they take the pointer over whatever the float holds.
                world.entity_mut(h).insert((
                    DockFloatHandle {
                        float: float.id.clone(),
                        edges,
                    },
                    OverlayLayer,
                ));
            }
            parts.floats.insert(float.id.clone(), e);
            e
        }
    };
    let rect = clamp_rect(float.rect, laid_out_size(world, dock));
    place_float(world, e, rect);
    let handles: Vec<Entity> = children_of(world, e)
        .into_iter()
        .filter(|&k| world.get::<DockFloatHandle>(k).is_some())
        .collect();
    let wanted: Vec<Entity> = std::iter::once(node).chain(handles).collect();
    let current = children_of(world, e);
    if current != wanted {
        for k in current {
            if !wanted.contains(&k) {
                world.entity_mut(k).remove::<bevy_ecs::hierarchy::ChildOf>();
            }
        }
        reorder(world, e, &wanted);
    }
    e
}

/// A panel's title, as its pin button and slid-out header show it.
pub type Titles<'a> = &'a BTreeMap<PanelId, String>;

/// Make `wanted` the first children of `parent`, in that order, keeping its
/// other children after them.
fn set_prefix(world: &mut World, parent: Entity, wanted: &[Entity]) {
    let current = children_of(world, parent);
    if current.len() >= wanted.len() && current[..wanted.len()] == *wanted {
        return;
    }
    let all: Vec<Entity> = wanted
        .iter()
        .copied()
        .chain(current.into_iter().filter(|k| !wanted.contains(k)))
        .collect();
    reorder(world, parent, &all);
}

/// One dock's tree with what goes around it, for [`arrange`].
pub struct Around<'a> {
    /// The dock's id and element.
    pub dock: (&'a str, Entity),
    /// Its model.
    pub area: &'a DockArea,
    /// The live panels' titles.
    pub titles: Titles<'a>,
    /// The tree's element.
    pub root: Option<Entity>,
    /// The floats' elements, bottom-most first.
    pub floats: Vec<Entity>,
    /// The slid-out panel's element.
    pub open_panel: Option<Entity>,
    /// The slide-out's size along the dock's axis it lies across, when the
    /// panel had one where it was docked.
    pub open_size: Option<f32>,
}

/// Stack the floats in the top layer in the model's order, the last on top,
/// above the top-layer elements that opened before them.
fn stack_floats(world: &mut World, floats: &[Entity]) {
    // Each float, then its resize handles over it.
    let layers: Vec<Entity> = floats
        .iter()
        .flat_map(|&f| {
            std::iter::once(f).chain(
                children_of(world, f)
                    .into_iter()
                    .filter(|&k| world.get::<DockFloatHandle>(k).is_some()),
            )
        })
        .collect();
    let mut order = world.get_resource_or_insert_with(OverlayOpenOrder::default);
    let mut last = None;
    for f in layers {
        let stamp = order.stamps.get(&f).copied();
        if stamp.is_none() || stamp <= last {
            let next = order.next;
            order.stamps.insert(f, next);
            order.next += 1;
            last = Some(next);
        } else {
            last = stamp;
        }
    }
}

/// Lay a dock out: the tree, the floats over it, and, when panels are
/// pinned, the strips along its edges and the slide-out.
pub fn arrange(world: &mut World, parts: &mut FrameParts, used: &mut FrameUse, a: Around<'_>) {
    stack_floats(world, &a.floats);
    let (dock_id, dock) = a.dock;
    let area = a.area;
    let titles = a.titles;
    let live = |p: &PanelId| titles.contains_key(p);
    let edges: Vec<(Side, Vec<&PanelId>)> = area
        .sidebars
        .edges()
        .into_iter()
        .map(|(s, ps)| (s, ps.iter().filter(|p| live(p)).collect::<Vec<_>>()))
        .filter(|(_, ps)| !ps.is_empty())
        .collect();
    let pinned: Vec<&str> = edges
        .iter()
        .map(|(s, _)| PINNED_CLASSES[*s as usize])
        .collect();
    set_classes(world, dock, &pinned, &PINNED_CLASSES);
    if edges.is_empty() {
        let prefix: Vec<Entity> = a.root.into_iter().chain(a.floats).collect();
        set_prefix(world, dock, &prefix);
        return;
    }
    used.middles.insert(dock_id.to_string());
    let middle = match parts
        .middles
        .get(dock_id)
        .copied()
        .filter(|&e| alive(world, e))
    {
        Some(e) => e,
        None => {
            let e = spawn_chrome(world, "row", &["dock-middle"], Some(dock));
            parts.middles.insert(dock_id.to_string(), e);
            e
        }
    };
    let mut strips: HashMap<Side, Entity> = HashMap::new();
    for (side, panels) in &edges {
        let key = (dock_id.to_string(), *side);
        used.strips.insert(key.clone());
        let strip = match parts.strips.get(&key).copied().filter(|&e| alive(world, e)) {
            Some(e) => e,
            None => {
                let tag = match side {
                    Side::Left | Side::Right => "column",
                    Side::Top | Side::Bottom => "row",
                };
                let class = format!("dock-sidebar-{}", side_name(*side));
                let e = spawn_chrome(world, tag, &["dock-sidebar", &class], Some(middle));
                world.entity_mut(e).insert(A11yRole::Toolbar);
                parts.strips.insert(key, e);
                e
            }
        };
        let mut buttons = Vec::new();
        for p in panels {
            used.pins.insert((*p).clone());
            let title = titles[*p].clone();
            let b = match parts.pins.get(*p).copied().filter(|&e| alive(world, e)) {
                Some(b) => b,
                None => {
                    let b = spawn_chrome(world, "button", &["dock-sidebar-button"], Some(strip));
                    world.entity_mut(b).insert(A11yRole::Button);
                    parts.pins.insert((*p).clone(), b);
                    b
                }
            };
            world.entity_mut(b).insert(DockPinButton {
                panel: (*p).clone(),
            });
            if world.get::<TextContent>(b).map(|t| t.0.as_str()) != Some(title.as_str()) {
                world.entity_mut(b).insert((
                    TextContent(title.clone()),
                    A11yLabel(title),
                    lumen_module::lumen_core::components::DirtyLayout,
                ));
            }
            let open = area.open.as_ref() == Some(*p);
            set_classes(world, b, if open { &["open"] } else { &[] }, &["open"]);
            let mut state = world.get::<A11yState>(b).copied().unwrap_or_default();
            if state.contains(A11yState::EXPANDED) != open || world.get::<A11yState>(b).is_none() {
                state.set(A11yState::EXPANDED, open);
                world.entity_mut(b).insert(state);
            }
            buttons.push(b);
        }
        if children_of(world, strip) != buttons {
            reorder(world, strip, &buttons);
        }
        strips.insert(*side, strip);
    }
    // The slid-out panel: a header with its title and the button that docks
    // it back, the panel under it, over the middle along its edge.
    let slide_out = match (area.open.as_ref().filter(|p| live(p)), a.open_panel) {
        (Some(p), Some(panel)) => edges
            .iter()
            .find(|(_, ps)| ps.contains(&p))
            .map(|(side, _)| (p, panel, *side)),
        _ => None,
    }
    .map(|(p, panel, side)| {
        used.slide_outs.insert(dock_id.to_string());
        let e = match parts
            .slide_outs
            .get(dock_id)
            .copied()
            .filter(|&e| alive(world, e))
        {
            Some(e) => e,
            None => {
                let e = spawn_chrome(world, "column", &["dock-sidebar-open"], Some(middle));
                world.entity_mut(e).insert(OverlayLayer);
                let header = spawn_chrome(world, "row", &["dock-sidebar-header"], Some(e));
                let caption = spawn_chrome(world, "label", &["dock-sidebar-title"], Some(header));
                world.entity_mut(caption).insert(DockSlideOutCaption);
                let unpin = spawn_chrome(world, "button", &["dock-sidebar-dock"], Some(header));
                let label = crate::plugin::text("lumen-dock-unpin", "Dock");
                world.entity_mut(unpin).insert((
                    A11yRole::Button,
                    A11yLabel(label.clone()),
                    TextContent(label),
                    DockUnpinButton { panel: p.clone() },
                ));
                parts.slide_outs.insert(dock_id.to_string(), e);
                e
            }
        };
        let side_class = format!("dock-sidebar-open-{}", side_name(side));
        set_classes(
            world,
            e,
            &[side_class.as_str()],
            &[
                "dock-sidebar-open-left",
                "dock-sidebar-open-right",
                "dock-sidebar-open-top",
                "dock-sidebar-open-bottom",
            ],
        );
        let (dim, other) = match side {
            Side::Left | Side::Right => ("width", "height"),
            Side::Top | Side::Bottom => ("height", "width"),
        };
        set_inline(world, e, dim, a.open_size.map(|s| format!("{s}px")));
        set_inline(world, e, other, None);
        if world.get::<DockSlideOut>(e).is_none_or(|s| s.panel != *p) {
            world.entity_mut(e).insert(DockSlideOut {
                dock: dock_id.to_string(),
                panel: p.clone(),
            });
        }
        // The header follows the panel shown.
        let title = titles[p].clone();
        let header = children_of(world, e).first().copied();
        for k in header.map(|h| children_of(world, h)).unwrap_or_default() {
            if world.get::<DockSlideOutCaption>(k).is_some()
                && world.get::<TextContent>(k).map(|t| t.0.as_str()) != Some(title.as_str())
            {
                world.entity_mut(k).insert((
                    TextContent(title.clone()),
                    lumen_module::lumen_core::components::DirtyLayout,
                ));
            }
            if let Some(mut b) = world.get_mut::<DockUnpinButton>(k)
                && b.panel != *p
            {
                b.panel = p.clone();
            }
        }
        if let Some(header) = header {
            set_prefix(world, e, &[header, panel]);
        }
        e
    });
    let mut row: Vec<Entity> = Vec::new();
    row.extend(strips.get(&Side::Left));
    row.extend(a.root);
    row.extend(strips.get(&Side::Right));
    row.extend(slide_out);
    if children_of(world, middle) != row {
        // Whatever else was in the middle has moved on.
        for k in children_of(world, middle) {
            if !row.contains(&k) {
                world.entity_mut(k).remove::<bevy_ecs::hierarchy::ChildOf>();
            }
        }
        reorder(world, middle, &row);
    }
    let mut prefix: Vec<Entity> = Vec::new();
    prefix.extend(strips.get(&Side::Top));
    prefix.push(middle);
    prefix.extend(strips.get(&Side::Bottom));
    prefix.extend(a.floats);
    set_prefix(world, dock, &prefix);
}

/// Take away what the frame built and no longer uses.
pub fn sweep(world: &mut World, parts: &mut FrameParts, used: &FrameUse) {
    let mut gone: Vec<Entity> = Vec::new();
    parts.floats.retain(|id, e| {
        let keep = used.floats.contains(id);
        if !keep {
            gone.push(*e);
        }
        keep
    });
    parts.pins.retain(|id, e| {
        let keep = used.pins.contains(id);
        if !keep {
            gone.push(*e);
        }
        keep
    });
    parts.slide_outs.retain(|id, e| {
        let keep = used.slide_outs.contains(id);
        if !keep {
            gone.push(*e);
        }
        keep
    });
    parts.strips.retain(|id, e| {
        let keep = used.strips.contains(id);
        if !keep {
            gone.push(*e);
        }
        keep
    });
    parts.middles.retain(|id, e| {
        let keep = used.middles.contains(id);
        if !keep {
            gone.push(*e);
        }
        keep
    });
    for e in gone {
        if let Ok(ent) = world.get_entity_mut(e) {
            ent.despawn();
        }
    }
}
