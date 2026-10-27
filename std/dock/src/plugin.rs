//! The plugin: its configuration, the systems that read what the user does,
//! and the events and signals that tell the app what changed.

use std::collections::BTreeSet;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_dock_model::{Effect, Where};
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::components::Transform;
use lumen_module::lumen_core::components::{
    A11yAnnouncementQueue, A11yLabel, A11yLive, A11yRole, LumenAttributes, LumenClasses, StyleDirty,
};
use lumen_module::lumen_core::input::{
    ClickEvent, ContextMenuEvent, DoubleClickEvent, DragEndEvent, DragMoveEvent, DragStartEvent,
    FocusTracker, Key, KeyPressed, MouseWheel, NamedKey, PointerButton, WindowFocused,
};
use lumen_module::lumen_core::prelude::TickStage;
use lumen_module::lumen_core::property_store::PropertyStore;
use lumen_module::lumen_core::surface_drag::{SurfaceDragEvent, SurfaceDragRequest};
use lumen_module::lumen_core::window::{PrimaryWindow, Window, WindowClosing, WindowRuntime};
use lumen_module::lumen_ir::layout_ir::{Attributes, Element};
use lumen_module::lumen_primitives::{ActivePopupNav, SplitResized, TabStripButton, Tabs, TabsSet};
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
        app.world.init_resource::<crate::drag::DockDrag>();
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
        message::<DragStartEvent>(&mut app.world);
        message::<DragMoveEvent>(&mut app.world);
        message::<DragEndEvent>(&mut app.world);
        message::<MouseWheel>(&mut app.world);
        message::<SurfaceDragEvent>(&mut app.world);
        message::<SurfaceDragRequest>(&mut app.world);
        install_observers(&mut app.world);
        match crate::api::script_fns(&shared) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(e) => lumen_module::lumen_core::warn_line!("lumen-dock: {e}"),
        }
        // The module's world-wide work (reading new elements, making the
        // elements follow the model, moving focus, the keys) runs as queued
        // commands, so an idle tick runs no exclusive system of the module's
        // and the schedule keeps its parallelism. The rest is three nodes
        // in the schedule, each behind one condition that says whether it
        // has work: every node costs the executor time on every tick, so
        // the module adds as few as the places its work has to sit.
        // Elements read before any handler runs, so a script's `on_ready`
        // finds the panels the markup declared.
        app.add_systems(
            TickStage::Systems,
            adopt_gate
                .run_if(arrivals_pending)
                .before(ScriptSet::Ready)
                .before(ScriptSet::Dispatch)
                .before(ScriptSet::DomInput)
                .before(ScriptSet::DomState),
        );
        // What the user and the scripts did this tick, then the elements
        // follow the model: after the DOM commands, so a panel a script
        // appended is placed on the same tick, and before the widgets build
        // their parts, so a stack's new tab gets its button this tick.
        app.add_systems(
            TickStage::Systems,
            user_input
                .pipe(keys_gate)
                .pipe(tab_menu_gate)
                .pipe(drag_gate)
                .pipe(follow_panel_attrs)
                .pipe(follow_title_signals)
                .pipe(realize_gate)
                .run_if(input_due)
                .in_set(DockSet::Input)
                .in_set(DockSet::Realize)
                .after(ScriptSet::DomState)
                .after(lumen_module::lumen_scene::dom::apply_dom_commands)
                .before(lumen_module::lumen_scene::widgets::keep_widget_parts),
        );
        // What changed, told to the app; then the new tabs decorated and
        // focus moved, after the tab sets show what the realization picked,
        // so focus can land in a panel that just became its stack's
        // showing tab.
        app.add_systems(
            TickStage::Systems,
            report
                .pipe(track_windows)
                .pipe(show_with_the_primary)
                .pipe(save_if_due)
                .pipe(decorate_tabs)
                .pipe(strip_gate)
                .pipe(focus_gate)
                .run_if(report_due)
                .in_set(DockSet::Report)
                .after(DockSet::Realize)
                .after(lumen_module::lumen_scene::widgets::keep_widget_parts)
                .after(TabsSet),
        );
    }
}

