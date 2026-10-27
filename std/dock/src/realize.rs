//! Making the elements follow the model.
//!
//! Each stack is a core `<tabs>` with one `<tab>` per panel, the panel moved
//! into its tab; each split is a core `<split>`; each window is a core
//! window entity whose surface root holds the window's tree. The structure is
//! kept per model id across changes: a rearrangement moves existing
//! elements and builds or removes only what changed, so a panel's content
//! keeps every bit of its state wherever it goes.

use std::collections::{BTreeMap, HashMap, HashSet};

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use glam::{IVec2, Vec2};
use lumen_dock_model::{Axis, Node, PanelId, Stack, TabsMode, WindowLayout};
use lumen_module::lumen_core::components::{
    A11yLabel, DirtyLayout, InlineStyle, Length, LumenAttributes, LumenClasses, Style, StyleDirty,
    Transform,
};
use lumen_module::lumen_core::style_overrides::{StyleOwner, StyleValues, hold_on, release_on};
use lumen_module::lumen_core::window::{
    InputBlocked, Monitors, PrimaryWindow, Window, WindowKind, WindowName, WindowOf, WindowRuntime,
};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element, TabSpec};
use lumen_module::lumen_primitives::{SplitGutter, SplitPane, TabStrip};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::adopt::{DockChrome, hide, spawn_chrome, unhide};
use crate::state::{Core, Modal, Shared};

/// What a dock tab's strip button says itself; its title is in its
/// caption element.
pub const BLANK: &str = " ";

/// The tab a panel sits in, on the `<tab>` the module built for it.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockTab {
    /// The panel.
    pub panel: String,
}

/// A stack the module built, on its `<tabs>`.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockStack {
    /// The stack's id in the model.
    pub id: String,
}

/// A window the module opened, on the window entity.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockWindow {
    /// The window's id in the model.
    pub id: String,
}

/// The parts of a split a maximized panel covers hold its place.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DockMaximized;

impl StyleOwner for DockMaximized {
    const RANK: u8 = 46;
}

/// What the module built, by model id.
#[derive(Resource, Default)]
pub struct Built {
    /// Stacks and splits, by node id.
    pub nodes: HashMap<String, Entity>,
    /// Tab wrappers, by panel.
    pub tabs: HashMap<PanelId, Entity>,
    /// Each dock's store, by dock.
    pub stores: HashMap<String, Entity>,
    /// Windows: the window entity and its surface root, by window id.
    pub windows: HashMap<String, (Entity, Entity)>,
    /// Windows the module blocked for a modal window.
    pub blocked: HashSet<Entity>,
    /// The parts a maximized panel hides.
    pub max_hidden: HashSet<Entity>,
}

fn alive(world: &World, e: Entity) -> bool {
    world.get_entity(e).is_ok()
}

fn children_of(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.to_vec())
        .unwrap_or_default()
}

/// Make `child` a child of `parent` unless it is already.
fn put_under(world: &mut World, child: Entity, parent: Entity) {
    if world.get::<ChildOf>(child).map(ChildOf::parent) != Some(parent) {
        world.entity_mut(child).insert(ChildOf(parent));
    }
}

/// Make `wanted` the children of `parent`, in that order. Each is moved
/// under `parent` first with an ordinary insert, which takes it off its old
/// parent's list; `replace_children` alone would leave it there too, and
/// the old parent's despawn would take it along.
fn reorder(world: &mut World, parent: Entity, wanted: &[Entity]) {
    for &k in wanted {
        put_under(world, k, parent);
    }
    world.entity_mut(parent).replace_children(wanted);
    world.entity_mut(parent).insert(DirtyLayout);
}

