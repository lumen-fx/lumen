//! The operations every surface reaches: the `dock` script namespace, the
//! C ABI through `lumen_call`, and the [`Dock`] system parameter a Rust app
//! uses. One implementation; each surface is a thin call into it.

use std::collections::BTreeSet;
use std::sync::Arc;

use bevy_ecs::system::{Res, SystemParam};
use lumen_dock_model::{OpError, PanelId, Rect, Side, Target, Where, WindowPlace, Workspace};
use lumen_module::lumen_script::{ScriptFn, ScriptFnBody, ScriptValue};

use crate::state::{Core, Reason, Shared};

/// Why a dock call did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockError {
    /// The model refused the operation.
    Op(OpError),
    /// No panel has the id.
    UnknownPanel(String),
    /// No dock has the id.
    UnknownDock(String),
    /// No named layout has the name.
    UnknownLayout(String),
    /// A zone that is not `tab`, `left`, `right`, `top` or `bottom`.
    BadZone(String),
    /// An edge that is not `left`, `right`, `top` or `bottom`.
    BadEdge(String),
    /// The panel does not float (`floatable="false"`).
    NotFloatable(String),
    /// A layout that cannot be used.
    Restore(lumen_dock_model::RestoreError),
}

impl std::fmt::Display for DockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DockError::Op(e) => e.fmt(f),
            DockError::UnknownPanel(p) => write!(f, "no panel has the id '{p}'"),
            DockError::UnknownDock(d) => write!(f, "no dock has the id '{d}'"),
            DockError::UnknownLayout(n) => write!(f, "no layout is called '{n}'"),
            DockError::BadZone(z) => write!(
                f,
                "'{z}' is not a zone: use tab, left, right, top or bottom"
            ),
            DockError::BadEdge(e) => {
                write!(f, "'{e}' is not an edge: use left, right, top or bottom")
            }
            DockError::NotFloatable(p) => write!(f, "panel '{p}' does not float"),
            DockError::Restore(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for DockError {}

impl From<OpError> for DockError {
    fn from(e: OpError) -> Self {
        DockError::Op(e)
    }
}

/// The edge of its dock nearest to where `panel` sits in the dock's tree,
/// as the tab menu's Pin pins it: a stack first in a row is nearest the
/// left, last the right; first in a column the top, last the bottom; the
/// bottom when nothing says otherwise.
pub fn nearest_edge(ws: &Workspace, panel: &str) -> &'static str {
    let Some(dock) = ws
        .find(panel)
        .and_then(|l| l.container.dock().map(str::to_string))
    else {
        return "bottom";
    };
    let mut node = &ws.docks[&dock].root;
    loop {
        let lumen_dock_model::Node::Split(split) = node else {
            return "bottom";
        };
        let Some(i) = split
            .children
            .iter()
            .position(|c| c.node.panels().iter().any(|p| *p == panel))
        else {
            return "bottom";
        };
        let last = split.children.len() - 1;
        match (split.axis, i) {
            (lumen_dock_model::Axis::Row, 0) => return "left",
            (lumen_dock_model::Axis::Row, i) if i == last => return "right",
            (lumen_dock_model::Axis::Column, 0) => return "top",
            (lumen_dock_model::Axis::Column, i) if i == last => return "bottom",
            _ => node = &split.children[i].node,
        }
    }
}

/// A window's number, as `dock::windows` lists it: the digits of its id.
pub fn window_number(id: &str) -> i64 {
    id.trim_start_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .unwrap_or(0)
}

impl Core {
    fn known(&self, panel: &str) -> Result<(), DockError> {
        if self.panels.contains_key(panel) || self.ws.is_shown(panel) {
            Ok(())
        } else {
            Err(DockError::UnknownPanel(panel.to_string()))
        }
    }