/// Whether the dock has input to read or elements to make follow the
/// model this tick: one condition for the node, so an idle tick pays for one
/// check rather than one per kind of input.
#[derive(bevy_ecs::system::SystemParam)]
struct InputDue<'w, 's> {
    clicks: MessageReader<'w, 's, ClickEvent>,
    doubles: MessageReader<'w, 's, DoubleClickEvent>,
    resized: MessageReader<'w, 's, SplitResized>,
    closing: MessageReader<'w, 's, WindowClosing>,
    delivered: MessageReader<'w, 's, CustomEventDelivered>,
    keys: MessageReader<'w, 's, KeyPressed>,
    menus: MessageReader<'w, 's, ContextMenuEvent>,
    drags: (
        MessageReader<'w, 's, DragStartEvent>,
        MessageReader<'w, 's, DragMoveEvent>,
        MessageReader<'w, 's, DragEndEvent>,
        MessageReader<'w, 's, SurfaceDragEvent>,
    ),
    picked: Query<'w, 's, (), (With<DockStack>, Changed<Tabs>)>,
    retitled: Query<'w, 's, (), (With<DockPanel>, Changed<LumenAttributes>)>,
    titles: Res<'w, TitleSignals>,
    store: Res<'w, PropertyStore>,
    arrivals: Res<'w, Arrivals>,
    shared: Res<'w, Shared>,
    menu: Res<'w, TabMenu>,
    nav: Option<Res<'w, ActivePopupNav>>,
}

fn input_due(mut due: InputDue) -> bool {
    // Every reader is drained, so a message is counted on one tick only.
    let messages = [
        due.clicks.read().count(),
        due.doubles.read().count(),
        due.resized.read().count(),
        due.closing.read().count(),
        due.delivered.read().count(),
        due.keys.read().count(),
        due.menus.read().count(),
        due.drags.0.read().count(),
        due.drags.1.read().count(),
        due.drags.2.read().count(),
        due.drags.3.read().count(),
    ];
    messages.iter().any(|&n| n > 0)
        || !due.picked.is_empty()
        || !due.retitled.is_empty()
        || (!due.titles.0.is_empty() && due.store.is_changed())
        || !due.arrivals.new.is_empty()
        || !due.arrivals.gone.is_empty()
        || !due.arrivals.waiting.is_empty()
        || ((due.menu.pending.is_some() || due.menu.show.is_some())
            && due.nav.is_none_or(|n| n.0.is_none()))
        || due.shared.lock().dirty
}

/// Whether the dock has anything to report, decorate, track or save this
/// tick.
#[derive(bevy_ecs::system::SystemParam)]
struct ReportDue<'w, 's> {
    shared: Res<'w, Shared>,
    persist: Option<Res<'w, Persist>>,
    focus: Res<'w, FocusTracker>,
    focused: MessageReader<'w, 's, WindowFocused>,
    new_tabs: Query<'w, 's, (), Added<TabStripButton>>,
    windows: Query<'w, 's, (), With<DockWindow>>,
    moved: Query<'w, 's, (), (With<DockWindow>, WindowMoved)>,
    primary: Query<'w, 's, (), (With<PrimaryWindow>, Changed<Window>)>,
    strips: StripsChanged<'w, 's>,
}

/// A dock tab's strip button that moved, resized or changed its classes.
type TabButtonChanged = (
    With<DockTabButton>,
    Or<(Changed<Transform>, Changed<LumenClasses>)>,
);

/// What makes a tab strip fit itself again: it or one of its tabs moved or
/// resized, the shown tab changed, or the wheel turned.
#[derive(bevy_ecs::system::SystemParam)]
struct StripsChanged<'w, 's> {
    strips: Query<'w, 's, (), (With<crate::overflow::DockStrip>, Changed<Transform>)>,
    tabs: Query<'w, 's, (), TabButtonChanged>,
    wheels: MessageReader<'w, 's, MouseWheel>,
}

impl StripsChanged<'_, '_> {
    /// Whether anything changed, and the wheel turns this tick.
    fn read(&mut self) -> (bool, Vec<MouseWheel>) {
        let wheels: Vec<MouseWheel> = self.wheels.read().copied().collect();
        (
            !self.strips.is_empty() || !self.tabs.is_empty() || !wheels.is_empty(),
            wheels,
        )
    }
}