/// Write an inline style property, restyling when it changed.
fn set_inline(world: &mut World, e: Entity, prop: &str, value: Option<String>) {
    let mut ent = world.entity_mut(e);
    let mut inline = ent.take::<InlineStyle>().unwrap_or_default();
    let changed = match &value {
        Some(v) => {
            if inline.get(prop) == Some(v.as_str()) {
                false
            } else {
                inline.set(prop, v.clone());
                true
            }
        }
        None => inline.remove(prop).is_some(),
    };
    ent.insert(inline);
    if changed {
        ent.insert((StyleDirty, DirtyLayout));
    }
}

/// Keep `classes` on `e` as the classes under the module's control: every
/// class in `managed` that is not wanted goes, every wanted one comes.
fn set_classes(world: &mut World, e: Entity, wanted: &[&str], managed: &[&str]) {
    let mut ent = world.entity_mut(e);
    let mut classes = ent.take::<LumenClasses>().unwrap_or_default();
    let before = classes.0.clone();
    classes
        .0
        .retain(|c| !managed.contains(&c.as_ref()) || wanted.contains(&c.as_ref()));
    for w in wanted {
        if !classes.has(w) {
            classes.0.push((*w).into());
        }
    }
    let changed = classes.0 != before;
    ent.insert(classes);
    if changed {
        ent.insert(StyleDirty);
    }
}

const STACK_CLASSES: [&str; 5] = ["center", "collapsed", "single", "no-tabs", "maximized"];

/// A panel's facts the realization reads.
struct Live {
    entity: Entity,
    title: String,
    os_title: String,
    min: Option<(f32, f32)>,
}

fn live_panels(world: &World, core: &Core) -> BTreeMap<PanelId, Live> {
    core.panels
        .iter()
        .filter_map(|(id, rec)| {
            let e = rec.entity.filter(|&e| alive(world, e))?;
            let style = world.get::<Style>(e);
            let px = |l: Option<Length>| match l {
                Some(Length::Px(v)) => v,
                _ => 0.0,
            };
            let min = style.map(|s| (px(Some(s.min_width)), px(Some(s.min_height))));
            Some((
                id.clone(),
                Live {
                    entity: e,
                    title: rec.attrs.title.clone(),
                    os_title: rec.os_title().to_string(),
                    min,
                },
            ))
        })
        .collect()
}

struct Pass<'a> {
    live: &'a BTreeMap<PanelId, Live>,
    used_nodes: HashSet<String>,
    used_tabs: HashSet<PanelId>,
    /// The ids from the root to the maximized panel's stack, when one is.
    max_path: Vec<String>,
    max_hidden: Vec<Entity>,
    max_held: Vec<Entity>,
}

