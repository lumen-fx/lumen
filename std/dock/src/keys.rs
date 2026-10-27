//! The keys the dock answers, and moving focus between its stacks and
//! windows.
//!
//! F6 and Shift+F6 move between the stacks of the focused window,
//! Ctrl+F6 and Ctrl+Shift+F6 between windows, Ctrl+PageDown and
//! Ctrl+PageUp between the tabs of the stack holding focus. On a focused
//! tab, Shift+Space maximizes its panel and Delete closes it; Escape gives a
//! maximized dock its layout back. Every one can be rebound or turned off in
//! the module's `keys` config.

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::prelude::*;
use lumen_dock_model::{Container, Where};
use lumen_module::lumen_core::components::{Disabled, Display, Style, TabIndex, Visible};
use lumen_module::lumen_core::input::{FocusTracker, FocusVisible, Focused, Key, Modifiers};
use lumen_module::lumen_core::window::{PrimaryWindow, WindowRuntime};
use lumen_module::lumen_primitives::TabStripButton;
use lumen_module::lumen_script::CustomEvent;

use crate::adopt::DockPanel;
use crate::realize::{Built, DockTab, DockWindow};
use crate::state::{Reason, Shared};

/// A key with its modifiers, as `Ctrl+Shift+F6` writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    /// The key.
    pub key: Key,
    /// Ctrl held.
    pub ctrl: bool,
    /// Shift held.
    pub shift: bool,
    /// Alt held.
    pub alt: bool,
}

impl Chord {
    /// Read `Ctrl+Shift+F6`: modifiers `Ctrl`, `Shift`, `Alt` joined to a key
    /// name with `+`. `None` for an empty or unreadable chord.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last()?;
        if key.is_empty() {
            return None;
        }
        let mut chord = Chord {
            key: Key::from(*key),
            ctrl: false,
            shift: false,
            alt: false,
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" => chord.alt = true,
                _ => return None,
            }
        }
        Some(chord)
    }

    fn matches(&self, key: &Key, m: Modifiers) -> bool {
        *key == self.key && m.ctrl == self.ctrl && m.shift == self.shift && m.alt == self.alt
    }
}

/// The keys the module answers. `None` turns one off.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Keys {
    /// The next stack of the focused window.
    pub next_stack: Option<Chord>,
    /// The previous stack.
    pub prev_stack: Option<Chord>,
    /// The next window.
    pub next_window: Option<Chord>,
    /// The previous window.
    pub prev_window: Option<Chord>,
    /// The next tab of the stack holding focus.
    pub next_tab: Option<Chord>,
    /// The previous tab.
    pub prev_tab: Option<Chord>,
    /// Maximize the focused tab's panel, or give its dock the layout back.
    pub maximize: Option<Chord>,
    /// Give a maximized dock its layout back.
    pub restore: Option<Chord>,
    /// Close the focused tab's panel.
    pub close_tab: Option<Chord>,
}

impl Default for Keys {
    fn default() -> Self {
        let c = Chord::parse;
        Self {
            next_stack: c("F6"),
            prev_stack: c("Shift+F6"),
            next_window: c("Ctrl+F6"),
            prev_window: c("Ctrl+Shift+F6"),
            next_tab: c("Ctrl+PageDown"),
            prev_tab: c("Ctrl+PageUp"),
            maximize: c("Shift+Space"),
            restore: c("Escape"),
            close_tab: c("Delete"),
        }
    }
}

impl Keys {
    /// No keys at all.
    pub fn none() -> Self {
        Self {
            next_stack: None,
            prev_stack: None,
            next_window: None,
            prev_window: None,
            next_tab: None,
            prev_tab: None,
            maximize: None,
            restore: None,
            close_tab: None,
        }
    }

