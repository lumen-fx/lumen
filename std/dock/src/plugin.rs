//! The plugin: its configuration, the systems that read what the user does,
//! and the events and signals that tell the app what changed.

use std::collections::BTreeSet;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_dock_model::{Effect, Where};
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::components::{
    A11yAnnouncementQueue, A11yLabel, A11yLive, A11yRole, LumenAttributes, LumenClasses, StyleDirty,
};
use lumen_module::lumen_core::input::{
    ClickEvent, ContextMenuEvent, DoubleClickEvent, FocusTracker, PointerButton, WindowFocused,
};
use lumen_module::lumen_core::prelude::TickStage;
use lumen_module::lumen_core::property_store::PropertyStore;
use lumen_module::lumen_core::window::{PrimaryWindow, Window, WindowClosing, WindowRuntime};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element};
use lumen_module::lumen_primitives::{SplitResized, TabStripButton, Tabs, TabsSet};
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};
use lumen_module::lumen_script::{CustomEvent, CustomEventDelivered, ScriptFnAppExt, ScriptSet};

use crate::adopt::{
    Arrivals, DockChrome, DockPanel, TAGS, TitleSignals, adopt_elements, arrivals_pending,
    install_observers,
};
use crate::keys::Keys;
use crate::menu::{DockMenuItem, MenuAction, TabMenu};
use crate::persist::{Persist, save_if_due};
use crate::realize::{Built, DockStack, DockTab, DockWindow, realize};
use crate::state::{PanelAttrs, Reason, Shared, panel_info};

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-dock";

/// The web half's descriptor, which declares this module's functions.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The default look, at user-agent origin.
const UA_CSS: &str = include_str!("../dock.css");

/// The default strings, under the app's own catalogue.
const MESSAGES_EN: &str = include_str!("../locale/en-US.ftl");

