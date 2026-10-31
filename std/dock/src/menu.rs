//! The tab menu: what a right click, the Menu key or Shift+F10 on a tab
//! offers. It is the keyboard and assistive-technology path to every
//! arrangement a drag would make: pop the panel out into a window of its
//! own, pop its whole stack out, dock it back, maximize it, close it.
//!
//! The menu is the core `<menu>` popup, built for the tab it opens on, so
//! its arrow keys, Enter, Escape and outside-click dismissal are the popup's
//! own. Its labels are the module's default messages, which an app's
//! catalogue rewords or translates.

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_dock_model::Where;
use lumen_module::lumen_core::components::DomHidden;
use lumen_module::lumen_core::property_store::PropertyStore;
use lumen_module::lumen_ir::menu::{MenuRow, menu_open_signal, popup_menu};
use lumen_module::lumen_primitives::{MenuItemButton, TabStripButton};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::adopt::DockChrome;
use crate::realize::{Built, DockTab};
use crate::state::Shared;

/// The id of the tab menu's popup.
pub const MENU_ID: &str = "lumen-dock-tab-menu";

/// What a tab menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Pop the panel out into a window of its own.
    Detach,
    /// Pop the panel's whole stack out.
    DetachGroup,
    /// Dock the panel back where it came from.
    Attach,
    /// Float the panel over its dock.
    Float,
    /// Pin the panel to the edge of its dock nearest to it.
    Pin,
    /// Fill the dock with the panel's stack.
    Maximize,
    /// Give the dock its whole layout back.
    Restore,
    /// Close the panel, asking its `panelclose` handlers first.
    Close,
}

impl MenuAction {
    /// The item's id, as the popup and a script's `on_menu` see it.
    pub fn item_id(self) -> &'static str {
        match self {
            MenuAction::Detach => "lumen-dock-detach",
            MenuAction::DetachGroup => "lumen-dock-detach-group",
            MenuAction::Attach => "lumen-dock-attach",
            MenuAction::Float => "lumen-dock-float",
            MenuAction::Pin => "lumen-dock-pin",
            MenuAction::Maximize => "lumen-dock-maximize",
            MenuAction::Restore => "lumen-dock-restore",
            MenuAction::Close => "lumen-dock-close-panel",
        }
    }

    /// The message id of its label, and the label without a catalogue.
    fn label(self) -> (&'static str, &'static str) {
        match self {
            MenuAction::Detach => ("lumen-dock-move-to-window", "Move to new window"),
            MenuAction::DetachGroup => (
                "lumen-dock-move-group-to-window",
                "Move group to new window",
            ),
            MenuAction::Attach => ("lumen-dock-dock-back", "Dock back"),
            MenuAction::Float => ("lumen-dock-float", "Float"),
            MenuAction::Pin => ("lumen-dock-pin", "Pin to sidebar"),
            MenuAction::Maximize => ("lumen-dock-maximize", "Maximize"),
            MenuAction::Restore => ("lumen-dock-restore", "Restore layout"),
            MenuAction::Close => ("lumen-dock-close", "Close"),
        }
    }

    /// The label in the app's language.
    pub fn text(self) -> String {
        let (key, fallback) = self.label();
        crate::plugin::text(key, fallback)
    }

    const ALL: [MenuAction; 8] = [
        MenuAction::Detach,
        MenuAction::DetachGroup,
        MenuAction::Attach,
        MenuAction::Float,
        MenuAction::Pin,
        MenuAction::Maximize,
        MenuAction::Restore,
        MenuAction::Close,
    ];

    fn from_item(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.item_id() == id)
    }
}

/// On each item of an open tab menu: what it does.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockMenuItem(pub MenuAction);

/// The tab menu last opened: the panel it is for and its popup.
#[derive(Resource, Default, Debug)]
pub struct TabMenu {
    /// The panel the menu acts on.
    pub panel: Option<String>,
    /// The popup's root, under the dock or the window it opened in.
    pub root: Option<Entity>,
    /// An item picked, waiting for the menu to close.
    pub pending: Option<MenuAction>,
    /// A tab picked from a stack's overflow list, waiting for the list to
    /// close.
    pub show: Option<String>,
}

/// The panel whose tab `e` is, or is inside.
pub fn tab_panel(world: &World, e: Entity) -> Option<String> {
    let mut cur = Some(e);
    let mut depth = 0;
    while let Some(c) = cur {
        if let Some(b) = world.get::<TabStripButton>(c)
            && let Some(t) = world.get::<DockTab>(b.tab)
        {
            return Some(t.panel.clone());
        }
        depth += 1;
        if depth > 4 {
            return None;
        }
        cur = world.get::<ChildOf>(c).map(ChildOf::parent);
    }
    None
}