/// Build or update the elements of `node`. Returns the element, or `None`
/// when nothing of it has a live panel to show.
fn build(
    world: &mut World,
    built: &mut Built,
    pass: &mut Pass<'_>,
    node: &Node,
    is_root: bool,
) -> Option<Entity> {
    match node {
        Node::Stack(stack) => build_stack(world, built, pass, stack, is_root),
        Node::Split(split) => {
            let mut panes: Vec<(Entity, &lumen_dock_model::Child)> = Vec::new();
            for child in &split.children {
                if let Some(e) = build(world, built, pass, &child.node, false) {
                    panes.push((e, child));
                }
            }
            match panes.len() {
                0 => return None,
                1 => return Some(panes[0].0),
                _ => {}
            }
            pass.used_nodes.insert(split.id.clone());
            let axis_class = match split.axis {
                Axis::Row => "dock-split-row",
                Axis::Column => "dock-split-column",
            };
            let e = match built
                .nodes
                .get(&split.id)
                .copied()
                .filter(|&e| alive(world, e))
            {
                Some(e) => e,
                None => {
                    let e = spawn_chrome(world, "split", &["dock-split"], None);
                    built.nodes.insert(split.id.clone(), e);
                    e
                }
            };
            set_classes(
                world,
                e,
                &[axis_class],
                &["dock-split-row", "dock-split-column"],
            );
            let on_path = pass.max_path.contains(&split.id);
            for &(pane, child) in &panes {
                let basis = child.basis.map(|b| format!("{b}px"));
                let before = world
                    .get::<InlineStyle>(pane)
                    .and_then(|s| s.get("flex-basis"))
                    .map(str::to_string);
                if before != basis {
                    // The model's size wins over the one a gutter drag held.
                    if let Ok(mut ent) = world.get_entity_mut(pane) {
                        release_on::<SplitPane>(&mut ent);
                    }
                }
                set_inline(
                    world,
                    pane,
                    "flex-basis",
                    // A child with no size of its own shares what the sized ones
                    // leave, as `flex: 1` does, whatever its content measures.
                    Some(basis.unwrap_or_else(|| "0px".into())),
                );
                set_inline(world, pane, "flex-grow", Some(format!("{}", child.grow)));
                set_inline(world, pane, "flex-shrink", Some("1".into()));
                if on_path {
                    let keep = world
                        .get::<DockStack>(pane)
                        .map(|s| pass.max_path.contains(&s.id))
                        .unwrap_or(false)
                        || built
                            .nodes
                            .iter()
                            .any(|(id, &n)| n == pane && pass.max_path.contains(id));
                    if keep {
                        pass.max_held.push(pane);
                    } else {
                        pass.max_hidden.push(pane);
                    }
                }
            }
            let wanted: Vec<Entity> = panes.iter().map(|(e, _)| *e).collect();
            let current: Vec<Entity> = children_of(world, e)
                .into_iter()
                .filter(|&k| world.get::<SplitGutter>(k).is_none())
                .collect();
            if current != wanted {
                for k in children_of(world, e) {
                    if world.get::<SplitGutter>(k).is_some() {
                        world.entity_mut(k).despawn();
                    } else if !wanted.contains(&k) {
                        // A pane leaving the split: whoever wants it takes it
                        // later in this pass; the rest is stale structure.
                        world.entity_mut(k).remove::<ChildOf>();
                    }
                }
                reorder(world, e, &wanted);
            }
            if on_path {
                for k in children_of(world, e) {
                    if world.get::<SplitGutter>(k).is_some() {
                        pass.max_hidden.push(k);
                    }
                }
            }
            Some(e)
        }
    }
}