    /// Show `panel`.
    pub fn op_show(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let info = self.info(panel);
        let effects = self.ws.show(panel, &info)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Hide `panel`.
    pub fn op_hide(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let effects = self.ws.hide(panel)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Show `panel` if hidden, hide it if shown. Returns whether it is shown.
    /// A pinned panel slides out and back in.
    pub fn op_toggle(&mut self, panel: &str, reason: Reason) -> Result<bool, DockError> {
        if let Some((dock, _)) = self.ws.pinned_at(panel) {
            if self.ws.in_view(panel) {
                let effects = self.ws.close_pinned(&dock);
                self.apply(effects, reason);
                return Ok(false);
            }
            let effects = self.ws.open_pinned(panel)?;
            self.apply(effects, reason);
            return Ok(true);
        }
        if self.ws.is_shown(panel) {
            self.op_hide(panel, reason).map(|()| false)
        } else {
            self.op_show(panel, reason).map(|()| true)
        }
    }

    /// Make `panel` its stack's showing tab.
    pub fn op_activate(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let effects = self.ws.activate(panel)?;
        if !effects.is_empty() {
            self.apply(effects, reason);
        }
        Ok(())
    }

    /// Show `panel` and ask for focus in it.
    pub fn op_focus(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.op_show(panel, reason)?;
        self.focus_request = Some(panel.to_string());
        self.dirty = true;
        Ok(())
    }

    /// Close `panel`: hide it, or remove it when it is destroyed on close.
    pub fn op_close(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let destroy = self
            .panels
            .get(panel)
            .is_some_and(|p| p.attrs.destroy_on_close);
        if destroy {
            if let Some(e) = self.panels.get(panel).and_then(|p| p.entity) {
                self.destroy.push(e);
            }
            let effects = self.ws.forget(panel);
            self.panels.remove(panel);
            self.apply(effects, reason);
            return Ok(());
        }
        if !self.ws.is_shown(panel) {
            return Ok(());
        }
        self.op_hide(panel, reason)
    }

    /// Move `panel` beside `target` (a panel or a dock) into `zone`.
    pub fn op_move(
        &mut self,
        panel: &str,
        target: &str,
        zone: &str,
        reason: Reason,
    ) -> Result<(), DockError> {
        self.known(panel)?;
        let side = match zone {
            "tab" | "center" => None,
            other => Some(Side::parse(other).ok_or_else(|| DockError::BadZone(other.into()))?),
        };
        let to = if let Some(loc) = self.ws.find(target) {
            match side {
                None => Target::Tab {
                    stack: loc.stack.clone(),
                    index: loc.index + 1,
                },
                Some(side) => Target::Edge {
                    node: loc.stack,
                    side,
                },
            }
        } else if self.ws.docks.contains_key(target) {
            match side {
                None => {
                    let stacks = self
                        .ws
                        .stacks_of(&lumen_dock_model::Container::Dock(target.into()));
                    let center = stacks
                        .iter()
                        .find(|s| s.center)
                        .or(stacks.first())
                        .map(|s| (s.id.clone(), s.panels.len()))
                        .ok_or_else(|| DockError::UnknownDock(target.into()))?;
                    Target::Tab {
                        stack: center.0,
                        index: center.1,
                    }
                }
                Some(side) => Target::RootEdge {
                    dock: target.into(),
                    side,
                },
            }
        } else {
            return Err(DockError::UnknownPanel(target.to_string()));
        };
        if !self.ws.is_shown(panel) {
            self.op_show(panel, reason)?;
        }
        let info = self.info(panel);
        let effects = self.ws.move_to(panel, &to, &info)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Pop `panel` out into a window of its own.
    pub fn op_detach(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        if !self.ws.is_shown(panel) {
            self.op_show(panel, reason)?;
        }
        let info = self.info(panel);
        let effects = self.ws.detach(panel, WindowPlace::default(), &info)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Pop the stack holding `panel` out into a window of its own.
    pub fn op_detach_group(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let effects = self.ws.detach_group(panel, WindowPlace::default())?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Dock `panel` back from its window, its float or its sidebar.
    pub fn op_attach(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let info = self.info(panel);
        let effects = if self.ws.pinned_at(panel).is_some() {
            self.ws.unpin(panel, &info)?
        } else {
            self.ws.attach(panel, &info)?
        };
        self.apply(effects, reason);
        Ok(())
    }

    /// Dock back every panel of the float `float`.
    pub fn op_attach_float(&mut self, float: &str, reason: Reason) {
        let panels: Vec<PanelId> = self
            .ws
            .float(float)
            .map(|(_, f)| f.root.panels().into_iter().cloned().collect())
            .unwrap_or_default();
        for p in panels {
            let info = self.info(&p);
            if let Ok(effects) = self.ws.attach(&p, &info) {
                self.apply(effects, reason);
            }
        }
    }

    /// Float `panel` over its dock at `rect`, from the dock's top-left
    /// corner in logical pixels.
    pub fn op_float(&mut self, panel: &str, rect: Rect, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        if self.panels.get(panel).is_some_and(|r| !r.attrs.floatable) {
            return Err(DockError::NotFloatable(panel.to_string()));
        }
        let info = self.info(panel);
        if !self.ws.is_shown(panel) {
            self.op_show(panel, reason)?;
        }
        let dock = self
            .ws
            .find(panel)
            .and_then(|l| l.container.dock().map(str::to_string))
            .or_else(|| info.dock.clone())
            .or_else(|| self.docks.keys().next().cloned())
            .ok_or_else(|| DockError::Op(OpError::NotDockable(panel.to_string())))?;
        let rect = Rect {
            w: rect.w.max(1.0),
            h: rect.h.max(1.0),
            ..rect
        };
        // A panel alone in a float only moves.
        if let Some(loc) = self.ws.find(panel)
            && let lumen_dock_model::Container::Float { float, .. } = &loc.container
            && self
                .ws
                .float(float)
                .is_some_and(|(_, f)| f.root.panels().len() == 1)
        {
            self.ws.set_float_rect(float, rect);
            self.apply(Vec::new(), reason);
            return Ok(());
        }
        let effects = self.ws.float_panel(panel, &dock, rect, &info)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Pin `panel` to the `edge` of its dock.
    pub fn op_pin(&mut self, panel: &str, edge: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let side = Side::parse(edge).ok_or_else(|| DockError::BadEdge(edge.to_string()))?;
        let info = self.info(panel);
        let effects = self.ws.pin(panel, side, &info)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Dock back every panel of the window holding `panel`, or hide those
    /// that do not dock.
    pub fn op_attach_window(&mut self, window: &str, dock_back: bool, reason: Reason) {
        let panels = &self.panels;
        let docks = &self.docks;
        let info = |p: &str| {
            panels
                .get(p)
                .map(|r| r.info.clone())
                .unwrap_or(lumen_dock_model::PanelInfo {
                    dock: docks.keys().next().cloned(),
                    place: lumen_dock_model::Place::Center,
                    dockable: true,
                })
        };
        let effects = self.ws.close_window(window, dock_back, &info);
        self.apply(effects, reason);
    }

    /// Collapse or expand the stack holding `panel`.
    pub fn op_collapse(
        &mut self,
        panel: &str,
        collapsed: bool,
        reason: Reason,
    ) -> Result<(), DockError> {
        let effects = self.ws.collapse(panel, collapsed)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Fill `panel`'s dock with its stack.
    pub fn op_maximize(&mut self, panel: &str, reason: Reason) -> Result<(), DockError> {
        self.known(panel)?;
        let effects = self.ws.maximize(panel)?;
        self.apply(effects, reason);
        Ok(())
    }

    /// Give `dock` its whole layout back.
    pub fn op_restore_maximized(&mut self, dock: &str, reason: Reason) -> bool {
        let done = self.ws.restore_maximized(dock);
        if done {
            self.apply(Vec::new(), reason);
        }
        done
    }

    /// Drop everything remembered about `panel`.
    pub fn op_forget(&mut self, panel: &str, reason: Reason) -> bool {
        let known = self.ws.is_shown(panel) || self.ws.homes.contains_key(panel);
        let effects = self.ws.forget(panel);
        self.reported.remove(panel);
        if known {
            self.apply(effects, reason);
        }
        known
    }

    /// The window `panel` is in: 0 for the main window, the window's number,
    /// or -1 when it is not shown.
    pub fn window_of(&self, panel: &str) -> i64 {
        match self.ws.where_is(panel) {
            Where::Dock(_) | Where::Float(_) => 0,
            Where::Sidebar(_) if self.ws.in_view(panel) => 0,
            Where::Window(id) => window_number(&id),
            Where::Sidebar(_) | Where::Hidden => -1,
        }
    }

    /// The windows of their own, by number.
    pub fn windows(&self) -> Vec<i64> {
        self.ws
            .windows
            .iter()
            .filter(|w| {
                w.root
                    .panels()
                    .iter()
                    .any(|p| self.panels.get(*p).is_some_and(|r| r.entity.is_some()))
            })
            .map(|w| window_number(&w.id))
            .collect()
    }

    /// Every panel id the module knows.
    pub fn panel_ids(&self) -> Vec<PanelId> {
        let mut ids: BTreeSet<PanelId> = self.panels.keys().cloned().collect();
        ids.extend(self.ws.shown_panels());
        ids.into_iter().collect()
    }

    /// The live workspace, with each window's place as it is now.
    pub fn snapshot(&self) -> Workspace {
        let mut ws = self.ws.clone();
        ws.layout_version = self.layout_version;
        ws
    }

    /// Arrange everything as `next` says, fitted to the panels the app has.
    pub fn op_restore(&mut self, next: Workspace, reason: Reason) {
        let mut next = next;
        let known: BTreeSet<PanelId> = self
            .panels
            .iter()
            .filter(|(_, p)| p.entity.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        next.reconcile(&self.default, &known);
        next.layout_version = self.layout_version;
        let effects = self.ws.replace_with(next);
        self.layout_name = self.ws.name.clone();
        self.apply(effects, reason);
    }

    /// Arrange everything as the text from [`Workspace::to_json`] says.
    pub fn op_restore_json(&mut self, text: &str, reason: Reason) -> Result<(), DockError> {
        match Workspace::from_json(text, self.layout_version) {
            Ok(ws) => {
                self.op_restore(ws, reason);
                Ok(())
            }
            Err(e) => {
                self.errors.push((e.reason().to_string(), e.to_string()));
                self.dirty = true;
                Err(DockError::Restore(e))
            }
        }
    }

    /// The named layouts.
    pub fn layout_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = self.builtin.keys().cloned().collect();
        names.extend(self.user.keys().cloned());
        names.into_iter().collect()
    }

    /// Keep the arrangement as the named layout `name`.
    pub fn op_save_layout(&mut self, name: &str) {
        let mut ws = self.snapshot();
        ws.name = name.to_string();
        self.user.insert(name.to_string(), ws);
        self.layout_name = name.to_string();
        self.save_due = true;
        self.dirty = true;
    }

    /// Arrange everything as the named layout `name` says.
    pub fn op_apply_layout(&mut self, name: &str, reason: Reason) -> Result<(), DockError> {
        if let Some(user) = self.user.get(name).cloned() {
            self.op_restore(user, reason);
        } else if let Some(builtin) = self.builtin.get(name).cloned() {
            let default = self.default.clone();
            let effects = self.ws.apply_named(&builtin, &default);
            self.ws.name = name.to_string();
            self.apply(effects, reason);
        } else {
            return Err(DockError::UnknownLayout(name.to_string()));
        }
        self.layout_name = name.to_string();
        Ok(())
    }

    /// Delete the user's layout `name`.
    pub fn op_delete_layout(&mut self, name: &str) -> bool {
        let done = self.user.remove(name).is_some();
        if done {
            self.save_due = true;
        }
        done
    }

    /// Go back to the layout the arrangement came from, or to the markup's.
    pub fn op_reset(&mut self, reason: Reason) {
        let name = self.layout_name.clone();
        if !name.is_empty() && self.op_apply_layout(&name, reason).is_ok() {
            return;
        }
        let default = self.default.clone();
        self.op_restore(default, reason);
        self.layout_name.clear();
    }
}

/// Report a call that did nothing on stderr, once per distinct message, and
/// turn it into the call's result.
fn report<T>(what: &str, r: Result<T, DockError>, fallback: T) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            lumen_module::lumen_core::warn_line!("dock::{what}: {e}");
            fallback
        }
    }
}

/// One script function's work on the state, given its call.
type Op = Box<
    dyn Fn(&mut Core, &lumen_module::lumen_script::ScriptFnCx<'_>) -> ScriptValue + Send + Sync,
>;

/// The script functions, bound to `shared`, with the signatures the web
/// half declares.
pub fn script_fns(shared: &Shared) -> Result<Vec<ScriptFn>, String> {
    let s = shared.clone();
    let body = move |f: Op| -> ScriptFnBody {
        let s = s.clone();
        Arc::new(move |cx| {
            let mut core = s.lock();
            Ok(f(&mut core, cx))
        })
    };
    let b = |v: bool| ScriptValue::Bool(v);
    let api = Reason::Api;
    let bodies: Vec<(String, ScriptFnBody)> = vec![
        (
            "show".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "show",
                    c.op_show(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "hide".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "hide",
                    c.op_hide(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "toggle".into(),
            body(Box::new(move |c, cx| {
                b(report("toggle", c.op_toggle(&cx.str_arg(0), api), false))
            })),
        ),
        (
            "focus".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "focus",
                    c.op_focus(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "activate".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "activate",
                    c.op_activate(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "close".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "close",
                    c.op_close(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "move".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "move",
                    c.op_move(&cx.str_arg(0), &cx.str_arg(1), &cx.str_arg(2), api)
                        .map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "detach".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "detach",
                    c.op_detach(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "detach_group".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "detach_group",
                    c.op_detach_group(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "float".into(),
            body(Box::new(move |c, cx| {
                let rect = Rect {
                    x: cx.float_arg(1) as f32,
                    y: cx.float_arg(2) as f32,
                    w: cx.float_arg(3) as f32,
                    h: cx.float_arg(4) as f32,
                };
                b(report(
                    "float",
                    c.op_float(&cx.str_arg(0), rect, api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "pin".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "pin",
                    c.op_pin(&cx.str_arg(0), &cx.str_arg(1), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "attach".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "attach",
                    c.op_attach(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "window_of".into(),
            body(Box::new(|c, cx| {
                ScriptValue::I64(c.window_of(&cx.str_arg(0)))
            })),
        ),
        (
            "windows".into(),
            body(Box::new(|c, _| {
                ScriptValue::Array(c.windows().into_iter().map(ScriptValue::I64).collect())
            })),
        ),
        (
            "collapse".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "collapse",
                    c.op_collapse(&cx.str_arg(0), cx.bool_arg(1), api)
                        .map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "maximize".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "maximize",
                    c.op_maximize(&cx.str_arg(0), api).map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "restore_maximized".into(),
            body(Box::new(move |c, cx| {
                b(c.op_restore_maximized(&cx.str_arg(0), api))
            })),
        ),
        (
            "panels".into(),
            body(Box::new(|c, _| {
                ScriptValue::Array(c.panel_ids().into_iter().map(ScriptValue::Str).collect())
            })),
        ),
        (
            "is_shown".into(),
            body(Box::new(|c, cx| {
                ScriptValue::Bool(c.ws.in_view(&cx.str_arg(0)))
            })),
        ),
        (
            "save".into(),
            body(Box::new(|c, _| ScriptValue::Str(c.snapshot().to_json()))),
        ),
        (
            "restore".into(),
            body(Box::new(move |c, cx| {
                b(report(
                    "restore",
                    c.op_restore_json(&cx.str_arg(0), Reason::Layout)
                        .map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "layouts".into(),
            body(Box::new(|c, _| {
                ScriptValue::Array(c.layout_names().into_iter().map(ScriptValue::Str).collect())
            })),
        ),
        (
            "save_layout".into(),
            body(Box::new(|c, cx| {
                c.op_save_layout(&cx.str_arg(0));
                ScriptValue::Bool(true)
            })),
        ),
        (
            "apply_layout".into(),
            body(Box::new(|c, cx| {
                ScriptValue::Bool(report(
                    "apply_layout",
                    c.op_apply_layout(&cx.str_arg(0), Reason::Layout)
                        .map(|()| true),
                    false,
                ))
            })),
        ),
        (
            "delete_layout".into(),
            body(Box::new(|c, cx| {
                ScriptValue::Bool(c.op_delete_layout(&cx.str_arg(0)))
            })),
        ),
        (
            "reset".into(),
            body(Box::new(|c, _| {
                c.op_reset(Reason::Layout);
                ScriptValue::Bool(true)
            })),
        ),
        (
            "forget".into(),
            body(Box::new(move |c, cx| b(c.op_forget(&cx.str_arg(0), api)))),
        ),
    ];
    lumen_module::web_half_fns(crate::plugin::NAME, crate::plugin::DESCRIPTOR, bodies)
}

/// The dock from a Rust system: the same operations the `dock` script
/// namespace has, on the app's one workspace.
///
/// ```ignore
/// fn open_console(dock: Dock) {
///     dock.show("console").ok();
/// }
/// ```
#[derive(SystemParam)]
pub struct Dock<'w> {
    shared: Res<'w, Shared>,
}

impl Dock<'_> {
    /// Bring `panel` into view.
    ///
    /// # Errors
    ///
    /// No panel has the id, or the model refused.
    pub fn show(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_show(panel, Reason::Api)
    }

    /// Take `panel` out of view.
    ///
    /// # Errors
    ///
    /// The panel is not shown.
    pub fn hide(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_hide(panel, Reason::Api)
    }

    /// Show `panel` if hidden, hide it if shown; whether it is shown after.
    ///
    /// # Errors
    ///
    /// No panel has the id.
    pub fn toggle(&self, panel: &str) -> Result<bool, DockError> {
        self.shared.lock().op_toggle(panel, Reason::Api)
    }

    /// Show `panel` and move keyboard focus into it.
    ///
    /// # Errors
    ///
    /// No panel has the id.
    pub fn focus(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_focus(panel, Reason::Api)
    }

    /// Make `panel` its stack's showing tab.
    ///
    /// # Errors
    ///
    /// The panel is not shown.
    pub fn activate(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_activate(panel, Reason::Api)
    }

    /// Close `panel` as its close button does, without asking.
    ///
    /// # Errors
    ///
    /// No panel has the id.
    pub fn close(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_close(panel, Reason::Api)
    }

    /// Move `panel` beside `target` (a panel or a dock id) into `zone`:
    /// `tab`, `left`, `right`, `top` or `bottom`.
    ///
    /// # Errors
    ///
    /// An unknown panel, target or zone, or the model refused.
    pub fn move_to(&self, panel: &str, target: &str, zone: &str) -> Result<(), DockError> {
        self.shared.lock().op_move(panel, target, zone, Reason::Api)
    }

    /// Pop `panel` out into a window of its own.
    ///
    /// # Errors
    ///
    /// No panel has the id.
    pub fn detach(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_detach(panel, Reason::Api)
    }

    /// Pop the stack holding `panel` out into a window of its own.
    ///
    /// # Errors
    ///
    /// The panel is not shown.
    pub fn detach_group(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_detach_group(panel, Reason::Api)
    }

    /// Dock `panel` back from its window, its float or its sidebar.
    ///
    /// # Errors
    ///
    /// The panel is in a dock already, or does not dock.
    pub fn attach(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_attach(panel, Reason::Api)
    }

    /// Float `panel` over its dock at `rect`, from the dock's top-left
    /// corner in logical pixels.
    ///
    /// # Errors
    ///
    /// No panel has the id, or it does not float.
    pub fn float(&self, panel: &str, rect: Rect) -> Result<(), DockError> {
        self.shared.lock().op_float(panel, rect, Reason::Api)
    }

    /// Pin `panel` to `edge` of its dock: `left`, `right`, `top` or
    /// `bottom`.
    ///
    /// # Errors
    ///
    /// No panel has the id, the edge is not one, or the panel does not
    /// dock.
    pub fn pin(&self, panel: &str, edge: &str) -> Result<(), DockError> {
        self.shared.lock().op_pin(panel, edge, Reason::Api)
    }

    /// Collapse or expand the stack holding `panel`.
    ///
    /// # Errors
    ///
    /// No panel or stack has the id.
    pub fn collapse(&self, panel: &str, collapsed: bool) -> Result<(), DockError> {
        self.shared
            .lock()
            .op_collapse(panel, collapsed, Reason::Api)
    }

    /// Fill `panel`'s dock with its stack.
    ///
    /// # Errors
    ///
    /// The panel is not in a dock.
    pub fn maximize(&self, panel: &str) -> Result<(), DockError> {
        self.shared.lock().op_maximize(panel, Reason::Api)
    }

    /// Give `dock` its whole layout back; whether anything was maximized.
    pub fn restore_maximized(&self, dock: &str) -> bool {
        self.shared.lock().op_restore_maximized(dock, Reason::Api)
    }

    /// Whether `panel` is in view: in a dock, a float or a window, or slid
    /// out of its sidebar.
    pub fn is_shown(&self, panel: &str) -> bool {
        self.shared.lock().ws.in_view(panel)
    }

    /// Every panel id the module knows.
    pub fn panels(&self) -> Vec<String> {
        self.shared.lock().panel_ids()
    }

    /// The window `panel` is in: 0 for the main window, else the window's
    /// number; -1 when it is not shown.
    pub fn window_of(&self, panel: &str) -> i64 {
        self.shared.lock().window_of(panel)
    }

    /// The whole arrangement, as a value to keep, edit or restore.
    pub fn save(&self) -> Workspace {
        self.shared.lock().snapshot()
    }

    /// Arrange everything as `layout` says, fitted to the panels the app
    /// has now.
    pub fn restore(&self, layout: &Workspace) {
        self.shared
            .lock()
            .op_restore(layout.clone(), Reason::Layout);
    }

    /// Arrange everything as the text from [`Workspace::to_json`] says.
    ///
    /// # Errors
    ///
    /// The text is not a layout this app can use; a `layouterror` event
    /// says why.
    pub fn restore_json(&self, text: &str) -> Result<(), DockError> {
        self.shared.lock().op_restore_json(text, Reason::Layout)
    }

    /// The named layouts.
    pub fn layouts(&self) -> Vec<String> {
        self.shared.lock().layout_names()
    }

    /// Keep the arrangement as the named layout `name`.
    pub fn save_layout(&self, name: &str) {
        self.shared.lock().op_save_layout(name);
    }

    /// Arrange everything as the named layout `name` says.
    ///
    /// # Errors
    ///
    /// No layout has the name.
    pub fn apply_layout(&self, name: &str) -> Result<(), DockError> {
        self.shared.lock().op_apply_layout(name, Reason::Layout)
    }

    /// Delete the user's layout `name`; whether there was one.
    pub fn delete_layout(&self, name: &str) -> bool {
        self.shared.lock().op_delete_layout(name)
    }

    /// Go back to the layout the arrangement came from, or the markup's.
    pub fn reset(&self) {
        self.shared.lock().op_reset(Reason::Layout);
    }

    /// Drop everything remembered about `panel`; whether anything was.
    pub fn forget(&self, panel: &str) -> bool {
        self.shared.lock().op_forget(panel, Reason::Api)
    }
}