    /// Read the `keys` config value: `false` turns every key off, a table
    /// rebinds the ones it names (an empty string turns one off).
    pub fn from_config(value: &toml::Value) -> Self {
        if value.as_bool() == Some(false) {
            return Self::none();
        }
        let mut keys = Self::default();
        let Some(table) = value.as_table() else {
            return keys;
        };
        for (name, chord) in table {
            let chord = chord.as_str().and_then(Chord::parse);
            let slot = match name.as_str() {
                "next-stack" => &mut keys.next_stack,
                "prev-stack" => &mut keys.prev_stack,
                "next-window" => &mut keys.next_window,
                "prev-window" => &mut keys.prev_window,
                "next-tab" => &mut keys.next_tab,
                "prev-tab" => &mut keys.prev_tab,
                "maximize" => &mut keys.maximize,
                "restore" => &mut keys.restore,
                "close-tab" => &mut keys.close_tab,
                other => {
                    lumen_module::lumen_core::warn_line!(
                        "lumen-dock: no key called '{other}' to bind"
                    );
                    continue;
                }
            };
            *slot = chord;
        }
        keys
    }
}

fn parent(world: &World, e: Entity) -> Option<Entity> {
    world.get::<ChildOf>(e).map(ChildOf::parent)
}

/// The panel `e` is in, and the strip button's panel when `e` is a tab.
fn panel_at(world: &World, e: Entity) -> Option<String> {
    let mut cur = Some(e);
    while let Some(c) = cur {
        if let Some(p) = world.get::<DockPanel>(c) {
            return Some(p.id.clone());
        }
        if let Some(b) = world.get::<TabStripButton>(c)
            && let Some(t) = world.get::<DockTab>(b.tab)
        {
            return Some(t.panel.clone());
        }
        cur = parent(world, c);
    }
    None
}

fn shown(world: &World, e: Entity) -> bool {
    let mut cur = Some(e);
    while let Some(c) = cur {
        if world.get::<Visible>(c).is_some_and(|v| !v.0)
            || world
                .get::<Style>(c)
                .is_some_and(|s| s.display == Display::None)
        {
            return false;
        }
        cur = parent(world, c);
    }
    true
}

/// The first element in `root`'s subtree that takes focus, in tree order.
fn first_focusable(world: &World, root: Entity) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<TabIndex>(e).is_some_and(|t| t.0 >= 0)
            && world.get::<Disabled>(e).is_none()
            && shown(world, e)
        {
            return Some(e);
        }
        if let Some(kids) = world.get::<Children>(e) {
            stack.extend(kids.iter().rev());
        }
    }
    None
}

/// Move keyboard focus to `target`.
fn focus(world: &mut World, target: Entity) {
    let prev = world.resource::<FocusTracker>().0;
    if prev == Some(target) {
        return;
    }
    if let Some(prev) = prev
        && let Ok(mut e) = world.get_entity_mut(prev)
    {
        e.remove::<(Focused, FocusVisible)>();
    }
    world.entity_mut(target).insert((Focused, FocusVisible));
    world.resource_mut::<FocusTracker>().0 = Some(target);
}

/// Move focus into `panel`: where it was last, else its first focusable
/// element, else its tab.
fn focus_panel(world: &mut World, shared: &Shared, panel: &str) -> bool {
    let (entity, last) = {
        let core = shared.lock();
        (
            core.panels.get(panel).and_then(|p| p.entity),
            core.last_focused.get(panel).copied(),
        )
    };
    let Some(entity) = entity else {
        return false;
    };
    let target = last
        .filter(|&l| {
            world.get_entity(l).is_ok()
                && panel_at(world, l).as_deref() == Some(panel)
                && shown(world, l)
        })
        .or_else(|| first_focusable(world, entity))
        .or_else(|| {
            let built = world.resource::<Built>();
            let tab = built.tabs.get(panel).copied()?;
            let mut q = world.try_query::<(Entity, &TabStripButton)>()?;
            q.iter(world).find(|(_, b)| b.tab == tab).map(|(e, _)| e)
        });
    match target {
        Some(t) => {
            focus(world, t);
            true
        }
        None => false,
    }
}

/// Move focus where `dock::focus` asked.
pub fn focus_requests(world: &mut World) {
    let shared = world.resource::<Shared>().clone();
    let (request, destroy) = {
        let mut core = shared.lock();
        (core.focus_request.take(), std::mem::take(&mut core.destroy))
    };
    for e in destroy {
        if let Ok(e) = world.get_entity_mut(e) {
            e.despawn();
        }
    }
    if let Some(panel) = request {
        focus_panel(world, &shared, &panel);
    }
}

/// The container `panel` is in.
fn container_of(shared: &Shared, panel: &str) -> Option<Container> {
    shared.lock().ws.find(panel).map(|l| l.container)
}