fn build_stack(
    world: &mut World,
    built: &mut Built,
    pass: &mut Pass<'_>,
    stack: &Stack,
    is_root: bool,
) -> Option<Entity> {
    let live: Vec<&PanelId> = stack
        .panels
        .iter()
        .filter(|p| pass.live.contains_key(*p))
        .collect();
    if live.is_empty() && !stack.center && !is_root {
        return None;
    }
    pass.used_nodes.insert(stack.id.clone());
    let e = match built
        .nodes
        .get(&stack.id)
        .copied()
        .filter(|&e| alive(world, e))
    {
        Some(e) => e,
        None => {
            let e = spawn_chrome(world, "tabs", &["dock-stack"], None);
            world.entity_mut(e).insert(DockStack {
                id: stack.id.clone(),
            });
            built.nodes.insert(stack.id.clone(), e);
            e
        }
    };
    let maximized = pass.max_path.last() == Some(&stack.id);
    let mut classes: Vec<&str> = Vec::new();
    if stack.center {
        classes.push("center");
    }
    if stack.collapsed {
        classes.push("collapsed");
    }
    if live.len() == 1 {
        classes.push("single");
    }
    if stack.tabs == TabsMode::Never || (stack.tabs == TabsMode::Auto && live.is_empty()) {
        classes.push("no-tabs");
    }
    if maximized {
        classes.push("maximized");
    }
    set_classes(world, e, &classes, &STACK_CLASSES);
    // A collapsed stack keeps its header and gives up its share.
    set_inline(
        world,
        e,
        "flex-grow",
        stack.collapsed.then(|| "0".to_string()),
    );
    let mut tabs = Vec::new();
    let mut min = (0.0f32, 0.0f32);
    for p in &live {
        let panel = &pass.live[*p];
        if let Some((w, h)) = panel.min {
            min = (min.0.max(w), min.1.max(h));
        }
        pass.used_tabs.insert((*p).clone());
        let tab = match built.tabs.get(*p).copied().filter(|&t| alive(world, t)) {
            Some(t) => t,
            None => {
                // The strip button's own caption is a blank: the title shows
                // in a caption element inside the button, beside the icon and
                // the close button, since an element with children lays out
                // by its children and not by its own text.
                let el = Element {
                    tag: "tab".into(),
                    attrs: Box::new(Attributes {
                        classes: vec!["dock-tab-body".into()],
                        tab: Some(TabSpec::new((*p).clone(), Some(BLANK.to_string()))),
                        ..Attributes::default()
                    }),
                    ..Element::default()
                };
                let t = spawn_subtree(world, &el, None, Placeholders::Resolved);
                world.entity_mut(t).insert((
                    DockChrome,
                    lumen_module::lumen_core::components::DomHidden,
                    StyleDirty,
                    DockTab {
                        panel: (*p).clone(),
                    },
                ));
                built.tabs.insert((*p).clone(), t);
                t
            }
        };
        put_under(world, panel.entity, tab);
        unhide(world, panel.entity);
        tabs.push(tab);
    }
    let width = (min.0 > 0.0).then(|| format!("{}px", min.0));
    let height = (min.1 > 0.0).then(|| format!("{}px", min.1));
    set_inline(world, e, "min-width", width);
    set_inline(world, e, "min-height", height);
    let strip: Vec<Entity> = children_of(world, e)
        .into_iter()
        .filter(|&k| world.get::<TabStrip>(k).is_some())
        .collect();
    let wanted: Vec<Entity> = strip.iter().copied().chain(tabs.iter().copied()).collect();
    let current = children_of(world, e);
    if current != wanted {
        for k in current {
            if !wanted.contains(&k) {
                world.entity_mut(k).remove::<ChildOf>();
            }
        }
        reorder(world, e, &wanted);
    }
    let active = stack
        .active
        .as_ref()
        .filter(|a| pass.live.contains_key(*a))
        .or(live.first().copied());
    if let Some(active) = active {
        let mut ent = world.entity_mut(e);
        let mut attrs = ent.take::<LumenAttributes>().unwrap_or_default();
        if attrs.get("value") != Some(active.as_str()) {
            attrs.set("value", active.clone());
        }
        ent.insert(attrs);
    }
    if let Some(label) = active
        .and_then(|a| pass.live.get(a))
        .map(|p| p.title.clone())
    {
        world.entity_mut(e).insert(A11yLabel(label));
    }
    Some(e)
}

/// The path of node ids from `node` to the stack holding `panel`.
fn path_to(node: &Node, panel: &str) -> Option<Vec<String>> {
    match node {
        Node::Stack(s) => s
            .panels
            .iter()
            .any(|p| p == panel)
            .then(|| vec![s.id.clone()]),
        Node::Split(split) => split.children.iter().find_map(|c| {
            path_to(&c.node, panel).map(|mut rest| {
                rest.insert(0, split.id.clone());
                rest
            })
        }),
    }
}

