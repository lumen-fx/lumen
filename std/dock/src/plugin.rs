//! The plugin: its configuration, the systems that read what the user does,
//! and the events and signals that tell the app what changed.

use std::collections::BTreeSet;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_dock_model::{Effect, Where};
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::components::{A11yLabel, A11yRole, LumenClasses, StyleDirty};
use lumen_module::lumen_core::input::{
    ClickEvent, DoubleClickEvent, FocusTracker, PointerButton, WindowFocused,
};
use lumen_module::lumen_core::prelude::TickStage;
use lumen_module::lumen_core::property_store::PropertyStore;
use lumen_module::lumen_core::window::{Window, WindowClosing, WindowRuntime};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element};
use lumen_module::lumen_primitives::{SplitResized, TabStripButton, Tabs, TabsSet};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};
use lumen_module::lumen_script::{CustomEvent, CustomEventDelivered, ScriptFnAppExt, ScriptSet};

use crate::adopt::{
    Arrivals, DockChrome, DockPanel, TAGS, adopt_elements, arrivals_pending, install_observers,
};
use crate::keys::{Keys, dock_keys};
use crate::persist::{Persist, save_if_due};
use crate::realize::{Built, DockStack, DockTab, DockWindow, realize};
use crate::state::{Reason, Shared};

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-dock";

/// The web half's descriptor, which declares this module's functions.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The default look, at user-agent origin.
const UA_CSS: &str = include_str!("../dock.css");

/// The default strings, under the app's own catalogue.
const MESSAGES_EN: &str = include_str!("../locale/en-US.ftl");

/// What closing a window of panels does with them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowClose {
    /// Panels that dock go back to where they came from; the rest hide.
    #[default]
    Dock,
    /// Every panel hides.
    Hide,
}

/// Docks and windows: `<dock>` and `<dock-panel>` elements, the `dock`
/// script namespace, and the layout kept between runs.
///
/// Ships as the bundled `lumen-dock` runtime module (an app declares
/// `lumen-dock = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin.
#[derive(Clone, Debug, Default)]
pub struct DockPlugin {
    /// Whether the arrangement is kept between runs.
    pub persist: Persist,
    /// The app's layout version: a kept arrangement saved under another one
    /// is set aside for the markup's layout.
    pub layout_version: u32,
    /// What closing a window of panels does with them.
    pub window_close: WindowClose,
    /// The keys the module answers.
    pub keys: Keys,
}

impl DockPlugin {
    /// Build from the module's `config` table: `persist` (`"none"` or
    /// `"auto"`), `layout-version`, `window-close` (`"dock"` or `"hide"`)
    /// and `keys`.
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        let mut plugin = Self::default();
        if config.str("persist") == Some("auto") {
            plugin.persist = Persist::Auto;
        }
        if let Some(v) = config
            .int("layout-version")
            .and_then(|v| u32::try_from(v).ok())
        {
            plugin.layout_version = v;
        }
        if config.str("window-close") == Some("hide") {
            plugin.window_close = WindowClose::Hide;
        }
        if let Some(keys) = config.table().get("keys") {
            plugin.keys = Keys::from_config(keys);
        }
        plugin
    }
}

/// The module's settings, for its systems.
#[derive(Resource, Clone, Debug)]
pub struct Settings {
    /// What closing a window does with its panels.
    pub window_close: WindowClose,
}

/// System sets the module's systems run in.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockSet {
    /// What the user did: picks, closes, keys, closed windows.
    Input,
    /// New and removed elements are read.
    Adopt,
    /// The elements follow the model.
    Realize,
    /// The events and signals that report what changed.
    Report,
}

/// Something the dock did, for a Rust system that prefers messages to
/// element events. Written beside each element event.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub enum DockEvent {
    /// A panel entered view.
    Shown(String),
    /// A panel left view.
    Hidden(String),
    /// A panel became its stack's showing tab.
    Activated(String),
    /// A panel moved between docks and windows: the panel, and where it went
    /// (`dock`, `window` or `hidden`).
    Moved(String, String),
    /// The arrangement changed, and why: `user`, `api` or `layout`.
    LayoutChanged(String),
}

