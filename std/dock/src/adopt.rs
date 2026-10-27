//! Reading what the page declares: each `<dock>` with its splits, stacks,
//! panels and named layouts, the `<dock-panel>`s outside any dock, and the
//! panels an app adds or takes away at run time.
//!
//! A `<dock>` is read once, when it arrives. Its `<dock-split>`,
//! `<dock-stack>` and `<dock-layout>` children describe the default layout
//! and are consumed: they leave the tree, and the module builds the realized
//! structure out of the core `<split>` and `<tabs>` in their place. Its
//! `<dock-panel>`s are kept, and moved wherever the layout puts them.

use std::collections::BTreeSet;

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use lumen_dock_model::{
    Axis, Child, DockArea, Node, PanelId, Rect, Split, Stack, TabsMode, WindowLayout, WindowPlace,
    Workspace,
};
use lumen_module::lumen_core::components::Display;
use lumen_module::lumen_core::components::{
    A11yLabel, A11yRole, DomHidden, LumenAttributes, LumenClasses, LumenId, LumenTag, StyleDirty,
};
use lumen_module::lumen_core::style_overrides::{StyleOverrides, StyleOwner, StyleValues, hold_on};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::state::{DockRec, PanelAttrs, PanelRec, Reason, Shared, attr, flag, panel_info};

/// The `<dock>` element.
pub const DOCK: &str = "dock";
/// A split in a declared layout.
pub const SPLIT: &str = "dock-split";
/// A tab stack in a declared layout.
pub const STACK: &str = "dock-stack";
/// A panel.
pub const PANEL: &str = "dock-panel";
/// A named layout.
pub const LAYOUT: &str = "dock-layout";
/// A panel's place in a named layout.
pub const REF: &str = "dock-ref";
/// A window in a named layout.
pub const WINDOW: &str = "dock-window";

/// Every element the module answers for.
pub const TAGS: [&str; 7] = [DOCK, SPLIT, STACK, PANEL, LAYOUT, REF, WINDOW];

/// On each `<dock-panel>` the module adopted: the panel's id.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockPanel {
    /// The panel's id, which is its element id.
    pub id: String,
}

/// On each `<dock>` the module adopted: the dock's id.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockElement {
    /// The dock's id, which is its element id.
    pub id: String,
}

/// On every element the module built itself: the stacks, splits, tab
/// wrappers, close buttons, the store hidden panels wait in, and each
/// window's root.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DockChrome;

/// The module hides an element: a hidden free panel, the store, the parts a
/// maximized panel covers.
#[derive(Component, Clone, Copy, Debug, Default)]
#[require(StyleOverrides)]
pub struct DockHides;

impl StyleOwner for DockHides {
    const RANK: u8 = 45;
}

/// Elements that arrived or left since the module last looked.
#[derive(Resource, Default)]
pub struct Arrivals {
    /// Elements with one of the module's tags that arrived.
    pub new: Vec<Entity>,
    /// Panels that left: the entity and the panel id.
    pub gone: Vec<(Entity, String)>,
    /// Panels created but not yet in the tree.
    pub waiting: Vec<Entity>,
}

/// Whether there is anything to adopt.
pub fn arrivals_pending(arrivals: Res<Arrivals>) -> bool {
    !arrivals.new.is_empty() || !arrivals.gone.is_empty() || !arrivals.waiting.is_empty()
}

/// Watch for the module's elements arriving and panels leaving. Costs
/// nothing on a tick that spawns none of them.
pub fn install_observers(world: &mut World) {
    world.add_observer(
        |ev: On<Add, LumenTag>, tags: Query<&LumenTag>, mut arrivals: ResMut<Arrivals>| {
            if tags
                .get(ev.entity)
                .is_ok_and(|t| matches!(&*t.0, DOCK | PANEL))
            {
                arrivals.new.push(ev.entity);
            }
        },
    );
    world.add_observer(
        |ev: On<Remove, DockPanel>, panels: Query<&DockPanel>, mut arrivals: ResMut<Arrivals>| {
            if let Ok(p) = panels.get(ev.entity) {
                arrivals.gone.push((ev.entity, p.id.clone()));
            }
        },
    );
}

fn tag_of(world: &World, e: Entity) -> Option<String> {
    world.get::<LumenTag>(e).map(|t| t.0.to_string())
}

fn children_of(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.to_vec())
        .unwrap_or_default()
}

fn id_of(world: &World, e: Entity) -> Option<String> {
    world
        .get::<LumenId>(e)
        .map(|i| i.0.clone())
        .filter(|i| !i.is_empty())
}