/// The monitors as the placement code reads them.
fn monitors(world: &World) -> Vec<lumen_dock_model::Monitor> {
    world
        .get_resource::<Monitors>()
        .map(|m| {
            m.0.iter()
                .map(|m| lumen_dock_model::Monitor {
                    name: m.name.clone(),
                    origin: [m.position.x, m.position.y],
                    size: [m.size.x, m.size.y],
                    work: (
                        [m.work_area.0.x, m.work_area.0.y],
                        [m.work_area.1.x, m.work_area.1.y],
                    ),
                    scale: m.scale,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn primary(world: &mut World) -> Option<Entity> {
    world
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .iter(world)
        .next()
}

/// Open the platform window for `layout`, or keep the one it has.
fn ensure_window(
    world: &mut World,
    built: &mut Built,
    core: &Core,
    layout: &WindowLayout,
    live: &BTreeMap<PanelId, Live>,
) -> Option<(Entity, Entity)> {
    let panels: Vec<&PanelId> = layout
        .root
        .panels()
        .into_iter()
        .filter(|p| live.contains_key(*p))
        .collect();
    let first = *panels.first()?;
    let active = layout
        .root
        .stacks()
        .first()
        .and_then(|s| s.active.as_ref())
        .filter(|a| live.contains_key(*a))
        .unwrap_or(first);
    let rec = &core.panels[active];
    let title = live[active].os_title.clone();
    if let Some(&(window, surface)) = built.windows.get(&layout.id)
        && alive(world, window)
        && alive(world, surface)
    {
        if let Some(mut w) = world.get_mut::<Window>(window) {
            if w.title != title {
                w.title = title;
            }
            // A restored layout puts a window it keeps back the way it was.
            let state = (layout.place.maximized, layout.place.fullscreen);
            if (w.maximized, w.fullscreen) != state {
                (w.maximized, w.fullscreen) = state;
            }
        }
        return Some((window, surface));
    }
    let primary = primary(world);
    let single = panels.len() == 1;
    let attrs = &rec.attrs;
    // The size: where it was, else what the panel asks for, else the size
    // the panel has where it is now.
    let here = world.get::<Transform>(live[active].entity).cloned();
    let fallback = attrs
        .window_size
        .or_else(|| {
            here.as_ref()
                .filter(|t| t.size.x > 1.0 && t.size.y > 1.0)
                .map(|t| [t.size.x, t.size.y])
        })
        .unwrap_or([640.0, 480.0]);
    let monitors = monitors(world);
    let mut placed = lumen_dock_model::place(
        layout.place.rect,
        layout.place.monitor.as_ref(),
        &monitors,
        fallback,
    );
    // A window with no saved place opens over the panel's place on screen.
    if layout.place.rect.is_none()
        && let (Some(t), Some(p)) = (&here, primary)
        && let Some(origin) = world.get::<WindowRuntime>(p).and_then(|r| r.position)
    {
        let scale = world
            .get::<lumen_module::lumen_core::window::WindowViewport>(p)
            .map(|v| v.scale_factor)
            .unwrap_or(1.0);
        placed.position = Some([
            origin.x + (t.absolute.x * scale) as i32,
            origin.y + (t.absolute.y * scale) as i32,
        ]);
    }
    let kind = if single {
        attrs.window_kind
    } else {
        WindowKind::Normal
    };
    let window = world
        .spawn((
            Window {
                title,
                size: Vec2::new(placed.size[0], placed.size[1]),
                kind,
                visible: true,
                resizable: !single || attrs.resizable,
                decorations: true,
                position: placed.position.map(|p| IVec2::new(p[0], p[1])),
                min_size: None,
                max_size: None,
                owner: (kind != WindowKind::Normal).then_some(primary).flatten(),
                maximized: layout.place.maximized,
                fullscreen: layout.place.fullscreen,
            },
            WindowName(first.clone()),
            DockWindow {
                id: layout.id.clone(),
            },
        ))
        .id();
    // The window's content sits under the dock its panels came from, or
    // where a free panel was declared.
    let parent = layout
        .home
        .as_ref()
        .and_then(|d| core.docks.get(d))
        .map(|d| d.entity)
        .filter(|&e| alive(world, e))
        .or_else(|| core.panels[first].declared_in.filter(|&e| alive(world, e)));
    let kind_class = match kind {
        WindowKind::Normal => "dock-window-normal",
        WindowKind::Dialog => "dock-window-dialog",
        WindowKind::Tool => "dock-window-tool",
    };
    let surface = spawn_chrome(world, "column", &["dock-window", kind_class], parent);
    world.entity_mut(surface).insert(WindowOf(window));
    built.windows.insert(layout.id.clone(), (window, surface));
    Some((window, surface))
}

/// Make the elements follow the model, when it changed.
pub fn realize(world: &mut World) {
    let shared = world.resource::<Shared>().clone();
    let core = shared.lock();
    if !core.dirty {
        return;
    }
    drop(core);
    let mut built = world.remove_resource::<Built>().unwrap_or_default();
    let mut core = shared.lock();
    core.dirty = false;
    core.report_due = true;
    let ws = core.ws.clone();
    let live = live_panels(world, &core);
    let mut pass = Pass {
        live: &live,
        used_nodes: HashSet::new(),
        used_tabs: HashSet::new(),
        max_path: Vec::new(),
        max_hidden: Vec::new(),
        max_held: Vec::new(),
    };
    let mut shown: HashSet<PanelId> = HashSet::new();
    // Every dock.
    for (dock_id, area) in &ws.docks {
        let Some(dock) = core
            .docks
            .get(dock_id)
            .map(|d| d.entity)
            .filter(|&e| alive(world, e))
        else {
            continue;
        };
        pass.max_path = area
            .maximized
            .as_ref()
            .filter(|m| live.contains_key(*m))
            .and_then(|m| path_to(&area.root, m))
            .unwrap_or_default();
        let Some(root) = build(world, &mut built, &mut pass, &area.root, true) else {
            continue;
        };
        put_under(world, root, dock);
        let first = children_of(world, dock).first().copied();
        if first != Some(root) {
            world.entity_mut(dock).insert_children(0, &[root]);
        }
        set_classes(
            world,
            dock,
            if pass.max_path.is_empty() {
                &[]
            } else {
                &["dock-maximized"]
            },
            &["dock-maximized"],
        );
        shown.extend(area.root.panels().into_iter().cloned());
    }
    // Every window with something to show.
    let mut used_windows: HashSet<String> = HashSet::new();
    let mut modal: Vec<(Entity, Modal)> = Vec::new();
    pass.max_path.clear();
    for layout in &ws.windows {
        let Some((window, surface)) = ensure_window(world, &mut built, &core, layout, &live) else {
            continue;
        };
        used_windows.insert(layout.id.clone());
        // The tree goes first; a tab menu open in the window stays after it.
        if let Some(root) = build(world, &mut built, &mut pass, &layout.root, true)
            && children_of(world, surface).first() != Some(&root)
        {
            put_under(world, root, surface);
            world.entity_mut(surface).insert_children(0, &[root]);
            world.entity_mut(surface).insert(DirtyLayout);
        }
        for p in layout.root.panels() {
            shown.insert(p.clone());
            if let Some(rec) = core.panels.get(p)
                && rec.attrs.modal != Modal::None
                && live.contains_key(p)
            {
                modal.push((window, rec.attrs.modal));
            }
        }
    }
    // Hidden panels wait in their dock's store; a free panel waits where it
    // was declared, hidden.
    for (id, panel) in &live {
        if shown.contains(id) {
            continue;
        }
        let rec = &core.panels[id];
        let store = rec
            .info
            .dock
            .as_ref()
            .and_then(|d| core.docks.get(d))
            .map(|d| d.entity)
            .filter(|&e| alive(world, e))
            .map(|dock| {
                store_of(
                    world,
                    &mut built,
                    dock,
                    rec.info.dock.as_deref().unwrap_or_default(),
                )
            });
        match store {
            Some(store) => put_under(world, panel.entity, store),
            None => {
                if let Some(parent) = rec.declared_in.filter(|&e| alive(world, e)) {
                    put_under(world, panel.entity, parent);
                }
                hide(world, panel.entity);
            }
        }
    }
    // Maximize: the stack fills the dock, the rest is hidden in place.
    let now_hidden: HashSet<Entity> = std::mem::take(&mut pass.max_hidden).into_iter().collect();
    for &e in built.max_hidden.difference(&now_hidden) {
        unhide(world, e);
    }
    for &e in &now_hidden {
        hide(world, e);
    }
    built.max_hidden = now_hidden;
    let held: Vec<Entity> = std::mem::take(&mut pass.max_held);
    let mut q = world.query_filtered::<Entity, With<DockMaximized>>();
    let previously: Vec<Entity> = q.iter(world).collect();
    for e in previously {
        if !held.contains(&e)
            && let Ok(mut ent) = world.get_entity_mut(e)
        {
            release_on::<DockMaximized>(&mut ent);
            ent.remove::<DockMaximized>();
        }
    }
    for e in &held {
        if let Ok(mut ent) = world.get_entity_mut(*e) {
            ent.insert(DockMaximized);
            hold_on::<DockMaximized>(
                &mut ent,
                StyleValues {
                    basis: Some(Length::Percent(100.0)),
                    ..StyleValues::default()
                },
            );
        }
    }
    // Structure no longer in the model goes; what it held has moved out.
    let used_nodes = std::mem::take(&mut pass.used_nodes);
    let used_tabs = std::mem::take(&mut pass.used_tabs);
    let stale_tabs: Vec<PanelId> = built
        .tabs
        .keys()
        .filter(|p| !used_tabs.contains(*p))
        .cloned()
        .collect();
    for p in stale_tabs {
        if let Some(t) = built.tabs.remove(&p)
            && let Ok(e) = world.get_entity_mut(t)
        {
            e.despawn();
        }
    }
    let stale_nodes: Vec<String> = built
        .nodes
        .keys()
        .filter(|n| !used_nodes.contains(*n))
        .cloned()
        .collect();
    for n in stale_nodes {
        if let Some(e) = built.nodes.remove(&n)
            && let Ok(e) = world.get_entity_mut(e)
        {
            e.despawn();
        }
    }
    let stale_windows: Vec<String> = built
        .windows
        .keys()
        .filter(|w| !used_windows.contains(*w))
        .cloned()
        .collect();
    for w in stale_windows {
        if let Some((window, _)) = built.windows.remove(&w)
            && let Ok(e) = world.get_entity_mut(window)
        {
            e.despawn();
        }
    }
    // Modality: a modal window blocks the primary, or every other window.
    let primary = primary(world);
    let mut wanted: HashMap<Entity, Entity> = HashMap::new();
    for (window, kind) in modal {
        match kind {
            Modal::Parent => {
                if let Some(p) = primary {
                    wanted.insert(p, window);
                }
            }
            Modal::App => {
                let mut q = world.query_filtered::<Entity, With<Window>>();
                for other in q.iter(world).collect::<Vec<_>>() {
                    if other != window {
                        wanted.insert(other, window);
                    }
                }
            }
            Modal::None => {}
        }
    }
    for e in built.blocked.drain().collect::<Vec<_>>() {
        if !wanted.contains_key(&e)
            && let Ok(mut ent) = world.get_entity_mut(e)
        {
            ent.remove::<InputBlocked>();
        }
    }
    for (e, by) in wanted {
        if let Ok(mut ent) = world.get_entity_mut(e) {
            ent.insert(InputBlocked { by });
            built.blocked.insert(e);
        }
    }
    drop(core);
    world.insert_resource(built);
}

/// The store of `dock`, built on first use.
fn store_of(world: &mut World, built: &mut Built, dock: Entity, dock_id: &str) -> Entity {
    if let Some(&s) = built.stores.get(dock_id)
        && alive(world, s)
    {
        return s;
    }
    let existing = children_of(world, dock).into_iter().find(|&k| {
        world
            .get::<LumenClasses>(k)
            .is_some_and(|c| c.has("dock-store"))
            && world.get::<DockChrome>(k).is_some()
    });
    let store = existing.unwrap_or_else(|| {
        let s = spawn_chrome(world, "column", &["dock-store"], Some(dock));
        hide(world, s);
        s
    });
    built.stores.insert(dock_id.to_string(), store);
    store
}