/// The module's message `key` in the app's language, else `fallback`.
pub(crate) fn text(key: &str, fallback: &str) -> String {
    lumen_module::lumen_core::i18n::try_translate(key).unwrap_or_else(|| fallback.to_string())
}

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
        app.world.init_resource::<TabMenu>();
        app.world.init_resource::<TitleSignals>();
        app.world.init_resource::<A11yAnnouncementQueue>();
        app.add_ua_stylesheet(NAME, UA_CSS);
        app.add_default_messages("en-US", MESSAGES_EN);
        // Registered here, ahead of the runtime's own plugins, for an app
        // assembled without them; registering a message twice is a no-op.
        message::<DockEvent>(&mut app.world);
        message::<CustomEvent>(&mut app.world);
        message::<CustomEventDelivered>(&mut app.world);
        message::<WindowClosing>(&mut app.world);
        message::<WindowFocused>(&mut app.world);
        message::<ClickEvent>(&mut app.world);
        message::<DoubleClickEvent>(&mut app.world);
        message::<ContextMenuEvent>(&mut app.world);
        message::<SplitResized>(&mut app.world);
        message::<lumen_module::lumen_core::input::KeyPressed>(&mut app.world);
        install_observers(&mut app.world);
        match crate::api::script_fns(&shared) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(e) => lumen_module::lumen_core::warn_line!("lumen-dock: {e}"),
        }
        // The module's world-wide work (reading new elements, making the
        // elements follow the model, moving focus, the keys) runs as queued
        // commands that a cheap system adds only when there is work, so an
        // idle tick runs no exclusive system of the module's and the
        // schedule keeps its parallelism.
        use bevy_ecs::schedule::common_conditions::{
            any_match_filter, any_with_component, on_message, resource_changed,
        };
        use lumen_module::lumen_core::input::KeyPressed;
        // Each system carries the condition under which it has work, so an
        // idle tick dispatches none of them.
        let input_due = on_message::<ClickEvent>
            .or_eager(on_message::<DoubleClickEvent>)
            .or_eager(on_message::<SplitResized>)
            .or_eager(on_message::<WindowClosing>)
            .or_eager(on_message::<CustomEventDelivered>)
            .or_eager(any_match_filter::<(With<DockStack>, Changed<Tabs>)>);
        app.add_systems(
            TickStage::Systems,
            (
                user_input.run_if(input_due),
                keys_gate.run_if(on_message::<KeyPressed>),
                tab_menu_gate.run_if(on_message::<ContextMenuEvent>),
            )
                .chain()
                .in_set(DockSet::Input)
                .after(ScriptSet::DomState),
        );
        // A panel's attributes or its bound window title changed, by a
        // script or the DOM commands: the elements follow on the same tick.
        app.add_systems(
            TickStage::Systems,
            (
                follow_panel_attrs
                    .run_if(any_match_filter::<(With<DockPanel>, Changed<LumenAttributes>)>),
                follow_title_signals.run_if(
                    (|t: Res<TitleSignals>| !t.0.is_empty())
                        .and_then(resource_changed::<PropertyStore>),
                ),
            )
                .after(DockSet::Input)
                .after(lumen_module::lumen_scene::dom::apply_dom_commands)
                .before(DockSet::Realize),
        );
        // Elements read twice a tick: before any handler runs, so a script's
        // `on_ready` finds the panels the markup declared, and after the DOM
        // commands, so a panel a script appended is placed on the same tick.
        // The work itself runs as queued commands, so a tick with none runs no
        // exclusive system of the module's.
        app.add_systems(
            TickStage::Systems,
            adopt_gate
                .run_if(arrivals_pending)
                .before(ScriptSet::Ready)
                .before(ScriptSet::Dispatch)
                .before(ScriptSet::DomInput)
                .before(ScriptSet::DomState),
        );
        app.add_systems(
            TickStage::Systems,
            realize_gate
                .run_if(|a: Res<Arrivals>, shared: Res<Shared>| {
                    arrivals_pending(a) || shared.lock().dirty
                })
                .in_set(DockSet::Realize)
                .after(DockSet::Input)
                .after(lumen_module::lumen_scene::dom::apply_dom_commands)
                .before(lumen_module::lumen_scene::widgets::keep_widget_parts),
        );
        app.add_systems(
            TickStage::Systems,
            (
                report.run_if(
                    (|shared: Res<Shared>| shared.lock().report_due)
                        .or_eager(resource_changed::<FocusTracker>)
                        .or_eager(on_message::<WindowFocused>),
                ),
                track_windows.run_if(any_with_component::<DockWindow>),
                show_with_the_primary.run_if(any_with_component::<DockWindow>),
                save_if_due
                    .run_if(|p: Option<Res<Persist>>| p.is_some_and(|p| *p == Persist::Auto)),
            )
                .chain()
                .in_set(DockSet::Report)
                .after(DockSet::Realize)
                .after(lumen_module::lumen_scene::widgets::keep_widget_parts),
        );
        // After the tab sets show what the realization picked, so focus can
        // land in a panel that just became its stack's showing tab.
        app.add_systems(
            TickStage::Systems,
            (
                decorate_tabs.run_if(any_match_filter::<Added<TabStripButton>>),
                focus_gate.run_if(|shared: Res<Shared>| {
                    let core = shared.lock();
                    core.focus_request.is_some() || !core.destroy.is_empty()
                }),
            )
                .chain()
                .after(TabsSet)
                .after(DockSet::Report),
        );
    }
}

/// Queue the elements the page added to be read.
fn adopt_gate(mut commands: Commands) {
    commands.queue(adopt_elements);
}

/// Queue the elements to follow the model, reading any the DOM commands
/// added first, when either has work.
fn realize_gate(arrivals: Res<Arrivals>, shared: Res<Shared>, mut commands: Commands) {
    let arrived = arrivals_pending(arrivals);
    if arrived || shared.lock().dirty {
        commands.queue(move |world: &mut World| {
            if arrived {
                adopt_elements(world);
            }
            realize(world);
        });
    }
}

/// Queue the keys pressed this tick for the dock to answer, when any was.
fn keys_gate(
    mut keys: MessageReader<lumen_module::lumen_core::input::KeyPressed>,
    mut commands: Commands,
) {
    let pressed: Vec<_> = keys.read().map(|k| (k.key.clone(), k.modifiers)).collect();
    if !pressed.is_empty() {
        commands.queue(move |world: &mut World| crate::keys::dock_keys(world, pressed));
    }
}

