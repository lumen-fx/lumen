//! What the module knows between ticks: the live workspace, the panels and
//! docks the page declared, and what changed since the elements last
//! followed the model.
//!
//! A script function body has no world, so the state lives behind one lock
//! the bodies and the module's systems share. A body changes the model and
//! marks it dirty; the realize system makes the elements follow on the same
//! tick.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use bevy_ecs::prelude::*;
use lumen_dock_model::{Effect, PanelId, PanelInfo, Place, Workspace};
use lumen_module::lumen_core::components::LumenAttributes;
use lumen_module::lumen_core::window::WindowKind;

/// Why the layout changed, as the `reason` of the events it raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The user did it: a tab's close button, a key, a window's close
    /// button.
    User,
    /// The app did it: a script call or a Rust call.
    Api,
    /// A layout was applied or restored.
    Layout,
}

impl Reason {
    /// The word the events carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::User => "user",
            Reason::Api => "api",
            Reason::Layout => "layout",
        }
    }
}

/// Who a modal window blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Modal {
    /// Nobody.
    #[default]
    None,
    /// The primary window.
    Parent,
    /// Every other window.
    App,
}

/// A panel's own-window properties and the rest of what its attributes say.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelAttrs {
    /// Tab text and accessible name.
    pub title: String,
    /// The OS title of its own window.
    pub window_title: Option<String>,
    /// The signal its own window's title follows (`bind-window-title`).
    pub window_title_signal: Option<String>,
    /// The image its tab shows before the title.
    pub icon: Option<String>,
    /// The OS window type of its own window.
    pub window_kind: WindowKind,
    /// Whose input its window blocks.
    pub modal: Modal,
    /// Initial inner size of its own window, in logical pixels.
    pub window_size: Option<[f32; 2]>,
    /// Whether its own window can be resized.
    pub resizable: bool,
    /// Whether a user can close it.
    pub closable: bool,
    /// Closing removes the element instead of hiding it.
    pub destroy_on_close: bool,
    /// Starts hidden.
    pub hidden: bool,
    /// The app's kind and state, for a panel the app recreates on restore.
    pub kind: Option<String>,
    /// See `kind`.
    pub state: Option<String>,
}

impl Default for PanelAttrs {
    fn default() -> Self {
        Self {
            title: String::new(),
            window_title: None,
            window_title_signal: None,
            icon: None,
            window_kind: WindowKind::Normal,
            modal: Modal::None,
            window_size: None,
            resizable: true,
            closable: true,
            destroy_on_close: false,
            hidden: false,
            kind: None,
            state: None,
        }
    }
}

/// Read a boolean attribute: `true`, `yes`, `1` and the empty value are
/// true, `false`, `no` and `0` false; an absent one is `default`.
pub fn flag(attrs: Option<&LumenAttributes>, name: &str, default: bool) -> bool {
    match attrs.and_then(|a| a.get(name)) {
        None => default,
        Some(v) => matches!(v.trim(), "" | "true" | "yes" | "1"),
    }
}

/// An attribute's value, trimmed, when it is present and not empty.
pub fn attr<'a>(attrs: Option<&'a LumenAttributes>, name: &str) -> Option<&'a str> {
    attrs
        .and_then(|a| a.get(name))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Two numbers, as `window-size="640 480"` writes them.
pub fn pair(s: &str) -> Option<[f32; 2]> {
    let mut it = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.trim_end_matches("px").parse::<f32>().ok());
    let a = it.next()??;
    let b = it.next()??;
    (it.next().is_none() && a > 0.0 && b > 0.0).then_some([a, b])
}

impl PanelAttrs {
    /// Read a `<dock-panel>`'s attributes.
    pub fn read(attrs: Option<&LumenAttributes>, id: &str) -> Self {
        Self {
            title: attr(attrs, "title").unwrap_or(id).to_string(),
            window_title: attr(attrs, "window-title").map(str::to_string),
            window_title_signal: attr(attrs, "bind-window-title").map(str::to_string),
            icon: attr(attrs, "icon").map(str::to_string),
            window_kind: match attr(attrs, "window-kind") {
                Some("dialog") => WindowKind::Dialog,
                Some("tool") => WindowKind::Tool,
                _ => WindowKind::Normal,
            },
            modal: match attr(attrs, "modal") {
                Some("parent") => Modal::Parent,
                Some("app") => Modal::App,
                _ => Modal::None,
            },
            window_size: attr(attrs, "window-size").and_then(pair),
            resizable: flag(attrs, "resizable", true),
            closable: flag(attrs, "closable", true),
            destroy_on_close: flag(attrs, "destroy-on-close", false),
            hidden: flag(attrs, "hidden", false),
            kind: attr(attrs, "kind").map(str::to_string),
            state: attrs.and_then(|a| a.get("state")).map(str::to_string),
        }
    }