/// Answer the dock's keys.
pub fn dock_keys(world: &mut World, pressed: Vec<(Key, Modifiers)>) {
    let keys = world.resource::<Keys>().clone();
    let shared = world.resource::<Shared>().clone();
    // Escape during a drag cancels the drag and nothing else.
    let dragging = world
        .get_resource::<crate::drag::DockDrag>()
        .is_some_and(|d| d.active());
    for (key, m) in pressed {
        if dragging && key == Key::Named(lumen_module::lumen_core::input::NamedKey::Escape) {
            continue;
        }
        let hit = |c: &Option<Chord>| c.as_ref().is_some_and(|c| c.matches(&key, m));
        let focused = world.resource::<FocusTracker>().0;
        let panel = focused.and_then(|f| panel_at(world, f));
        let on_tab = focused.is_some_and(|f| {
            world
                .get::<TabStripButton>(f)
                .is_some_and(|b| world.get::<DockTab>(b.tab).is_some())
        });
        if hit(&keys.next_stack) || hit(&keys.prev_stack) {
            let back = hit(&keys.prev_stack);
            let container = panel
                .as_deref()
                .and_then(|p| container_of(&shared, p))
                .or_else(|| focused_container(world, &shared));
            let Some(container) = container else { continue };
            let order: Vec<String> = {
                let core = shared.lock();
                core.ws
                    .stacks_of(&container)
                    .into_iter()
                    .filter_map(|s| {
                        s.active
                            .clone()
                            .filter(|a| core.panels.get(a).is_some_and(|r| r.entity.is_some()))
                    })
                    .collect()
            };
            if order.is_empty() {
                continue;
            }
            let here = panel.as_deref().and_then(|p| {
                let core = shared.lock();
                let stack = core.ws.find(p)?.stack;
                order
                    .iter()
                    .position(|a| core.ws.find(a).is_some_and(|l| l.stack == stack))
            });
            let n = order.len();
            let next = match (here, back) {
                (None, false) => 0,
                (None, true) => n - 1,
                (Some(i), false) => (i + 1) % n,
                (Some(i), true) => (i + n - 1) % n,
            };
            focus_panel(world, &shared, &order[next]);
        } else if hit(&keys.next_window) || hit(&keys.prev_window) {
            let back = hit(&keys.prev_window);
            // The main window first, then each window of panels.
            let mut windows: Vec<Option<String>> = vec![None];
            windows.extend(shared.lock().ws.windows.iter().map(|w| Some(w.id.clone())));
            let here = panel
                .as_deref()
                .map(|p| match shared.lock().ws.where_is(p) {
                    Where::Window(w) => Some(w),
                    _ => None,
                });
            let here = here.unwrap_or_else(|| focused_window_id(world));
            let i = windows.iter().position(|w| *w == here).unwrap_or(0);
            let n = windows.len();
            if n < 2 {
                continue;
            }
            let next = if back { (i + n - 1) % n } else { (i + 1) % n };
            let target_panel = {
                let core = shared.lock();
                let container = match &windows[next] {
                    Some(w) => Container::Window(w.clone()),
                    None => match core.ws.docks.keys().next() {
                        Some(d) => Container::Dock(d.clone()),
                        None => continue,
                    },
                };
                core.ws
                    .stacks_of(&container)
                    .into_iter()
                    .find_map(|s| s.active.clone())
            };
            if let Some(p) = target_panel {
                focus_panel(world, &shared, &p);
            }
        } else if hit(&keys.next_tab) || hit(&keys.prev_tab) {
            let back = hit(&keys.prev_tab);
            let Some(p) = panel else { continue };
            let target = {
                let core = shared.lock();
                core.ws.find(&p).and_then(|loc| {
                    let stack = core.ws.stack(&loc.stack)?;
                    let live: Vec<&String> = stack
                        .panels
                        .iter()
                        .filter(|q| core.panels.get(*q).is_some_and(|r| r.entity.is_some()))
                        .collect();
                    let i = live.iter().position(|q| **q == p)?;
                    let n = live.len();
                    (n > 1).then(|| {
                        let j = if back { (i + n - 1) % n } else { (i + 1) % n };
                        live[j].clone()
                    })
                })
            };
            if let Some(t) = target {
                let _ = shared.lock().op_activate(&t, Reason::User);
                if on_tab {
                    // Focus follows along the strip once the tab exists.
                    shared.lock().focus_request = Some(t);
                }
            }
        } else if hit(&keys.maximize) && on_tab {
            let Some(p) = panel else { continue };
            let mut core = shared.lock();
            let dock = match core.ws.where_is(&p) {
                Where::Dock(d) => d,
                _ => continue,
            };
            if core
                .ws
                .docks
                .get(&dock)
                .and_then(|d| d.maximized.as_deref())
                == Some(p.as_str())
            {
                core.op_restore_maximized(&dock, Reason::User);
            } else {
                let _ = core.op_maximize(&p, Reason::User);
            }
        } else if hit(&keys.restore) {
            let mut core = shared.lock();
            // A slid-out panel slides back in first.
            let open: Vec<String> = core
                .ws
                .docks
                .iter()
                .filter(|(_, d)| d.open.is_some())
                .map(|(id, _)| id.clone())
                .collect();
            if !open.is_empty() {
                for dock in open {
                    let effects = core.ws.close_pinned(&dock);
                    core.apply(effects, Reason::User);
                }
                continue;
            }
            let dock = panel
                .as_deref()
                .and_then(|p| match core.ws.where_is(p) {
                    Where::Dock(d) => Some(d),
                    _ => None,
                })
                .or_else(|| {
                    core.ws
                        .docks
                        .iter()
                        .find(|(_, d)| d.maximized.is_some())
                        .map(|(id, _)| id.clone())
                });
            if let Some(dock) = dock {
                core.op_restore_maximized(&dock, Reason::User);
            }
        } else if hit(&keys.close_tab) && on_tab {
            let Some(p) = panel else { continue };
            let entity = {
                let core = shared.lock();
                core.panels
                    .get(&p)
                    .filter(|r| r.attrs.closable)
                    .and_then(|r| r.entity)
            };
            if let Some(e) = entity {
                let mut ask = CustomEvent::new(e, "panelclose").with("reason", "user");
                ask.cancelable = true;
                world.write_message(ask);
            }
        }
    }
}