/// Spawn one element of the module's own structure.
pub fn spawn_chrome(
    world: &mut World,
    tag: &str,
    classes: &[&str],
    parent: Option<Entity>,
) -> Entity {
    let el = Element {
        tag: tag.to_string(),
        attrs: Box::new(Attributes {
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Attributes::default()
        }),
        ..Element::default()
    };
    let e = spawn_subtree(world, &el, parent, Placeholders::Resolved);
    world
        .entity_mut(e)
        .insert((DockChrome, DomHidden, StyleDirty));
    e
}

/// Hide an element the module keeps out of view.
pub fn hide(world: &mut World, e: Entity) {
    if let Ok(mut ent) = world.get_entity_mut(e) {
        ent.insert(DockHides);
        hold_on::<DockHides>(
            &mut ent,
            StyleValues {
                display: Some(Display::None),
                ..StyleValues::default()
            },
        );
    }
}

/// Stop hiding an element.
pub fn unhide(world: &mut World, e: Entity) {
    if let Ok(mut ent) = world.get_entity_mut(e)
        && ent.contains::<DockHides>()
    {
        lumen_module::lumen_core::style_overrides::release_on::<DockHides>(&mut ent);
        ent.remove::<DockHides>();
    }
}

/// What reading one declared tree collects.
struct Reader<'a> {
    dock: &'a str,
    seq: u32,
    /// Panels in the tree: id, element, and whether it starts hidden.
    panels: Vec<(String, Entity, bool)>,
    /// Panels written hidden: id, the stack they were written in, and their
    /// index there.
    hidden_at: Vec<(String, String, usize)>,
    /// Read `<dock-ref panel>` instead of `<dock-panel>`.
    refs: bool,
}

impl Reader<'_> {
    fn next_id(&mut self, kind: char) -> String {
        self.seq += 1;
        format!("{}/{kind}{}", self.dock, self.seq)
    }

    /// One `<dock-split>` or `<dock-stack>` as a child of a split, or
    /// `None` when it holds nothing.
    fn child(&mut self, world: &World, e: Entity) -> Option<Child> {
        let attrs = world.get::<LumenAttributes>(e);
        let node = match tag_of(world, e).as_deref() {
            Some(SPLIT) => self.split(world, e)?,
            Some(STACK) => self.stack(world, e)?,
            Some(PANEL) if !self.refs => {
                let (p, hidden) = self.panel(world, e)?;
                let id = self.next_id('s');
                if hidden {
                    self.hidden_at.push((p, id, 0));
                    return None;
                }
                Node::Stack(Stack::with(id, [p]))
            }
            _ => return None,
        };
        let basis = attr(attrs, "basis").and_then(|b| b.trim_end_matches("px").parse::<f32>().ok());
        let center = matches!(&node, Node::Stack(s) if s.center);
        let grow = attr(attrs, "grow")
            .and_then(|g| g.parse::<f32>().ok())
            .unwrap_or(if basis.is_none() || center { 1.0 } else { 0.0 });
        Some(Child { basis, grow, node })
    }

    fn split(&mut self, world: &World, e: Entity) -> Option<Node> {
        let attrs = world.get::<LumenAttributes>(e);
        let axis = match attr(attrs, "axis") {
            Some("column") => Axis::Column,
            _ => Axis::Row,
        };
        let id = id_of(world, e).unwrap_or_else(|| self.next_id('x'));
        let mut children: Vec<Child> = children_of(world, e)
            .into_iter()
            .filter_map(|k| self.child(world, k))
            .collect();
        match children.len() {
            0 => None,
            1 => Some(children.pop().expect("one").node),
            _ => Some(Node::Split(Split { axis, id, children })),
        }
    }

    fn stack(&mut self, world: &World, e: Entity) -> Option<Node> {
        let attrs = world.get::<LumenAttributes>(e);
        let id = id_of(world, e).unwrap_or_else(|| self.next_id('s'));
        let mut stack = Stack::new(id);
        stack.center = flag(attrs, "center", false);
        stack.collapsed = flag(attrs, "collapsed", false);
        stack.tabs = attr(attrs, "tabs")
            .and_then(TabsMode::parse)
            .unwrap_or_default();
        for k in children_of(world, e) {
            let panel = match tag_of(world, k).as_deref() {
                Some(REF) if self.refs => {
                    attr(world.get::<LumenAttributes>(k), "panel").map(|p| (p.to_string(), false))
                }
                Some(PANEL) if !self.refs => self.panel(world, k),
                _ => None,
            };
            match panel {
                Some((p, true)) => self
                    .hidden_at
                    .push((p, stack.id.clone(), stack.panels.len())),
                Some((p, false)) if !stack.panels.contains(&p) => stack.panels.push(p),
                _ => {}
            }
        }
        let active = attr(attrs, "active").map(str::to_string);
        stack.active = active
            .filter(|a| stack.panels.contains(a))
            .or_else(|| stack.panels.first().cloned());
        (stack.center || !stack.panels.is_empty()).then_some(Node::Stack(stack))
    }

    /// A `<dock-panel>` in the tree: its id and whether it starts hidden.
    fn panel(&mut self, world: &World, e: Entity) -> Option<(String, bool)> {
        let Some(id) = id_of(world, e) else {
            lumen_module::lumen_core::warn_line!(
                "lumen-dock: a <dock-panel> in dock '{}' has no id and is left out",
                self.dock
            );
            return None;
        };
        let hidden = flag(world.get::<LumenAttributes>(e), "hidden", false);
        self.panels.push((id.clone(), e, hidden));
        Some((id, hidden))
    }
}