    /// The OS title of its own window.
    pub fn os_title(&self) -> &str {
        self.window_title.as_deref().unwrap_or(&self.title)
    }
}

/// Read the placement half of a `<dock-panel>`'s attributes, for a panel
/// declared in `dock` (`None` outside any dock).
pub fn panel_info(attrs: Option<&LumenAttributes>, dock: Option<String>) -> PanelInfo {
    PanelInfo {
        dock,
        place: attr(attrs, "place")
            .and_then(Place::parse)
            .unwrap_or_default(),
        dockable: flag(attrs, "dockable", true),
    }
}

/// One panel the page declared or the app created.
#[derive(Clone, Debug)]
pub struct PanelRec {
    /// The `<dock-panel>` element; `None` while the panel is a placeholder
    /// the app has not created again.
    pub entity: Option<Entity>,
    /// Where it goes.
    pub info: PanelInfo,
    /// What its attributes say.
    pub attrs: PanelAttrs,
    /// The element it was declared under: a free panel's window sits there.
    pub declared_in: Option<Entity>,
    /// The value of the signal its window title is bound to, once it has
    /// one.
    pub bound_title: Option<String>,
}

impl PanelRec {
    /// The OS title of its own window: the bound signal's value, else its
    /// `window-title`, else its `title`.
    pub fn os_title(&self) -> &str {
        self.bound_title
            .as_deref()
            .unwrap_or_else(|| self.attrs.os_title())
    }
}

/// One `<dock>` element of the page.
#[derive(Clone, Debug)]
pub struct DockRec {
    /// The `<dock>` element.
    pub entity: Entity,
    /// Its accessible name.
    pub label: Option<String>,
}

/// An event to raise once the elements follow the model.
#[derive(Clone, Debug)]
pub struct Pending {
    /// What happened.
    pub effect: Effect,
    /// Why.
    pub reason: Reason,
}

/// The module's state.
#[derive(Debug, Default)]
pub struct Core {
    /// The live workspace.
    pub ws: Workspace,
    /// The workspace the markup describes, before anything changed it.
    pub default: Workspace,
    /// The `<dock-layout>` layouts, by name.
    pub builtin: BTreeMap<String, Workspace>,
    /// The user's own named layouts, by name.
    pub user: BTreeMap<String, Workspace>,
    /// Every panel, by id.
    pub panels: BTreeMap<PanelId, PanelRec>,
    /// Every dock, by id.
    pub docks: BTreeMap<String, DockRec>,
    /// The model changed and the elements have not followed yet.
    pub dirty: bool,
    /// The layout changed since `layoutchange` last fired, and why.
    pub changed: Option<Reason>,
    /// Events waiting for the elements to follow.
    pub pending: Vec<Pending>,
    /// `layouterror` events waiting: reason, detail.
    pub errors: Vec<(String, String)>,
    /// Placeholders reported with `panelrestore` already.
    pub reported: BTreeSet<PanelId>,
    /// The app's layout version.
    pub layout_version: u32,
    /// The auto-saved file is read once the page has mounted.
    pub restore_due: bool,
    /// The layout or a window's place changed since the auto-saved file was
    /// last written.
    pub save_due: bool,
    /// The name the layout came from, as the `dock.layout` signal shows it.
    pub layout_name: String,
    /// A panel `dock::focus` asked to move keyboard focus into.
    pub focus_request: Option<PanelId>,
    /// Panel elements closing removes from the document.
    pub destroy: Vec<Entity>,
    /// The element last focused in each panel, where F6 returns to.
    pub last_focused: BTreeMap<PanelId, Entity>,
    /// The elements followed a change the events and signals have not
    /// reported yet.
    pub report_due: bool,
}

impl Core {
    /// What the model needs to know about `panel`.
    pub fn info(&self, panel: &str) -> PanelInfo {
        self.panels
            .get(panel)
            .map(|p| p.info.clone())
            .unwrap_or(PanelInfo {
                dock: self.docks.keys().next().cloned(),
                place: Place::Center,
                dockable: true,
            })
    }