fn report_due(mut due: ReportDue) -> bool {
    let window_focus = due.focused.read().count() > 0;
    let model = {
        let core = due.shared.lock();
        core.report_due
            || core.focus_request.is_some()
            || !core.destroy.is_empty()
            || (due.persist.as_ref().is_some_and(|p| **p == Persist::Auto) && core.save_due)
    };
    model
        || window_focus
        || due.focus.is_changed()
        || !due.new_tabs.is_empty()
        || (!due.windows.is_empty() && (!due.moved.is_empty() || !due.primary.is_empty()))
        || due.strips.read().0
}

/// Queue the strips to fit themselves to their tabs when one changed, and
/// a wheel turn over one to scroll it.
fn strip_gate(_: In<()>, mut changed: StripsChanged, mut commands: Commands) {
    let (due, wheels) = changed.read();
    if !due {
        return;
    }
    commands.queue(move |world: &mut World| {
        // A wheel turn over a strip scrolls it along, whichever way it
        // turned.
        let mut q =
            world.query_filtered::<Entity, With<lumen_module::lumen_core::input::Hovered>>();
        let hovered: Vec<Entity> = q.iter(world).collect();
        let strip = hovered
            .into_iter()
            .find_map(|h| crate::overflow::strip_under(world, h));
        let turn: f32 = wheels
            .iter()
            .map(|w| {
                if w.delta.x.abs() > w.delta.y.abs() {
                    w.delta.x
                } else {
                    w.delta.y
                }
            })
            .sum();
        let wheel = strip.filter(|_| turn != 0.0).map(|s| (s, turn));
        crate::overflow::fit_strips(world, wheel);
    });
}

/// Queue the elements the page added to be read.
fn adopt_gate(mut commands: Commands) {
    commands.queue(adopt_elements);
}

/// Queue the elements to follow the model, reading any the DOM commands
/// added first, when either has work.
fn realize_gate(_: In<()>, arrivals: Res<Arrivals>, shared: Res<Shared>, mut commands: Commands) {
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
    _: In<()>,
    mut keys: MessageReader<lumen_module::lumen_core::input::KeyPressed>,
    mut commands: Commands,
) {
    let pressed: Vec<_> = keys.read().map(|k| (k.key.clone(), k.modifiers)).collect();
    if !pressed.is_empty() {
        commands.queue(move |world: &mut World| crate::keys::dock_keys(world, pressed));
    }
}

/// Queue what the pointer did to a drag this tick: a drag starting on a
/// tab, moves and the release (through the seam where the window backend
/// carries one), and Escape.
fn drag_gate(
    _: In<()>,
    (mut starts, mut moves, mut ends, mut seam): (
        MessageReader<DragStartEvent>,
        MessageReader<DragMoveEvent>,
        MessageReader<DragEndEvent>,
        MessageReader<SurfaceDragEvent>,
    ),
    mut keys: MessageReader<KeyPressed>,
    drag: Res<crate::drag::DockDrag>,
    mut commands: Commands,
) {
    use crate::drag::DragInput;
    let mut inputs: Vec<DragInput> = starts
        .read()
        .map(|s| DragInput::Start {
            entity: s.entity,
            start: s.start,
        })
        .collect();
    let started = !inputs.is_empty();
    if drag.active() || started {
        // Escape first: the press it cancels may end on the same tick, and
        // that end must not land the drag.
        if keys.read().any(|k| k.key == Key::Named(NamedKey::Escape)) {
            inputs.insert(usize::from(started), DragInput::Escape);
        }
        inputs.extend(moves.read().map(|m| DragInput::Moved(m.position)));
        inputs.extend(ends.read().map(|e| DragInput::Ended(e.position)));
        inputs.extend(seam.read().map(|e| DragInput::Seam(*e)));
    } else {
        moves.clear();
        ends.clear();
        seam.clear();
        keys.clear();
    }
    if !inputs.is_empty() {
        commands.queue(move |world: &mut World| crate::drag::drag_step(world, inputs));
    }
}

/// Queue the focus move or the panel removal the dock owes, and the
/// elements to follow a change the report itself made (a slid-out panel
/// focus left).
fn focus_gate(_: In<()>, shared: Res<Shared>, mut commands: Commands) {
    let (due, dirty) = {
        let core = shared.lock();
        (
            core.focus_request.is_some() || !core.destroy.is_empty(),
            core.dirty,
        )
    };
    if dirty {
        commands.queue(realize);
    }
    if due {
        commands.queue(crate::keys::focus_requests);
    }
}