/// Register message `T` unless something already did.
fn message<T: bevy_ecs::message::Message>(world: &mut World) {
    if !world.contains_resource::<bevy_ecs::message::Messages<T>>() {
        bevy_ecs::message::MessageRegistry::register_message::<T>(world);
    }
}

impl Plugin for DockPlugin {
    fn build(self, app: &mut App) {
        for tag in TAGS {
            lumen_module::lumen_widget::register_widget_tag_owned(tag);
        }
        let shared = Shared::default();
        {
            let mut core = shared.lock();
            core.layout_version = self.layout_version;
            core.ws.layout_version = self.layout_version;
            core.restore_due = true;
        }
        app.world.insert_resource(shared.clone());
        app.world.insert_resource(self.persist);
        app.world.insert_resource(self.keys.clone());
        app.world.insert_resource(Settings {
            window_close: self.window_close,
        });
        app.world.init_resource::<Arrivals>();
        app.world.init_resource::<Built>();
        app.add_ua_stylesheet(NAME, UA_CSS);
        app.add_default_messages("en-US", MESSAGES_EN);
        message::<DockEvent>(&mut app.world);
        install_observers(&mut app.world);
        match crate::api::script_fns(&shared) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(e) => lumen_module::lumen_core::warn_line!("lumen-dock: {e}"),
        }
        app.add_systems(
            TickStage::Systems,
            (ensure_messages, user_input, dock_keys)
                .chain()
                .in_set(DockSet::Input)
                .after(ScriptSet::DomState),
        );
        // Elements read twice a tick: before any handler runs, so a script's
        // `on_ready` finds the panels the markup declared, and after the DOM
        // commands, so a panel a script appended is placed on the same tick.
        app.add_systems(
            TickStage::Systems,
            adopt_elements
                .run_if(arrivals_pending)
                .before(ScriptSet::Ready)
                .before(ScriptSet::Dispatch)
                .before(ScriptSet::DomInput)
                .before(ScriptSet::DomState),
        );
        app.add_systems(
            TickStage::Systems,
            adopt_elements
                .run_if(arrivals_pending)
                .in_set(DockSet::Adopt)
                .after(DockSet::Input)
                .after(lumen_module::lumen_scene::dom::apply_dom_commands),
        );
        app.add_systems(
            TickStage::Systems,
            realize
                .in_set(DockSet::Realize)
                .after(DockSet::Adopt)
                .before(lumen_module::lumen_scene::widgets::keep_widget_parts),
        );
        app.add_systems(
            TickStage::Systems,
            (report, track_windows, save_if_due)
                .chain()
                .in_set(DockSet::Report)
                .after(DockSet::Realize),
        );
        // After the tab sets show what the realization picked, so focus can
        // land in a panel that just became its stack's showing tab.
        app.add_systems(
            TickStage::Systems,
            (decorate_tabs, crate::keys::focus_requests)
                .chain()
                .after(TabsSet)
                .after(DockSet::Report),
        );
    }
}

/// Register, once, the messages the module reads that the runtime's own
/// plugins normally register. Their plugins install after the module's, and
/// a message registered twice drops what was written before it is read, so
/// the module only fills the ones an app assembled without them lacks.
fn ensure_messages(world: &mut World, mut done: Local<bool>) {
    if std::mem::replace(&mut *done, true) {
        return;
    }
    message::<CustomEvent>(world);
    message::<CustomEventDelivered>(world);
    message::<WindowClosing>(world);
    message::<WindowFocused>(world);
    message::<ClickEvent>(world);
    message::<DoubleClickEvent>(world);
    message::<SplitResized>(world);
    message::<lumen_module::lumen_core::input::KeyPressed>(world);
}

/// The panel a strip button shows, through the tab it stands for.
fn panel_of_button(
    button: Entity,
    buttons: &Query<&TabStripButton>,
    tabs: &Query<&DockTab>,
) -> Option<String> {
    let tab = buttons.get(button).ok()?.tab;
    tabs.get(tab).ok().map(|t| t.panel.clone())
}

/// On a tab's close button: the panel it closes.
#[derive(Component, Clone, Debug)]
pub struct DockCloseButton {
    /// The panel.
    pub panel: String,
}