/// The root a dock's declared children make: their one tree, a row of
/// them, or an empty center stack.
fn root_of(reader: &mut Reader<'_>, world: &World, kids: &[Entity]) -> Node {
    let mut roots: Vec<Child> = kids
        .iter()
        .filter(|&&k| matches!(tag_of(world, k).as_deref(), Some(SPLIT | STACK)))
        .filter_map(|&k| reader.child(world, k))
        .collect();
    match roots.len() {
        0 => {
            let mut center = Stack::new(format!("{}/center", reader.dock));
            center.center = true;
            Node::Stack(center)
        }
        1 => roots.pop().expect("one").node,
        _ => Node::Split(Split {
            axis: Axis::Row,
            id: reader.next_id('x'),
            children: roots,
        }),
    }
}

/// Read a `<dock-layout>`'s tree and windows into `into`, for dock `dock`.
fn read_layout(world: &World, dock: &str, layout: Entity, into: &mut Workspace) {
    let mut reader = Reader {
        dock,
        seq: 1000,
        panels: Vec::new(),
        hidden_at: Vec::new(),
        refs: true,
    };
    let kids = children_of(world, layout);
    let root = root_of(&mut reader, world, &kids);
    into.docks.insert(dock.to_string(), DockArea::new(root));
    for k in kids {
        if tag_of(world, k).as_deref() != Some(WINDOW) {
            continue;
        }
        let rect = attr(world.get::<LumenAttributes>(k), "rect").and_then(|r| {
            let v: Vec<f32> = r
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|t| !t.is_empty())
                .filter_map(|t| t.parse().ok())
                .collect();
            (v.len() == 4).then(|| Rect {
                x: v[0],
                y: v[1],
                w: v[2],
                h: v[3],
            })
        });
        let inner = children_of(world, k);
        let root = root_of(&mut reader, world, &inner);
        if root.panels().is_empty() {
            continue;
        }
        let id = format!("w{}", into.windows.len() + 1);
        into.windows.push(WindowLayout {
            id,
            home: Some(dock.to_string()),
            place: WindowPlace {
                rect,
                ..WindowPlace::default()
            },
            root,
        });
    }
}

/// Register one panel element, marking it as adopted.
fn register_panel(
    world: &mut World,
    shared: &Shared,
    id: &str,
    e: Entity,
    dock: Option<&str>,
    declared_in: Option<Entity>,
) -> bool {
    let attrs = world.get::<LumenAttributes>(e).cloned();
    let mut core = shared.lock();
    if let Some(existing) = core.panels.get(id)
        && let Some(other) = existing.entity
        && other != e
        && world.get_entity(other).is_ok()
    {
        lumen_module::lumen_core::warn_line!(
            "lumen-dock: two <dock-panel>s have the id '{id}'; the second is left out"
        );
        return false;
    }
    let rec = PanelRec {
        entity: Some(e),
        info: panel_info(attrs.as_ref(), dock.map(str::to_string)),
        attrs: PanelAttrs::read(attrs.as_ref(), id),
        declared_in,
    };
    if let (Some(kind), Some(state)) = (&rec.attrs.kind, &rec.attrs.state) {
        core.ws.panels.insert(
            id.to_string(),
            lumen_dock_model::PanelMeta {
                kind: kind.clone(),
                state: state.clone(),
            },
        );
    }
    core.reported.remove(id);
    core.panels.insert(id.to_string(), rec);
    drop(core);
    world.entity_mut(e).insert(DockPanel { id: id.to_string() });
    true
}