/// Queue the focus move or the panel removal the dock owes.
fn focus_gate(mut commands: Commands) {
    commands.queue(crate::keys::focus_requests);
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

/// The messages [`user_input`] reads.
#[derive(bevy_ecs::system::SystemParam)]
struct UserInput<'w, 's> {
    clicks: MessageReader<'w, 's, ClickEvent>,
    doubles: MessageReader<'w, 's, DoubleClickEvent>,
    resized: MessageReader<'w, 's, SplitResized>,
    closing: MessageReader<'w, 's, WindowClosing>,
    delivered: MessageReader<'w, 's, CustomEventDelivered>,
}

/// Read what the user did to the dock this tick: a tab picked, a close
/// button or a middle click, a double click on a window's strip, a split
/// resized, a window closed, a panel close a handler allowed.
fn user_input(
    (shared, settings, built): (Res<Shared>, Res<Settings>, Res<Built>),
    mut input: UserInput,
    mut events: MessageWriter<CustomEvent>,
    picked: Query<(&DockStack, &Tabs), Changed<Tabs>>,
    (dock_tabs, buttons, close_buttons, parents): (
        Query<&DockTab>,
        Query<&TabStripButton>,
        Query<&DockCloseButton>,
        Query<&ChildOf>,
    ),
    windows: Query<&DockWindow>,
    (menu_items, tab_menu): (Query<&DockMenuItem>, Res<TabMenu>),
) {
    let UserInput {
        clicks,
        doubles,
        resized,
        closing,
        delivered,
    } = &mut input;
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
    // A close button, or a middle click on a tab, asks to close the panel;
    // a tab menu item does what it says.
    for click in clicks.read() {
        if let Some(action) = crate::menu::item_action(&menu_items, &parents, click.entity) {
            let Some(panel) = tab_menu.panel.clone() else {
                continue;
            };
            if action == MenuAction::Close {
                if let Some(e) = core
                    .panels
                    .get(&panel)
                    .filter(|r| r.attrs.closable)
                    .and_then(|r| r.entity)
                {
                    let mut ask = CustomEvent::new(e, "panelclose").with("reason", "user");
                    ask.cancelable = true;
                    events.write(ask);
                }
                continue;
            }
            let done = match action {
                MenuAction::Detach => core.op_detach(&panel, Reason::User),
                MenuAction::DetachGroup => core.op_detach_group(&panel, Reason::User),
                MenuAction::Attach => core.op_attach(&panel, Reason::User),
                MenuAction::Maximize => core.op_maximize(&panel, Reason::User),
                MenuAction::Restore => {
                    if let Where::Dock(d) = core.ws.where_is(&panel) {
                        core.op_restore_maximized(&d, Reason::User);
                    }
                    Ok(())
                }
                MenuAction::Close => Ok(()),
            };
            if let Err(e) = done {
                lumen_module::lumen_core::warn_line!("lumen-dock: {e}");
            }
            continue;
        }
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
fn report(
    (shared, focus): (Res<Shared>, Res<FocusTracker>),
    (mut events, mut messages, mut announce): (
        MessageWriter<CustomEvent>,
        MessageWriter<DockEvent>,
        ResMut<A11yAnnouncementQueue>,
    ),
    mut store: ResMut<PropertyStore>,
    mut focused_window: MessageReader<WindowFocused>,
    (panels_q, parents, windows): (Query<&DockPanel>, Query<&ChildOf>, Query<&DockWindow>),
    mut last_focus: Local<Option<String>>,
) {
    let mut core = shared.lock();
    // An idle tick reports nothing and builds nothing to find that out.
    let model_moved = std::mem::take(&mut core.report_due);
    if !model_moved && !focus.is_changed() && focused_window.is_empty() {
        return;
    }
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
                if pending.reason != Reason::Layout
                    && let Some(text) = announcement(&core, &panel, &from, &to)
                {
                    announce.pending.push((text, A11yLive::Polite));
                }
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
    let missing: Vec<String> = if !model_moved {
        Vec::new()
    } else {
        core.ws
            .shown_panels()
            .into_iter()
            .chain(core.ws.homes.keys().cloned())
            .filter(|p| core.panels.get(p).is_none_or(|r| r.entity.is_none()))
            .filter(|p| !core.reported.contains(p))
            .collect()
    };
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
    let ids: Vec<String> = if model_moved {
        core.panels.keys().cloned().collect()
    } else {
        Vec::new()
    };
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

/// What a screen reader hears when a panel moved: "Inspector moved to a
/// new window", "Inspector docked back", "Inspector moved beside Scene".
/// A panel coming into or going out of view is not a move.
fn announcement(
    core: &crate::state::Core,
    panel: &str,
    from: &Where,
    to: &Where,
) -> Option<String> {
    let title = |p: &str| core.panels.get(p).map(|r| r.attrs.title.clone());
    let name = title(panel)?;
    let said = match (from, to) {
        (Where::Dock(_), Where::Window(_)) | (Where::Window(_), Where::Window(_)) => {
            text("lumen-dock-moved-to-window", "moved to a new window")
        }
        (Where::Window(_), Where::Dock(_)) => text("lumen-dock-docked-back", "docked back"),
        (Where::Dock(_), Where::Dock(_)) => {
            let beside = core
                .ws
                .find(panel)
                .and_then(|loc| core.ws.stack(&loc.stack))
                .and_then(|s| {
                    s.panels
                        .iter()
                        .filter(|p| p.as_str() != panel)
                        .find(|p| core.panels.get(*p).is_some_and(|r| r.entity.is_some()))
                        .and_then(|p| title(p))
                });
            match beside {
                Some(other) => format!(
                    "{} {other}",
                    text("lumen-dock-moved-beside", "moved beside")
                ),
                None => text("lumen-dock-moved", "moved"),
            }
        }
        _ => return None,
    };
    Some(format!("{name} {said}"))
}

/// A window the platform moved or resized, or the app changed.
type WindowMoved = Or<(Changed<WindowRuntime>, Changed<Window>)>;

/// Keep each window's place in the model as the platform reports it, so a
/// save writes where the window is.
fn track_windows(
    shared: Res<Shared>,
    windows: Query<(&DockWindow, &Window, &WindowRuntime), WindowMoved>,
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
        // A maximized or fullscreen window keeps the rectangle it had
        // before, which is where it goes when the user restores it.
        let covering = window.maximized || window.fullscreen;
        let (rect, key) = if covering && layout.place.rect.is_some() {
            (layout.place.rect, layout.place.monitor.clone())
        } else {
            let previous = Some((layout.place.rect, layout.place.monitor.clone()));
            let (rect, key) = lumen_dock_model::record(
                runtime.position.map(|p| [p.x, p.y]),
                [window.size.x, window.size.y],
                &monitors,
                previous,
            );
            (Some(rect), key)
        };
        let place = lumen_dock_model::WindowPlace {
            rect,
            monitor: key,
            maximized: window.maximized,
            fullscreen: window.fullscreen,
        };
        if layout.place != place {
            layout.place = place;
            core.save_due = true;
        }
    }
}

/// Show the windows of panels again when the main window shows again: under
/// `[app] quit = "explicit"`, closing the main window hides every window,
/// and an app that brings the main window back brings its panels' windows
/// with it.
fn show_with_the_primary(
    primary: Query<Ref<Window>, With<PrimaryWindow>>,
    mut windows: Query<&mut Window, (With<DockWindow>, Without<PrimaryWindow>)>,
) {
    let Ok(main) = primary.single() else { return };
    if !main.is_changed() || !main.visible {
        return;
    }
    for mut w in &mut windows {
        if !w.visible {
            w.visible = true;
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
    let close = text("lumen-dock-close", "Close");
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
        let icon = rec.attrs.icon.clone();
        commands.queue(move |world: &mut World| {
            if let Some(src) = icon {
                set_tab_icon(world, button, Some(&src));
            }
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

/// On the image a tab shows its panel's `icon` in.
#[derive(Component, Clone, Copy, Debug)]
pub struct DockTabIcon;

/// Show `src` before the title on the strip button `button`, or take the
/// icon away when `src` is `None`.
fn set_tab_icon(world: &mut World, button: Entity, src: Option<&str>) {
    if world.get_entity(button).is_err() {
        return;
    }
    let existing = world
        .get::<bevy_ecs::hierarchy::Children>(button)
        .and_then(|kids| kids.iter().find(|&k| world.get::<DockTabIcon>(k).is_some()));
    match (existing, src) {
        (Some(icon), None) => {
            world.entity_mut(icon).despawn();
        }
        (Some(icon), Some(src)) => {
            lumen_module::lumen_scene::spawn::set_image_source(
                &mut world.entity_mut(icon),
                Some(src),
            );
        }
        (None, Some(src)) => {
            let el = Element {
                tag: "image".into(),
                attrs: Box::new(Attributes {
                    classes: vec!["dock-tab-icon".into()],
                    src: Some(src.to_string()),
                    ..Attributes::default()
                }),
                ..Element::default()
            };
            let icon = spawn_subtree(world, &el, Some(button), Placeholders::Resolved);
            world.entity_mut(icon).insert((
                DockChrome,
                DockTabIcon,
                lumen_module::lumen_core::components::DomHidden,
                StyleDirty,
            ));
            world.entity_mut(button).insert_children(0, &[icon]);
        }
        (None, None) => {}
    }
}

/// The strip button showing `panel`'s tab, if it has one.
fn button_of(world: &mut World, panel: &str) -> Option<Entity> {
    let tab = world.resource::<Built>().tabs.get(panel).copied()?;
    let mut q = world.try_query::<(Entity, &TabStripButton)>()?;
    q.iter(world).find(|(_, b)| b.tab == tab).map(|(e, _)| e)
}

/// Open the tab menu for each context menu request on a dock's tab.
fn tab_menu_gate(mut asks: MessageReader<ContextMenuEvent>, mut commands: Commands) {
    let asks: Vec<(Entity, glam::Vec2)> = asks.read().map(|a| (a.entity, a.position)).collect();
    commands.queue(move |world: &mut World| {
        for (e, at) in asks {
            if let Some(panel) = crate::menu::tab_panel(world, e) {
                crate::menu::open(world, panel, at);
            }
        }
    });
}

/// Read a panel's attributes again when a script changed them: its tab
/// follows its `title` and `icon`, its window its `window-title` and
/// `bind-window-title`, and the next placement its `place` and `dockable`.
fn follow_panel_attrs(
    shared: Res<Shared>,
    changed: Query<(&DockPanel, &LumenAttributes), Changed<LumenAttributes>>,
    mut commands: Commands,
) {
    let mut core = shared.lock();
    for (panel, attrs) in &changed {
        let Some(rec) = core.panels.get_mut(&panel.id) else {
            continue;
        };
        let next = PanelAttrs::read(Some(attrs), &panel.id);
        let info = panel_info(Some(attrs), rec.info.dock.clone());
        if next == rec.attrs && info == rec.info {
            continue;
        }
        let retitled = next.title != rec.attrs.title;
        let icon = (next.icon != rec.attrs.icon).then(|| next.icon.clone());
        let signal = (next.window_title_signal != rec.attrs.window_title_signal)
            .then(|| next.window_title_signal.clone());
        let close = (retitled && next.closable)
            .then(|| format!("{} {}", text("lumen-dock-close", "Close"), next.title));
        rec.attrs = next;
        rec.info = info;
        if signal.is_some() {
            rec.bound_title = None;
        }
        core.dirty = true;
        let id = panel.id.clone();
        commands.queue(move |world: &mut World| {
            if let Some(signal) = signal {
                let value = signal.as_deref().and_then(|name| {
                    world
                        .resource::<PropertyStore>()
                        .get_global_str(name)
                        .map(|v| v.to_string())
                });
                if let Some(name) = signal {
                    world.resource_mut::<TitleSignals>().0.insert(name);
                }
                let shared = world.resource::<Shared>().clone();
                if let Some(rec) = shared.lock().panels.get_mut(&id) {
                    rec.bound_title = value;
                }
            }
            let Some(button) = button_of(world, &id) else {
                return;
            };
            if let Some(icon) = icon {
                set_tab_icon(world, button, icon.as_deref());
            }
            if let Some(label) = close {
                let mut q = world.query::<(Entity, &DockCloseButton)>();
                let buttons: Vec<Entity> = q
                    .iter(world)
                    .filter(|(_, b)| b.panel == id)
                    .map(|(e, _)| e)
                    .collect();
                for b in buttons {
                    world.entity_mut(b).insert(A11yLabel(label.clone()));
                }
            }
        });
    }
}

/// A panel's window title follows the signal its `bind-window-title` names.
fn follow_title_signals(
    shared: Res<Shared>,
    store: Res<PropertyStore>,
    signals: Res<TitleSignals>,
) {
    let names: Vec<String> = store
        .observe_dirty()
        .iter()
        .filter_map(|k| match k {
            lumen_module::lumen_core::property_store::PropertyKey::Global(name) => {
                Some(name.to_string())
            }
            _ => None,
        })
        .filter(|n| signals.0.contains(n))
        .collect();
    if names.is_empty() {
        return;
    }
    let mut core = shared.lock();
    let mut changed = false;
    for name in names {
        let value = store.get_global_str(&name).map(|v| v.to_string());
        for rec in core.panels.values_mut() {
            if rec.attrs.window_title_signal.as_deref() == Some(name.as_str())
                && rec.bound_title != value
            {
                rec.bound_title = value.clone();
                changed = true;
            }
        }
    }
    if changed {
        core.dirty = true;
    }
}