    /// Record what an operation did, and that the elements must follow.
    pub fn apply(&mut self, effects: Vec<Effect>, reason: Reason) {
        self.dirty = true;
        self.changed = Some(match self.changed {
            Some(Reason::User) => Reason::User,
            _ => reason,
        });
        self.save_due = true;
        self.pending
            .extend(effects.into_iter().map(|effect| Pending { effect, reason }));
    }

    /// The panel `entity` is, if it is one.
    pub fn panel_of(&self, entity: Entity) -> Option<&PanelId> {
        self.panels
            .iter()
            .find(|(_, p)| p.entity == Some(entity))
            .map(|(id, _)| id)
    }
}

/// The shared state, as the module's systems and its script functions reach
/// it. One per app.
#[derive(Resource, Clone, Default)]
pub struct Shared(pub Arc<Mutex<Core>>);

impl Shared {
    /// Lock the state. A panic while it was held leaves the model as the
    /// operation left it, which the invariants still cover, so the lock is
    /// taken back rather than refused.
    pub fn lock(&self) -> MutexGuard<'_, Core> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> LumenAttributes {
        let mut a = LumenAttributes::default();
        for (k, v) in pairs {
            a.set(k, *v);
        }
        a
    }

    #[test]
    fn attributes_read_as_the_markup_writes_them() {
        let a = attrs(&[
            ("title", "Preferences"),
            ("window-kind", "dialog"),
            ("modal", "parent"),
            ("window-size", "640 480"),
            ("resizable", "false"),
            ("closable", "no"),
            ("destroy-on-close", ""),
            ("hidden", "true"),
            ("kind", "doc"),
            ("state", ""),
            ("place", "window"),
            ("dockable", "0"),
        ]);
        let p = PanelAttrs::read(Some(&a), "prefs");
        assert_eq!(p.title, "Preferences");
        assert_eq!(p.os_title(), "Preferences");
        assert_eq!(p.window_kind, WindowKind::Dialog);
        assert_eq!(p.modal, Modal::Parent);
        assert_eq!(p.window_size, Some([640.0, 480.0]));
        assert!(!p.resizable && !p.closable && p.destroy_on_close && p.hidden);
        assert_eq!(p.kind.as_deref(), Some("doc"));
        assert_eq!(p.state.as_deref(), Some(""));
        let info = panel_info(Some(&a), None);
        assert_eq!(info.place, Place::Window);
        assert!(!info.dockable);

        let plain = PanelAttrs::read(None, "x");
        assert_eq!(plain.title, "x");
        assert_eq!(
            plain,
            PanelAttrs {
                title: "x".into(),
                ..PanelAttrs::default()
            }
        );
        let tool = attrs(&[
            ("window-kind", "tool"),
            ("modal", "app"),
            ("window-title", "T"),
            ("bind-window-title", "doc.name"),
            ("icon", "icons/t.svg"),
        ]);
        let p = PanelAttrs::read(Some(&tool), "t");
        assert_eq!(
            (p.window_kind, p.modal, p.os_title()),
            (WindowKind::Tool, Modal::App, "T")
        );
        assert_eq!(p.window_title_signal.as_deref(), Some("doc.name"));
        assert_eq!(p.icon.as_deref(), Some("icons/t.svg"));
        let mut rec = PanelRec {
            entity: None,
            info: panel_info(None, None),
            attrs: p,
            declared_in: None,
            bound_title: None,
        };
        assert_eq!(rec.os_title(), "T");
        rec.bound_title = Some("draft.txt".into());
        assert_eq!(rec.os_title(), "draft.txt");
        assert_eq!(pair("10, 20px"), Some([10.0, 20.0]));
        assert_eq!(pair("10"), None);
        assert_eq!(pair("1 2 3"), None);
        assert_eq!(pair("0 5"), None);
        for (r, s) in [
            (Reason::User, "user"),
            (Reason::Api, "api"),
            (Reason::Layout, "layout"),
        ] {
            assert_eq!(r.as_str(), s);
        }
    }

    #[test]
    fn the_layout_change_reason_keeps_the_user() {
        let mut core = Core::default();
        core.apply(Vec::new(), Reason::User);
        core.apply(Vec::new(), Reason::Api);
        assert_eq!(core.changed, Some(Reason::User));
        let mut core = Core::default();
        core.apply(Vec::new(), Reason::Api);
        core.apply(Vec::new(), Reason::Layout);
        assert_eq!(core.changed, Some(Reason::Layout));
        assert!(core.dirty && core.save_due);
        assert_eq!(core.info("nobody").place, Place::Center);
    }
}