/// Read one `<dock>`.
fn adopt_dock(world: &mut World, shared: &Shared, dock: Entity) {
    if world.get::<DockElement>(dock).is_some() {
        return;
    }
    let Some(id) = id_of(world, dock) else {
        lumen_module::lumen_core::warn_line!(
            "lumen-dock: a <dock> needs an id; this one is left as it is"
        );
        return;
    };
    let attrs = world.get::<LumenAttributes>(dock).cloned();
    let kids = children_of(world, dock);
    let mut reader = Reader {
        dock: &id,
        seq: 0,
        panels: Vec::new(),
        hidden_at: Vec::new(),
        refs: false,
    };
    let root = root_of(&mut reader, world, &kids);
    let in_tree = std::mem::take(&mut reader.panels);
    let hidden_at = std::mem::take(&mut reader.hidden_at);
    // Panels written straight under the dock wait in its catalogue, hidden
    // until shown.
    let catalogue: Vec<(String, Entity)> = kids
        .iter()
        .filter(|&&k| tag_of(world, k).as_deref() == Some(PANEL))
        .filter_map(|&k| id_of(world, k).map(|i| (i, k)))
        .collect();
    let mut builtin: Vec<(String, Workspace)> = Vec::new();
    for &k in &kids {
        if tag_of(world, k).as_deref() == Some(LAYOUT) {
            let name = attr(world.get::<LumenAttributes>(k), "name")
                .unwrap_or("Layout")
                .to_string();
            let mut ws = Workspace {
                name: name.clone(),
                ..Workspace::default()
            };
            read_layout(world, &id, k, &mut ws);
            builtin.push((name, ws));
        }
    }

    // The store hidden panels of this dock wait in.
    let store = spawn_chrome(world, "column", &["dock-store"], Some(dock));
    hide(world, store);
    for (pid, e, _) in &in_tree {
        if register_panel(world, shared, pid, *e, Some(&id), Some(dock)) {
            world.entity_mut(*e).insert(ChildOf(store));
        }
    }
    for (pid, e) in &catalogue {
        if register_panel(world, shared, pid, *e, Some(&id), Some(dock)) {
            world.entity_mut(*e).insert(ChildOf(store));
        }
    }
    // The declaration is consumed: what is left of it after the panels
    // moved out goes.
    for k in kids {
        if matches!(
            tag_of(world, k).as_deref(),
            Some(SPLIT | STACK | LAYOUT | REF | WINDOW)
        ) && let Ok(e) = world.get_entity_mut(k)
        {
            e.despawn();
        }
    }

    {
        let mut e = world.entity_mut(dock);
        let mut classes = e.take::<LumenClasses>().unwrap_or_default();
        if !classes.has("dock") {
            classes.0.push("dock".into());
        }
        e.insert((
            classes,
            StyleDirty,
            DockElement { id: id.clone() },
            A11yRole::Group,
        ));
        if let Some(label) = attr(attrs.as_ref(), "label") {
            e.insert(A11yLabel(label.to_string()));
        }
    }

    let mut core = shared.lock();
    let mut area = Workspace::default();
    area.docks.insert(id.clone(), DockArea::new(root));
    // A panel written hidden in a stack starts hidden, with its place in
    // that stack remembered.
    for (p, stack, index) in hidden_at {
        area.homes.insert(
            p,
            lumen_dock_model::Home {
                dock: Some(lumen_dock_model::Spot {
                    dock: id.clone(),
                    stack,
                    index,
                    split: None,
                    slot: 0,
                }),
                window: None,
            },
        );
    }
    let area_dock = area.docks.remove(&id).expect("inserted");
    core.default.docks.insert(id.clone(), area_dock.clone());
    for (p, h) in &area.homes {
        core.default.homes.insert(p.clone(), h.clone());
    }
    core.docks.insert(
        id.clone(),
        DockRec {
            entity: dock,
            label: attr(attrs.as_ref(), "label").map(str::to_string),
        },
    );
    for (name, ws) in builtin {
        let entry = core
            .builtin
            .entry(name.clone())
            .or_insert_with(|| Workspace {
                name,
                ..Workspace::default()
            });
        entry.docks.extend(ws.docks);
        entry.windows.extend(ws.windows);
    }
    if core.ws.docks.contains_key(&id) {
        // The dock came back (a page returned, the markup reloaded): its
        // arrangement stays, fitted to the panels it has now.
        let default = core.default.clone();
        let live: BTreeSet<PanelId> = core
            .panels
            .iter()
            .filter(|(_, p)| p.entity.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        core.ws.reconcile(&default, &live);
    } else {
        core.ws.docks.insert(id.clone(), area_dock);
        for (p, h) in area.homes {
            core.ws.homes.entry(p).or_insert(h);
        }
    }
    core.dirty = true;
}

/// The `<dock>` above `e`, if any; `Err` when `e` is not in the tree.
fn dock_above(world: &World, e: Entity) -> Result<Option<Entity>, ()> {
    let mut cur = world.get::<ChildOf>(e).map(ChildOf::parent).ok_or(())?;
    loop {
        if world.get::<DockElement>(cur).is_some() || tag_of(world, cur).as_deref() == Some(DOCK) {
            return Ok(Some(cur));
        }
        match world.get::<ChildOf>(cur) {
            Some(c) => cur = c.parent(),
            None => return Ok(None),
        }
    }
}

/// Adopt what arrived and forget what left: the exclusive system behind
/// every `<dock>` and `<dock-panel>` the page or the app adds.
pub fn adopt_elements(world: &mut World) {
    let shared = world.resource::<Shared>().clone();
    let (new, gone, waiting) = {
        let mut arrivals = world.resource_mut::<Arrivals>();
        (
            std::mem::take(&mut arrivals.new),
            std::mem::take(&mut arrivals.gone),
            std::mem::take(&mut arrivals.waiting),
        )
    };
    for (e, id) in gone {
        let mut core = shared.lock();
        if let Some(rec) = core.panels.get_mut(&id)
            && rec.entity == Some(e)
        {
            rec.entity = None;
            let destroy = rec.attrs.destroy_on_close;
            if destroy {
                let effects = core.ws.forget(&id);
                core.panels.remove(&id);
                core.apply(effects, Reason::Api);
            }
            core.dirty = true;
        }
    }
    let alive = |world: &World, e: Entity| world.get_entity(e).is_ok();
    let mut docks: Vec<Entity> = new
        .iter()
        .copied()
        .filter(|&e| alive(world, e) && tag_of(world, e).as_deref() == Some(DOCK))
        .collect();
    docks.sort();
    for dock in docks {
        if dock_above(world, dock).ok().flatten().is_some() {
            lumen_module::lumen_core::warn_line!(
                "lumen-dock: a <dock> inside another dock is not supported; it is left as plain content"
            );
            continue;
        }
        adopt_dock(world, &shared, dock);
    }
    let mut panels: Vec<Entity> = new
        .into_iter()
        .chain(waiting)
        .filter(|&e| alive(world, e) && tag_of(world, e).as_deref() == Some(PANEL))
        .filter(|&e| world.get::<DockPanel>(e).is_none())
        .collect();
    panels.sort();
    panels.dedup();
    for e in panels {
        let Some(id) = id_of(world, e) else {
            lumen_module::lumen_core::warn_line!(
                "lumen-dock: a <dock-panel> needs an id; this one is left as it is"
            );
            continue;
        };
        match dock_above(world, e) {
            Err(()) => world.resource_mut::<Arrivals>().waiting.push(e),
            Ok(Some(dock)) => {
                let Some(dock_id) = world.get::<DockElement>(dock).map(|d| d.id.clone()) else {
                    continue;
                };
                // A panel inside another panel's content is content.
                let parent = world.get::<ChildOf>(e).map(ChildOf::parent);
                if parent != Some(dock) {
                    continue;
                }
                if !register_panel(world, &shared, &id, e, Some(&dock_id), Some(dock)) {
                    continue;
                }
                let mut core = shared.lock();
                let (hidden, shown) = {
                    let rec = &core.panels[&id];
                    (rec.attrs.hidden, core.ws.is_shown(&id))
                };
                if !hidden && !shown && !core.ws.homes.contains_key(&id) {
                    let info = core.info(&id);
                    if let Ok(effects) = core.ws.show(&id, &info) {
                        core.apply(effects, Reason::Api);
                    }
                }
                core.dirty = true;
            }
            Ok(None) => {
                let parent = world.get::<ChildOf>(e).map(ChildOf::parent);
                if register_panel(world, &shared, &id, e, None, parent) {
                    let shown = shared.lock().ws.is_shown(&id);
                    if !shown {
                        hide(world, e);
                    }
                    shared.lock().dirty = true;
                }
            }
        }
    }
    crate::persist::restore_if_due(world, &shared);
}