/// Read what the user did to the dock this tick: a tab picked, a close
/// button or a middle click, a double click on a window's strip, a split
/// resized, a window closed, a panel close a handler allowed.
#[allow(clippy::too_many_arguments)]
fn user_input(
    shared: Res<Shared>,
    settings: Res<Settings>,
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    mut resized: MessageReader<SplitResized>,
    mut closing: MessageReader<WindowClosing>,
    mut delivered: MessageReader<CustomEventDelivered>,
    mut events: MessageWriter<CustomEvent>,
    picked: Query<(&DockStack, &Tabs), Changed<Tabs>>,
    (dock_tabs, buttons, close_buttons, parents): (
        Query<&DockTab>,
        Query<&TabStripButton>,
        Query<&DockCloseButton>,
        Query<&ChildOf>,
    ),
    windows: Query<&DockWindow>,
    built: Res<Built>,
) {
    let mut core = shared.lock();
    // A tab the user picked becomes the stack's showing panel.
    for (stack, tabs) in &picked {
        let Some(panel) = tabs.shown.and_then(|t| dock_tabs.get(t).ok()) else {
            continue;
        };
        let active = core.ws.stack(&stack.id).and_then(|s| s.active.clone());
        if active.as_deref() != Some(panel.panel.as_str())
            && let Ok(effects) = core.ws.activate(&panel.panel)
        {
            core.apply(effects, Reason::User);
            // The elements already show it; only the events are owed.
            core.dirty = true;
        }
    }
    // A close button, or a middle click on a tab, asks to close the panel.
    for click in clicks.read() {
        let panel = match close_buttons.get(click.entity) {
            Ok(b) => Some(b.panel.clone()),
            Err(_) if click.button == PointerButton::Middle => {
                let mut cur = Some(click.entity);
                let mut found = None;
                while let Some(e) = cur {
                    if let Some(p) = panel_of_button(e, &buttons, &dock_tabs) {
                        found = Some(p);
                        break;
                    }
                    cur = parents.get(e).ok().map(ChildOf::parent);
                }
                found
            }
            Err(_) => None,
        };
        let Some(panel) = panel else { continue };
        let Some(rec) = core.panels.get(&panel) else {
            continue;
        };
        if !rec.attrs.closable {
            continue;
        }
        if let Some(e) = rec.entity {
            let mut ask = CustomEvent::new(e, "panelclose").with("reason", "user");
            ask.cancelable = true;
            events.write(ask);
        }
    }
    // A close no handler refused goes ahead.
    for answer in delivered.read() {
        if answer.event.name != "panelclose" || answer.default_prevented {
            continue;
        }
        if let Some(panel) = core.panel_of(answer.event.target).cloned() {
            let _ = core.op_close(&panel, Reason::User);
        }
    }
    // A double click on a window's strip docks the window back.
    for double in doubles.read() {
        let mut cur = Some(double.entity);
        while let Some(e) = cur {
            if let Some(panel) = panel_of_button(e, &buttons, &dock_tabs)
                && let Where::Window(w) = core.ws.where_is(&panel)
            {
                core.op_attach_window(&w, true, Reason::User);
                break;
            }
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
    }
    // A split the user resized keeps its sizes in the model.
    for r in resized.read() {
        let Some(id) = built
            .nodes
            .iter()
            .find(|(_, e)| **e == r.split)
            .map(|(id, _)| id.clone())
        else {
            continue;
        };
        if core.ws.resize(&id, &r.sizes) {
            core.apply(Vec::new(), Reason::User);
            // The split holds the sizes already.
        }
    }
    // A window of panels the user closed: its panels go back, or hide.
    for c in closing.read() {
        if let Ok(w) = windows.get(c.window) {
            let back = settings.window_close == WindowClose::Dock;
            core.op_attach_window(&w.id, back, Reason::User);
        }
    }
}

/// Raise the events and set the signals for what changed since the last
/// tick that did.
#[allow(clippy::too_many_arguments)]
fn report(
    shared: Res<Shared>,
    mut events: MessageWriter<CustomEvent>,
    mut messages: MessageWriter<DockEvent>,
    mut store: ResMut<PropertyStore>,
    focus: Res<FocusTracker>,
    mut focused_window: MessageReader<WindowFocused>,
    panels_q: Query<&DockPanel>,
    parents: Query<&ChildOf>,
    windows: Query<&DockWindow>,
    mut last_focus: Local<Option<String>>,
) {
    let mut core = shared.lock();
    let pending = std::mem::take(&mut core.pending);
    let changed = core.changed.take();
    let errors = std::mem::take(&mut core.errors);
    let entity = |core: &crate::state::Core, p: &str| core.panels.get(p).and_then(|r| r.entity);
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for pending in pending {
        let reason = pending.reason.as_str();
        match pending.effect {
            Effect::Shown(p) => {
                touched.insert(p.clone());
                if let Some(e) = entity(&core, &p) {
                    events.write(CustomEvent::new(e, "panelshow").with("reason", reason));
                }
                messages.write(DockEvent::Shown(p));
            }
            Effect::Hidden(p) => {
                touched.insert(p.clone());
                if let Some(e) = entity(&core, &p) {
                    events.write(CustomEvent::new(e, "panelhide").with("reason", reason));
                }
                messages.write(DockEvent::Hidden(p));
            }
            Effect::Activated(p) => {
                if let Some(e) = entity(&core, &p) {
                    events.write(CustomEvent::new(e, "panelactivate").with("reason", reason));
                }
                messages.write(DockEvent::Activated(p));
            }
            Effect::Moved { panel, from, to } => {
                touched.insert(panel.clone());
                if let Some(e) = entity(&core, &panel) {
                    events.write(
                        CustomEvent::new(e, "panelmove")
                            .with("from", from.kind())
                            .with("to", to.kind())
                            .with("reason", reason),
                    );
                }
                messages.write(DockEvent::Moved(panel, to.kind().to_string()));
            }
            Effect::WindowOpened(_) | Effect::WindowClosed(_) => {}
        }
    }
    let docks: Vec<Entity> = core.docks.values().map(|d| d.entity).collect();
    if let Some(reason) = changed {
        for &d in &docks {
            events.write(CustomEvent::new(d, "layoutchange").with("reason", reason.as_str()));
        }
        messages.write(DockEvent::LayoutChanged(reason.as_str().to_string()));
        if store.get_global_str("dock.layout").map(|s| s.to_string())
            != Some(core.layout_name.clone())
        {
            store.set_global_str("dock.layout", core.layout_name.clone());
        }
    }
    for (reason, detail) in errors {
        lumen_module::lumen_core::warn_line!("lumen-dock: a layout was not used: {detail}");
        for &d in &docks {
            events.write(
                CustomEvent::new(d, "layouterror")
                    .with("reason", reason.clone())
                    .with("detail", detail.clone()),
            );
        }
    }
    // A layout naming a panel the app does not have asks for it once.
    let missing: Vec<String> = core
        .ws
        .shown_panels()
        .into_iter()
        .chain(core.ws.homes.keys().cloned())
        .filter(|p| core.panels.get(p).is_none_or(|r| r.entity.is_none()))
        .filter(|p| !core.reported.contains(p))
        .collect();
    for p in missing {
        let meta = core.ws.panels.get(&p).cloned().unwrap_or_default();
        for &d in &docks {
            events.write(
                CustomEvent::new(d, "panelrestore")
                    .with("panel", p.clone())
                    .with("kind", meta.kind.clone())
                    .with("state", meta.state.clone()),
            );
        }
        core.reported.insert(p);
    }
    // `dock.shown.<panel>` for each panel whose view changed, and for every
    // panel the first time.
    let ids: Vec<String> = core.panels.keys().cloned().collect();
    for p in ids {
        let name = format!("dock.shown.{p}");
        let shown = core.ws.is_shown(&p);
        if touched.contains(&p) || store.get_global_str(&name).is_none() {
            store.set_global_bool(&name, shown);
        }
    }
    // The panel holding focus: `dock.active` and `panelfocus`.
    if focus.is_changed() {
        let mut cur = focus.0;
        let mut panel = None;
        while let Some(e) = cur {
            if let Ok(p) = panels_q.get(e) {
                panel = Some((e, p.id.clone()));
                break;
            }
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
        let id = panel.as_ref().map(|(_, id)| id.clone());
        if id != *last_focus {
            if let Some((e, p)) = &panel {
                events.write(CustomEvent::new(*e, "panelfocus"));
                core.last_focused
                    .insert(p.clone(), focus.0.expect("found under it"));
            }
            store.set_global_str("dock.active", id.clone().unwrap_or_default());
            *last_focus = id;
        } else if let (Some((_, p)), Some(f)) = (&panel, focus.0) {
            core.last_focused.insert(p.clone(), f);
        }
    }
    // A window gaining or losing focus tells each panel in it.
    for f in focused_window.read() {
        let name = if f.focused {
            "windowfocus"
        } else {
            "windowblur"
        };
        let panels: Vec<String> = match windows.get(f.window) {
            Ok(w) => core
                .ws
                .window(&w.id)
                .map(|w| w.root.panels().into_iter().cloned().collect())
                .unwrap_or_default(),
            Err(_) => core
                .ws
                .docks
                .values()
                .flat_map(|d| d.root.panels().into_iter().cloned().collect::<Vec<_>>())
                .collect(),
        };
        for p in panels {
            if let Some(e) = entity(&core, &p) {
                let mut ev = CustomEvent::new(e, name);
                ev.bubbles = false;
                events.write(ev);
            }
        }
    }
}

/// Keep each window's place in the model as the platform reports it, so a
/// save writes where the window is.
fn track_windows(
    shared: Res<Shared>,
    windows: Query<
        (&DockWindow, &Window, &WindowRuntime),
        Or<(Changed<WindowRuntime>, Changed<Window>)>,
    >,
    monitors: Option<Res<lumen_module::lumen_core::window::Monitors>>,
) {
    if windows.is_empty() {
        return;
    }
    let monitors: Vec<lumen_dock_model::Monitor> = monitors
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
        .unwrap_or_default();
    let mut core = shared.lock();
    for (dw, window, runtime) in &windows {
        let Some(layout) = core.ws.windows.iter_mut().find(|w| w.id == dw.id) else {
            continue;
        };
        let previous = Some((layout.place.rect, layout.place.monitor.clone()));
        let (rect, key) = lumen_dock_model::record(
            runtime.position.map(|p| [p.x, p.y]),
            [window.size.x, window.size.y],
            &monitors,
            previous,
        );
        let place = lumen_dock_model::WindowPlace {
            rect: Some(rect),
            monitor: key,
            maximized: runtime.maximized,
        };
        if layout.place != place {
            layout.place = place;
            core.save_due = true;
        }
    }
}

/// Give each new tab of a closable panel its close button, labelled for
/// assistive technology, and mark the strip buttons the dock's.
fn decorate_tabs(
    mut commands: Commands,
    new: Query<(Entity, &TabStripButton), Added<TabStripButton>>,
    tabs: Query<&DockTab>,
    shared: Res<Shared>,
) {
    if new.is_empty() {
        return;
    }
    let core = shared.lock();
    let close = lumen_module::lumen_core::i18n::try_translate("lumen-dock-close")
        .unwrap_or_else(|| "Close".to_string());
    for (button, b) in &new {
        let Ok(tab) = tabs.get(b.tab) else { continue };
        let Some(rec) = core.panels.get(&tab.panel) else {
            continue;
        };
        commands.entity(button).insert(DockTabButton);
        let classes = vec!["dock-tab".to_string()];
        let closable = rec.attrs.closable;
        let label = format!("{close} {}", rec.attrs.title);
        let panel = tab.panel.clone();
        commands.queue(move |world: &mut World| {
            if let Ok(mut e) = world.get_entity_mut(button) {
                let mut cl = e.take::<LumenClasses>().unwrap_or_default();
                for c in &classes {
                    if !cl.has(c) {
                        cl.0.push(c.clone().into());
                    }
                }
                e.insert((cl, StyleDirty));
            }
            if !closable {
                return;
            }
            let el = Element {
                tag: "button".into(),
                attrs: Box::new(Attributes {
                    classes: vec!["dock-tab-close".into()],
                    tab_index: Some(-1),
                    ..Attributes::default()
                }),
                ..Element::default()
            };
            let x = spawn_subtree(world, &el, Some(button), Placeholders::Resolved);
            world.entity_mut(x).insert((
                DockChrome,
                lumen_module::lumen_core::components::DomHidden,
                StyleDirty,
                A11yRole::Button,
                A11yLabel(label),
                DockCloseButton { panel },
            ));
        });
    }
}

/// On a strip button that shows one of the dock's tabs.
#[derive(Component, Clone, Copy, Debug)]
pub struct DockTabButton;