/// The dock or window of the window with keyboard focus.
fn focused_container(world: &mut World, shared: &Shared) -> Option<Container> {
    match focused_window_id(world) {
        Some(w) => Some(Container::Window(w)),
        None => shared
            .lock()
            .ws
            .docks
            .keys()
            .next()
            .cloned()
            .map(Container::Dock),
    }
}

/// The model id of the focused window, `None` for the main window.
fn focused_window_id(world: &mut World) -> Option<String> {
    let mut q = world.query::<(&WindowRuntime, Option<&DockWindow>, Has<PrimaryWindow>)>();
    q.iter(world)
        .find(|(r, ..)| r.focused)
        .and_then(|(_, d, _)| d.map(|d| d.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_as_written() {
        let c = Chord::parse("Ctrl+Shift+F6").unwrap();
        assert!(c.ctrl && c.shift && !c.alt);
        assert_eq!(c.key, Key::Character("F6".into()));
        assert_eq!(Chord::parse("Alt+Delete").unwrap().key, Key::from("Delete"));
        assert!(Chord::parse("").is_none());
        assert!(Chord::parse("Hyper+X").is_none());
        assert!(Chord::parse("Ctrl+").is_none());
        let m = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        };
        assert!(c.matches(&Key::Character("F6".into()), m));
        assert!(!c.matches(&Key::Character("F6".into()), Modifiers::default()));
    }

    #[test]
    fn the_keys_config_rebinds_and_turns_off() {
        let off = Keys::from_config(&toml::Value::Boolean(false));
        assert_eq!(off, Keys::none());
        let table: toml::Value = toml::from_str::<toml::Table>(
            "next-stack = \"Alt+F7\"\nclose-tab = \"\"\nbogus = \"X\"\n",
        )
        .unwrap()
        .into();
        let keys = Keys::from_config(&table);
        assert_eq!(keys.next_stack, Chord::parse("Alt+F7"));
        assert_eq!(keys.close_tab, None);
        assert_eq!(keys.prev_stack, Keys::default().prev_stack);
        assert_eq!(Keys::from_config(&toml::Value::Integer(3)), Keys::default());
    }
}