/// The items the menu offers for `panel`, in order.
pub fn actions_for(shared: &Shared, panel: &str) -> Vec<MenuAction> {
    let core = shared.lock();
    let Some(rec) = core.panels.get(panel) else {
        return Vec::new();
    };
    let live = |p: &String| core.panels.get(p).is_some_and(|r| r.entity.is_some());
    let mates = core
        .ws
        .find(panel)
        .and_then(|loc| core.ws.stack(&loc.stack))
        .map(|s| s.panels.iter().filter(|p| live(p)).count())
        .unwrap_or(1);
    let mut out = Vec::new();
    match core.ws.where_is(panel) {
        Where::Dock(dock) => {
            out.push(MenuAction::Detach);
            if mates > 1 {
                out.push(MenuAction::DetachGroup);
            }
            if rec.attrs.floatable {
                out.push(MenuAction::Float);
            }
            out.push(MenuAction::Pin);
            let maximized = core
                .ws
                .docks
                .get(&dock)
                .and_then(|d| d.maximized.as_deref());
            out.push(if maximized == Some(panel) {
                MenuAction::Restore
            } else {
                MenuAction::Maximize
            });
        }
        Where::Float(_) => {
            out.push(MenuAction::Detach);
            if mates > 1 {
                out.push(MenuAction::DetachGroup);
            }
            out.push(MenuAction::Attach);
            out.push(MenuAction::Pin);
        }
        Where::Window(w) => {
            let alone = core
                .ws
                .window(&w)
                .map(|w| w.root.panels().into_iter().filter(|p| live(p)).count() <= 1)
                .unwrap_or(true);
            if !alone {
                out.push(MenuAction::Detach);
            }
            if rec.info.dockable {
                out.push(MenuAction::Attach);
            }
        }
        Where::Sidebar(_) => out.push(MenuAction::Attach),
        Where::Hidden => {}
    }
    if rec.attrs.closable {
        out.push(MenuAction::Close);
    }
    out
}

/// Every entity under `root`, `root` first.
fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        out.push(e);
        if let Some(kids) = world.get::<Children>(e) {
            stack.extend(kids.iter());
        }
    }
    out
}

/// The element a popup for `panel` opens under: its dock, or the root of
/// its window of panels.
pub(crate) fn popup_container(world: &World, shared: &Shared, panel: &str) -> Option<Entity> {
    let core = shared.lock();
    let built = world.resource::<Built>();
    let e = match core.ws.where_is(panel) {
        Where::Dock(d) | Where::Float(d) | Where::Sidebar(d) => {
            core.docks.get(&d).map(|d| d.entity)
        }
        Where::Window(w) => built.windows.get(&w).map(|(_, surface)| *surface),
        Where::Hidden => None,
    };
    e.filter(|&e| world.get_entity(e).is_ok())
}

/// Open the popup `id` with `rows` under `container` at `at`, closing the
/// dock's popup that was open, and return its root and its items with their
/// item ids. Only one of the dock's popups is open at a time.
pub(crate) fn open_popup(
    world: &mut World,
    container: Entity,
    id: &str,
    rows: &[MenuRow],
    at: Vec2,
) -> (Entity, Vec<(Entity, String)>) {
    if let Some(old) = world.resource_mut::<TabMenu>().root.take()
        && let Ok(e) = world.get_entity_mut(old)
    {
        e.despawn();
    }
    let root = spawn_subtree(
        world,
        &popup_menu(id, rows),
        Some(container),
        Placeholders::Resolved,
    );
    world.entity_mut(root).insert(DockChrome);
    {
        let mut store = world.resource_mut::<PropertyStore>();
        store.set_global_str(&format!("__menu_at:{id}"), format!("{} {}", at.x, at.y));
        store.set_global_bool(&menu_open_signal(id), true);
    }
    // The popup's body mounts now rather than on the next tick, so every
    // part of it is marked before anything indexes it.
    {
        use bevy_ecs::system::RunSystemOnce;
        let _ = world.run_system_once(lumen_module::lumen_scene::spawn::reconcile_if_blocks);
    }
    let mut items = Vec::new();
    for e in subtree(world, root) {
        if let Some(b) = world.get::<MenuItemButton>(e) {
            items.push((e, b.item_id.clone()));
        }
        world.entity_mut(e).insert(DomHidden);
    }
    world.resource_mut::<TabMenu>().root = Some(root);
    (root, items)
}

/// Open the tab menu for `panel` at `at`, in the window coordinates of the
/// window its tab shows in.
pub fn open(world: &mut World, panel: String, at: Vec2) {
    let shared = world.resource::<Shared>().clone();
    let actions = actions_for(&shared, &panel);
    if actions.is_empty() {
        return;
    }
    let Some(container) = popup_container(world, &shared, &panel) else {
        return;
    };
    let rows: Vec<MenuRow> = actions
        .iter()
        .map(|a| MenuRow::Item {
            id: a.item_id().to_string(),
            label: a.text(),
            disabled: false,
            accel: None,
        })
        .collect();
    let (_, items) = open_popup(world, container, MENU_ID, &rows, at);
    for (e, id) in items {
        if let Some(a) = MenuAction::from_item(&id) {
            world.entity_mut(e).insert(DockMenuItem(a));
        }
    }
    world.resource_mut::<TabMenu>().panel = Some(panel);
}

/// The action of the menu item `e` is, or is inside.
pub fn item_action(
    world_items: &Query<&DockMenuItem>,
    parents: &Query<&ChildOf>,
    e: Entity,
) -> Option<MenuAction> {
    let mut cur = Some(e);
    for _ in 0..3 {
        let c = cur?;
        if let Ok(item) = world_items.get(c) {
            return Some(item.0);
        }
        cur = parents.get(c).ok().map(ChildOf::parent);
    }
    None
}