/// Do what the tab menu item `action` says to `panel`.
fn menu_action(
    core: &mut crate::state::Core,
    events: &mut MessageWriter<CustomEvent>,
    action: MenuAction,
    panel: &str,
) {
    let done = match action {
        MenuAction::Close => {
            if let Some(e) = core
                .panels
                .get(panel)
                .filter(|r| r.attrs.closable)
                .and_then(|r| r.entity)
            {
                let mut ask = CustomEvent::new(e, "panelclose").with("reason", "user");
                ask.cancelable = true;
                events.write(ask);
            }
            Ok(())
        }
        MenuAction::Detach => core.op_detach(panel, Reason::User),
        MenuAction::Float => {
            // Each new float a little below and right of the last.
            let n = core
                .ws
                .docks
                .values()
                .map(|d| d.floats.len())
                .sum::<usize>() as f32;
            let rect = lumen_dock_model::Rect {
                x: 48.0 + 24.0 * n,
                y: 48.0 + 24.0 * n,
                w: 360.0,
                h: 260.0,
            };
            core.op_float(panel, rect, Reason::User)
        }
        MenuAction::Pin => {
            let edge = crate::api::nearest_edge(&core.ws, panel);
            core.op_pin(panel, edge, Reason::User)
        }
        MenuAction::DetachGroup => core.op_detach_group(panel, Reason::User),
        MenuAction::Attach => core.op_attach(panel, Reason::User),
        MenuAction::Maximize => core.op_maximize(panel, Reason::User),
        MenuAction::Restore => {
            if let Where::Dock(d) = core.ws.where_is(panel) {
                core.op_restore_maximized(&d, Reason::User);
            }
            Ok(())
        }
    };
    match done {
        // Keyboard focus goes with a panel that moved.
        Ok(())
            if matches!(
                action,
                MenuAction::Detach | MenuAction::DetachGroup | MenuAction::Attach
            ) =>
        {
            core.focus_request = Some(panel.to_string());
        }
        Ok(()) => {}
        Err(e) => lumen_module::lumen_core::warn_line!("lumen-dock: {e}"),
    }
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
    windows: Query<'w, 's, &'static DockWindow>,
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
    (menu_items, mut tab_menu, nav): (
        Query<&DockMenuItem>,
        ResMut<TabMenu>,
        Option<Res<ActivePopupNav>>,
    ),
    mut frame: FrameQueries,
) {
    let UserInput {
        clicks,
        doubles,
        resized,
        closing,
        delivered,
        windows,
    } = &mut input;
    let mut core = shared.lock();
    let menu_closed = nav.is_none_or(|n| n.0.is_none());
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
    // A tab menu item does what it says once the menu has closed and handed
    // focus back to the tab it opened on: the move may rebuild that tab.
    if menu_closed
        && let Some(action) = tab_menu.pending.take()
        && let Some(panel) = tab_menu.panel.clone()
    {
        menu_action(&mut core, &mut events, action, &panel);
    }
    // A tab picked from a stack's overflow list shows, once the list closed.
    if menu_closed && let Some(panel) = tab_menu.show.take() {
        let _ = core.op_activate(&panel, Reason::User);
        core.focus_request = Some(panel);
    }
    // A close button, or a middle click on a tab, asks to close the panel;
    // a tab menu item waits for its menu to close.
    for click in clicks.read() {
        if let Some(action) = crate::menu::item_action(&menu_items, &parents, click.entity) {
            tab_menu.pending = Some(action);
            continue;
        }
        if let Some(panel) =
            crate::overflow::item_panel(&frame.overflow_items, &parents, click.entity)
        {
            tab_menu.show = Some(panel);
            continue;
        }
        if frame.click(&mut core, &parents, click.entity) {
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
    // A double click on a window's strip docks the window back, on a
    // float's its panels.
    for double in doubles.read() {
        let mut cur = Some(double.entity);
        while let Some(e) = cur {
            if let Ok(f) = frame.floats.get(e) {
                core.op_attach_float(&f.id, Reason::User);
                break;
            }
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

/// A slid-out panel's element, or a pinned panel's button: where focus may
/// go without sliding the panel back in.
type SlideOutParts = Or<(
    With<crate::frame::DockSlideOut>,
    With<crate::frame::DockPinButton>,
)>;

/// The parts of the dock's frame a click can land on.
#[derive(bevy_ecs::system::SystemParam)]
struct FrameQueries<'w, 's> {
    pins: Query<'w, 's, &'static crate::frame::DockPinButton>,
    unpins: Query<'w, 's, &'static crate::frame::DockUnpinButton>,
    slides: Query<'w, 's, &'static crate::frame::DockSlideOut>,
    floats: Query<'w, 's, &'static crate::frame::DockFloat>,
    overflow: Query<'w, 's, &'static crate::overflow::DockOverflowButton>,
    overflow_items: Query<'w, 's, &'static crate::overflow::DockOverflowItem>,
    commands: Commands<'w, 's>,
}

impl FrameQueries<'_, '_> {
    /// Answer a click on `entity` that the frame owns: a pinned panel's
    /// button slides it out or back in, a slid-out panel's dock button docks
    /// it back, a stack's overflow button lists its tabs; any click outside
    /// a slid-out panel slides it back in, and a click in a float raises it.
    /// Returns whether the click was the frame's own.
    fn click(
        &mut self,
        core: &mut crate::state::Core,
        parents: &Query<&ChildOf>,
        entity: Entity,
    ) -> bool {
        let mut inside_slide = false;
        let mut cur = Some(entity);
        let mut depth = 0;
        while let Some(e) = cur {
            if depth < 3
                && let Ok(b) = self.pins.get(e)
            {
                let panel = b.panel.clone();
                let effects = match core.ws.pinned_at(&panel) {
                    Some((dock, _)) if core.ws.in_view(&panel) => core.ws.close_pinned(&dock),
                    Some(_) => core.ws.open_pinned(&panel).unwrap_or_default(),
                    None => Vec::new(),
                };
                core.apply(effects, Reason::User);
                return true;
            }
            if depth < 3
                && let Ok(b) = self.unpins.get(e)
            {
                let panel = b.panel.clone();
                if core.op_attach(&panel, Reason::User).is_ok() {
                    core.focus_request = Some(panel);
                }
                return true;
            }
            if depth < 3
                && let Ok(b) = self.overflow.get(e)
            {
                let stack = b.stack;
                self.commands
                    .queue(move |world: &mut World| crate::overflow::open_list(world, stack));
                return true;
            }
            if self.slides.get(e).is_ok() {
                inside_slide = true;
            }
            if let Ok(f) = self.floats.get(e)
                && core.ws.raise_float(&f.id)
            {
                core.dirty = true;
                core.save_due = true;
            }
            depth += 1;
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
        if !inside_slide {
            let open: Vec<String> = core
                .ws
                .docks
                .iter()
                .filter(|(_, d)| d.open.is_some())
                .map(|(id, _)| id.clone())
                .collect();
            for dock in open {
                let effects = core.ws.close_pinned(&dock);
                core.apply(effects, Reason::User);
            }
        }
        false
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
    (panels_q, parents, windows, slides): (
        Query<&DockPanel>,
        Query<&ChildOf>,
        Query<&DockWindow>,
        Query<(), SlideOutParts>,
    ),
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
    // Focus leaving a slid-out panel slides it back in.
    if focus.is_changed()
        && let Some(f) = focus.0
        && core.ws.docks.values().any(|d| d.open.is_some())
    {
        let mut cur = Some(f);
        let mut inside = false;
        while let Some(e) = cur {
            if slides.contains(e) {
                inside = true;
                break;
            }
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
        if !inside {
            let open: Vec<String> = core
                .ws
                .docks
                .iter()
                .filter(|(_, d)| d.open.is_some())
                .map(|(id, _)| id.clone())
                .collect();
            for dock in open {
                let effects = core.ws.close_pinned(&dock);
                core.apply(effects, Reason::User);
            }
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
        (Where::Window(_) | Where::Float(_) | Where::Sidebar(_), Where::Dock(_)) => {
            text("lumen-dock-docked-back", "docked back")
        }
        (_, Where::Float(_)) => text("lumen-dock-floated", "floating"),
        (_, Where::Sidebar(_)) => text("lumen-dock-pinned", "pinned to the sidebar"),
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
    _: In<()>,
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
    _: In<()>,
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
    _: In<()>,
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
        commands
            .entity(button)
            .insert((DockTabButton, A11yLabel(rec.attrs.title.clone())));
        let closable = rec.attrs.closable;
        let close_label = format!("{close} {}", rec.attrs.title);
        let title = rec.attrs.title.clone();
        let panel = tab.panel.clone();
        let icon = rec.attrs.icon.clone();
        commands.queue(move |world: &mut World| {
            if world.get_entity(button).is_err() {
                return;
            }
            {
                let mut e = world.entity_mut(button);
                let mut cl = e.take::<LumenClasses>().unwrap_or_default();
                if !cl.has("dock-tab") {
                    cl.0.push("dock-tab".into());
                }
                e.insert((cl, StyleDirty));
            }
            // The stack's shown tab is marked from its first frame, and its
            // strip fitted to its tabs from then on.
            if let Some(strip) = world.get::<ChildOf>(button).map(ChildOf::parent) {
                let stack = world.get::<ChildOf>(strip).map(ChildOf::parent);
                let shown = stack
                    .and_then(|stack| world.get::<Tabs>(stack))
                    .and_then(|t| t.shown);
                crate::realize::mark_active_tab(world, strip, shown);
                if let Some(stack) = stack.filter(|&s| world.get::<DockStack>(s).is_some()) {
                    crate::overflow::mark_strip(world, stack, strip);
                }
            }
            // The title, then the close button, then the icon put first.
            let caption = spawn_part(world, button, "label", "dock-tab-label", Some(title));
            world.entity_mut(caption).insert(DockTabCaption);
            if closable {
                let x = spawn_part(
                    world,
                    button,
                    "button",
                    "dock-tab-close",
                    Some(CLOSE.into()),
                );
                world.entity_mut(x).insert((
                    lumen_module::lumen_core::components::TabIndex(-1),
                    A11yRole::Button,
                    A11yLabel(close_label),
                    DockCloseButton { panel },
                ));
            }
            if let Some(src) = icon {
                set_tab_icon(world, button, Some(&src));
            }
        });
    }
}

/// The close button's mark, the multiplication sign, until a stylesheet
/// draws its own with `mask-image`.
const CLOSE: &str = "\u{d7}";

/// Spawn one part of a dock tab under `parent`: the module's own structure,
/// out of the document a script walks.
fn spawn_part(
    world: &mut World,
    parent: Entity,
    tag: &str,
    class: &str,
    text: Option<String>,
) -> Entity {
    let el = Element {
        tag: tag.into(),
        attrs: Box::new(Attributes {
            classes: vec![class.into()],
            text,
            ..Attributes::default()
        }),
        ..Element::default()
    };
    let e = spawn_subtree(world, &el, Some(parent), Placeholders::Resolved);
    world.entity_mut(e).insert((
        DockChrome,
        lumen_module::lumen_core::components::DomHidden,
        StyleDirty,
    ));
    e
}

/// On the caption a dock tab shows its panel's title in.
#[derive(Component, Clone, Copy, Debug)]
pub struct DockTabCaption;

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
fn tab_menu_gate(_: In<()>, mut asks: MessageReader<ContextMenuEvent>, mut commands: Commands) {
    let asks: Vec<(Entity, glam::Vec2)> = asks.read().map(|a| (a.entity, a.position)).collect();
    if asks.is_empty() {
        return;
    }
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
    _: In<()>,
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
        let title = retitled.then(|| next.title.clone());
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
            if let Some(title) = &title {
                world.entity_mut(button).insert(A11yLabel(title.clone()));
                let caption = world
                    .get::<bevy_ecs::hierarchy::Children>(button)
                    .and_then(|kids| {
                        kids.iter()
                            .find(|&k| world.get::<DockTabCaption>(k).is_some())
                    });
                if let Some(caption) = caption {
                    world.entity_mut(caption).insert((
                        lumen_module::lumen_core::components::TextContent(title.clone()),
                        lumen_module::lumen_core::components::DirtyLayout,
                    ));
                }
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
    _: In<()>,
    shared: Res<Shared>,
    store: Res<PropertyStore>,
    signals: Res<TitleSignals>,
) {
    if signals.0.is_empty() || !store.is_changed() {
        return;
    }
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
