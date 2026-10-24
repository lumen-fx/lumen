//! Event handlers on element ids: the candela-style half of the SDK.
//!
//! An app registers closures against an element id
//! ([`App::on_click`](crate::App::on_click) and the rest of the `on_*` family),
//! or against every element ([`App::on_any_click`](crate::App::on_any_click)).
//! Each tick, [`collect_ui_events`] gathers the element events the engine
//! produced, and [`dispatch_handlers`] calls the handler each one belongs to
//! with a [`Ui`] for the app and an [`Event`] describing what happened.
//!
//! An event belongs to the nearest element at or above its target that has a
//! handler for its kind: a click on a button's icon runs the button's
//! handler. With no such element it goes to the `on_any_*` handler of its
//! kind, which reads [`Event::id`] to tell elements apart.
//!
//! Both systems sit in [`AppSet`], after the tick's input and control pushes
//! and before the binding readers, so a signal a handler writes is on screen
//! the tick the event fired.

// The engine's copy of each crate this module names. The re-export block in
// lib.rs says why they come from there rather than from a dependency.
use crate::{bevy_ecs, lumen_core, lumen_primitives, lumen_scene, lumen_script};

use std::collections::{HashMap, HashSet};

use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::LumenId;
use lumen_core::input::{
    ClickEvent, ContextMenuEvent, DoubleClickEvent, LongPressEvent, MenuClicked, Modifiers,
    ModifiersState, PointerButton,
};
use lumen_core::tick::{AppSet, TickStage};
use lumen_primitives::{SliderChanged, ToggleChanged};
use lumen_scene::spawn::ForMarker;
use lumen_script::CustomEventDelivered;
use lumen_script::node_query::NodeRef;

use crate::dom::Node;
use crate::ui::Ui;

/// The kind of element event a handler is registered for.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum EventKind {
    /// Pointer press and release on the same element.
    Click,
    /// Two clicks within the double-click window. On that tick the plain
    /// [`EventKind::Click`] for the same element does not fire: a quick
    /// pair is one double-click, not two clicks and a double-click.
    DoubleClick,
    /// A press held past the long-press threshold.
    LongPress,
    /// A request for a context menu: a right click, the ContextMenu key or
    /// Shift+F10. Open a `<menu>` from the handler with
    /// [`Ui::open_menu_at`] at [`Event::position`].
    ContextMenu,
    /// A `<toggle>`, `<switch>` or `<checkbox>` flipped; [`Event::checked`]
    /// is its new state.
    Toggle,
    /// A `<slider>` moved; [`Event::value`] is its new value.
    Slider,
    /// A `<menuitem>` was chosen. The id is the item's own; a menu item is
    /// not in the element tree, so there is no target and no row.
    Menu,
    /// A custom event a module raised under this name
    /// ([`lumen_script::CustomEvent`]), such as a dock's `layoutchange`.
    /// [`Event::detail`] is what it carries and
    /// [`Event::default_prevented`] whether a DOM handler cancelled it.
    Custom(&'static str),
}

/// What happened, handed to a handler beside the [`Ui`].
#[derive(Clone, Debug)]
pub struct Event {
    kind: EventKind,
    id: Option<String>,
    target: Option<Entity>,
    row: Option<Row>,
    position: Option<Vec2>,
    button: Option<PointerButton>,
    checked: Option<bool>,
    value: Option<f64>,
    detail: Option<HashMap<String, lumen_script::ScriptValue>>,
    default_prevented: bool,
    modifiers: Modifiers,
}

impl Event {
    /// The kind of event.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// The id of the element whose handler is running. For an `on_any_*`
    /// handler, the id of the nearest element at or above the target that
    /// has one. `None` when nothing on the way to the root has an id.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The innermost element the event happened on, which may be a child of
    /// the element whose handler runs (the icon inside a button). `None` for
    /// a menu item.
    pub fn target(&self) -> Option<Node> {
        self.target
            .map(|e| Node::from_handle(NodeRef::from_entity(e).0))
    }

    /// The `<for>` row the target sits in, when it sits in one: which list,
    /// which record, and its fields.
    pub fn row(&self) -> Option<&Row> {
        self.row.as_ref()
    }

    /// Where the pointer was, in window coordinates, for a click or a
    /// context-menu request.
    pub fn position(&self) -> Option<Vec2> {
        self.position
    }

    /// The pointer button of a click or a context-menu request.
    pub fn button(&self) -> Option<PointerButton> {
        self.button
    }

    /// The new state of a toggled control.
    pub fn checked(&self) -> Option<bool> {
        self.checked
    }

    /// The new value of a slider.
    pub fn value(&self) -> Option<f64> {
        self.value
    }

    /// What a custom event carries; `None` for the other kinds.
    pub fn detail(&self) -> Option<&HashMap<String, lumen_script::ScriptValue>> {
        self.detail.as_ref()
    }

    /// Whether a DOM handler called `prevent_default` on a cancelable custom
    /// event before this handler ran. Always `false` for the other kinds.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented
    }

    /// The modifier keys held when the event was handled.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
}

/// A `<for>` row an event happened in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    list: String,
    key: String,
    index: usize,
    fields: HashMap<String, String>,
}

impl Row {
    /// The array signal the `<for each>` reads.
    pub fn list(&self) -> &str {
        &self.list
    }

    /// The row's key: the record's `key` field when the `<for>` names one,
    /// else its position.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The row's position in the array.
    pub fn index(&self) -> usize {
        self.index
    }

    /// A field of the record the row was built from, as the row shows it.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// A boxed handler.
pub(crate) type Handler = Box<dyn FnMut(&mut Ui<'_>, &Event) + Send + Sync + 'static>;

/// A boxed `on_ready` handler.
pub(crate) type ReadyHandler = Box<dyn FnMut(&mut Ui<'_>) + Send + Sync + 'static>;

/// One `(kind, id, handler)` registration. A `None` id is the `on_any_*`
/// slot.
pub(crate) type HandlerEntry = (EventKind, Option<String>, Handler);

/// The handlers, by kind and id.
#[derive(Resource, Default)]
pub(crate) struct Handlers {
    by_id: HashMap<(EventKind, Option<String>), Vec<Handler>>,
    ready: Vec<ReadyHandler>,
    ready_done: bool,
}

impl Handlers {
    fn has(&self, kind: EventKind, id: &str) -> bool {
        self.by_id.contains_key(&(kind, Some(id.to_string())))
    }

    /// The [`EventKind::Custom`] some handler was registered for under
    /// `name`.
    fn custom_kind(&self, name: &str) -> Option<EventKind> {
        self.by_id.keys().find_map(|(kind, _)| match kind {
            EventKind::Custom(n) if *n == name => Some(*kind),
            _ => None,
        })
    }
}

/// An element event waiting for dispatch, before its handler is chosen.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    kind: EventKind,
    target: Option<Entity>,
    menu_id: Option<String>,
    position: Option<Vec2>,
    button: Option<PointerButton>,
    checked: Option<bool>,
    value: Option<f64>,
    detail: Option<HashMap<String, lumen_script::ScriptValue>>,
    default_prevented: bool,
}

impl Pending {
    fn on(kind: EventKind, target: Option<Entity>) -> Self {
        Self {
            kind,
            target,
            menu_id: None,
            position: None,
            button: None,
            checked: None,
            value: None,
            detail: None,
            default_prevented: false,
        }
    }
}

/// This tick's element events, filled by [`collect_ui_events`] and drained by
/// [`dispatch_handlers`].
#[derive(Resource, Default)]
pub(crate) struct PendingEvents(Vec<Pending>);

/// Gather this tick's element events, in the order they happened per kind.
///
/// A double-click drops the plain clicks on the same element that tick, so
/// a quick pair is one double-click.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_ui_events(
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    mut longs: MessageReader<LongPressEvent>,
    mut menus: MessageReader<ContextMenuEvent>,
    mut toggles: MessageReader<ToggleChanged>,
    mut sliders: MessageReader<SliderChanged>,
    mut items: MessageReader<MenuClicked>,
    mut customs: MessageReader<CustomEventDelivered>,
    mut queue: ResMut<PendingEvents>,
    handlers: Res<Handlers>,
) {
    let doubled: Vec<Entity> = doubles.read().map(|d| d.entity).collect();
    let double_set: HashSet<Entity> = doubled.iter().copied().collect();
    for click in clicks.read() {
        if double_set.contains(&click.entity) {
            continue;
        }
        let mut ev = Pending::on(EventKind::Click, Some(click.entity));
        ev.position = Some(click.position);
        ev.button = Some(click.button);
        queue.0.push(ev);
    }
    let mut seen = HashSet::new();
    for entity in doubled {
        if seen.insert(entity) {
            queue
                .0
                .push(Pending::on(EventKind::DoubleClick, Some(entity)));
        }
    }
    for press in longs.read() {
        queue
            .0
            .push(Pending::on(EventKind::LongPress, Some(press.entity)));
    }
    for request in menus.read() {
        let mut ev = Pending::on(EventKind::ContextMenu, Some(request.entity));
        ev.position = Some(request.position);
        ev.button = Some(request.button);
        queue.0.push(ev);
    }
    for toggle in toggles.read() {
        let mut ev = Pending::on(EventKind::Toggle, Some(toggle.entity));
        ev.checked = Some(toggle.checked);
        queue.0.push(ev);
    }
    for slider in sliders.read() {
        let mut ev = Pending::on(EventKind::Slider, Some(slider.entity));
        ev.value = Some(f64::from(slider.value));
        queue.0.push(ev);
    }
    for item in items.read() {
        let mut ev = Pending::on(EventKind::Menu, None);
        ev.menu_id = Some(item.id.clone());
        queue.0.push(ev);
    }
    for delivered in customs.read() {
        // A kind names its event with a `&'static str`; the one a handler
        // was registered with is the one the event takes, so an event no
        // handler names is dropped here.
        let Some(kind) = handlers.custom_kind(&delivered.event.name) else {
            continue;
        };
        let mut ev = Pending::on(kind, Some(delivered.event.target));
        ev.detail = Some(delivered.event.detail.clone());
        ev.default_prevented = delivered.default_prevented;
        queue.0.push(ev);
    }
}

/// The ids at and above `entity`, innermost first.
fn ids_up(world: &World, entity: Entity) -> Vec<String> {
    let mut ids = Vec::new();
    let mut at = Some(entity);
    while let Some(e) = at {
        let Ok(entity_ref) = world.get_entity(e) else {
            break;
        };
        if let Some(id) = entity_ref.get::<LumenId>() {
            ids.push(id.0.clone());
        }
        at = entity_ref.get::<ChildOf>().map(ChildOf::parent);
    }
    ids
}

/// The `<for>` row `entity` sits in: the child of a `<for>` block it is, or
/// is under, and that child's record.
pub(crate) fn row_of(world: &World, entity: Entity) -> Option<Row> {
    let mut child = entity;
    loop {
        let parent = world.get::<ChildOf>(child)?.parent();
        if let Some(block) = world.get::<ForMarker>(parent) {
            let children = world.get::<Children>(parent)?;
            let position = children.iter().position(|c| c == child)?;
            let per_row = block.body.len().max(1);
            let slot = position / per_row;
            let (index, key) = block.rows.get(slot).cloned()?;
            let fields = block.row_items.get(slot).cloned().unwrap_or_default();
            return Some(Row {
                list: block.array_name.clone(),
                key,
                index,
                fields,
            });
        }
        child = parent;
    }
}

/// Call the handler each of this tick's events belongs to, and the
/// `on_ready` handlers on the first tick the tree is queryable.
///
/// Exclusive: a handler gets the whole world through its [`Ui`], so the
/// handlers are taken out of the world for the call and put back after,
/// with any a handler registered meanwhile.
pub(crate) fn dispatch_handlers(world: &mut World) {
    let pending = std::mem::take(&mut world.resource_mut::<PendingEvents>().0);
    let ready_due = {
        let handlers = world.resource::<Handlers>();
        !handlers.ready_done && !handlers.ready.is_empty()
    } && tree_is_published();
    if pending.is_empty() && !ready_due {
        return;
    }
    let mut handlers = std::mem::take(&mut *world.resource_mut::<Handlers>());
    if ready_due {
        handlers.ready_done = true;
        let mut ui = Ui::new(world);
        for handler in &mut handlers.ready {
            handler(&mut ui);
        }
    }
    let modifiers = world
        .get_resource::<ModifiersState>()
        .map(|m| m.0)
        .unwrap_or_default();
    for p in pending {
        let (slot_id, id) = match (&p.menu_id, p.target) {
            (Some(item), _) => {
                let slot = handlers.has(p.kind, item).then(|| item.clone());
                (slot, Some(item.clone()))
            }
            (None, Some(target)) => {
                let ids = ids_up(world, target);
                let slot = ids.iter().find(|id| handlers.has(p.kind, id)).cloned();
                let id = slot.clone().or_else(|| ids.first().cloned());
                (slot, id)
            }
            (None, None) => (None, None),
        };
        let event = Event {
            kind: p.kind,
            id,
            target: p.target,
            row: p.target.and_then(|t| row_of(world, t)),
            position: p.position,
            button: p.button,
            checked: p.checked,
            value: p.value,
            detail: p.detail,
            default_prevented: p.default_prevented,
            modifiers,
        };
        if let Some(list) = handlers.by_id.get_mut(&(p.kind, slot_id)) {
            let mut ui = Ui::new(world);
            for handler in list.iter_mut() {
                handler(&mut ui, &event);
            }
        }
    }
    // Put the handlers back, keeping any registered while they were out.
    let mut registry = world.resource_mut::<Handlers>();
    for (key, mut list) in handlers.by_id {
        registry.by_id.entry(key).or_default().append(&mut list);
    }
    let mut late_ready = std::mem::take(&mut registry.ready);
    handlers.ready.append(&mut late_ready);
    registry.ready = handlers.ready;
    registry.ready_done |= handlers.ready_done;
}

/// Whether the tree has been published for queries yet: what `on_ready`
/// waits for, as a script's does.
fn tree_is_published() -> bool {
    !lumen_core::node::dom_index_snapshot().records().is_empty()
}

/// Install the handlers and the two systems that serve them. A no-op for an
/// app with none, which pays for no systems.
pub(crate) fn install_handlers(
    app: &mut lumen_core::app::App,
    handlers: Vec<HandlerEntry>,
    ready: Vec<ReadyHandler>,
) {
    if handlers.is_empty() && ready.is_empty() {
        return;
    }
    let mut registry = Handlers {
        ready,
        ..Handlers::default()
    };
    for (kind, id, handler) in handlers {
        registry.by_id.entry((kind, id)).or_default().push(handler);
    }
    app.world.insert_resource(registry);
    app.world.insert_resource(PendingEvents::default());
    // What the collector reads, for an app with no widgets installed (a bare
    // test app): a message nothing writes is still one a system may read.
    ensure_message::<ClickEvent>(app);
    ensure_message::<DoubleClickEvent>(app);
    ensure_message::<LongPressEvent>(app);
    ensure_message::<ContextMenuEvent>(app);
    ensure_message::<ToggleChanged>(app);
    ensure_message::<SliderChanged>(app);
    ensure_message::<MenuClicked>(app);
    ensure_message::<CustomEventDelivered>(app);
    app.add_systems(
        TickStage::Systems,
        (collect_ui_events, dispatch_handlers)
            .chain()
            .in_set(AppSet)
            .in_set(HandlerSet),
    );
}

/// Where the handlers run inside [`AppSet`]: first, so the systems an app
/// adds with [`App::add_systems`](crate::App::add_systems), which run after
/// it, see on the same tick what a handler changed (a tool a click picked, a
/// dialog a click opened).
#[derive(bevy_ecs::schedule::SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct HandlerSet;

fn ensure_message<M: bevy_ecs::message::Message>(app: &mut lumen_core::app::App) {
    if !app
        .world
        .contains_resource::<bevy_ecs::message::Messages<M>>()
    {
        app.add_message::<M>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::property_store::PropertyStore;

    fn app_with(handlers: Vec<HandlerEntry>) -> lumen_core::app::App {
        let mut app = lumen_core::app::App::new();
        install_handlers(&mut app, handlers, Vec::new());
        app
    }

    fn click(app: &mut lumen_core::app::App, entity: Entity) {
        app.world.write_message(ClickEvent {
            entity,
            position: Vec2::new(3.0, 4.0),
            button: PointerButton::Primary,
            local: None,
        });
        app.tick();
    }

    fn hit(app: &lumen_core::app::App) -> Option<String> {
        app.world
            .resource::<PropertyStore>()
            .get_global_str("hit")
            .map(|s| s.to_string())
    }

    /// A click on a child runs the handler of the nearest ancestor that has
    /// one, and the event names that ancestor's id.
    #[test]
    fn a_click_runs_the_nearest_handler() {
        let mut app = app_with(vec![(
            EventKind::Click,
            Some("tool-pen".into()),
            Box::new(|ui, ev| {
                assert_eq!(ev.id(), Some("tool-pen"));
                assert_eq!(ev.position(), Some(Vec2::new(3.0, 4.0)));
                ui.set("hit", "pen");
            }),
        )]);
        let button = app.world.spawn(LumenId("tool-pen".into())).id();
        let icon = app.world.spawn(ChildOf(button)).id();
        click(&mut app, icon);
        assert_eq!(hit(&app).as_deref(), Some("pen"));
    }

    /// A module's custom event reaches the handler registered for its name
    /// on the nearest id, with what it carries and whether a DOM handler
    /// cancelled it; an event no handler names runs nothing.
    #[test]
    fn a_custom_event_runs_its_handler() {
        let mut app = app_with(vec![(
            EventKind::Custom("layoutchange"),
            Some("editor".into()),
            Box::new(|ui, ev| {
                assert_eq!(ev.kind(), EventKind::Custom("layoutchange"));
                assert_eq!(ev.id(), Some("editor"));
                assert!(ev.default_prevented());
                let reason = match ev.detail().and_then(|d| d.get("reason")) {
                    Some(lumen_script::ScriptValue::Str(s)) => s.clone(),
                    other => panic!("no reason: {other:?}"),
                };
                ui.set("hit", reason);
            }),
        )]);
        let dock = app.world.spawn(LumenId("editor".into())).id();
        let panel = app.world.spawn(ChildOf(dock)).id();
        let raise = |app: &mut lumen_core::app::App, name: &str| {
            app.world.write_message(CustomEventDelivered {
                event: lumen_script::CustomEvent::new(panel, name).with("reason", "user"),
                default_prevented: true,
            });
            app.tick();
        };
        raise(&mut app, "panelmove");
        assert_eq!(hit(&app), None, "nothing handles `panelmove`");
        raise(&mut app, "layoutchange");
        assert_eq!(hit(&app).as_deref(), Some("user"));
    }

    /// With no handler on the way up, the `on_any_*` handler runs with the
    /// nearest id.
    #[test]
    fn the_any_handler_gets_the_nearest_id() {
        let mut app = app_with(vec![(
            EventKind::Click,
            None,
            Box::new(|ui, ev| ui.set("hit", ev.id().unwrap_or("none"))),
        )]);
        let panel = app.world.spawn(LumenId("panel".into())).id();
        let label = app.world.spawn(ChildOf(panel)).id();
        click(&mut app, label);
        assert_eq!(hit(&app).as_deref(), Some("panel"));
    }

    /// A double-click is one double-click: the plain click on the same
    /// element that tick does not fire.
    #[test]
    fn a_double_click_suppresses_its_click() {
        let mut app = app_with(vec![
            (
                EventKind::Click,
                Some("b".into()),
                Box::new(|ui, _| ui.set("hit", "click")),
            ),
            (
                EventKind::DoubleClick,
                Some("b".into()),
                Box::new(|ui, _| ui.set("hit", "double")),
            ),
        ]);
        let b = app.world.spawn(LumenId("b".into())).id();
        app.world.write_message(DoubleClickEvent {
            entity: b,
            position: Vec2::ZERO,
        });
        click(&mut app, b);
        assert_eq!(hit(&app).as_deref(), Some("double"));
    }

    /// A toggle hands its new state to the handler.
    #[test]
    fn a_toggle_carries_its_state() {
        let mut app = app_with(vec![(
            EventKind::Toggle,
            Some("dark".into()),
            Box::new(|ui, ev| ui.set("dark_on", ev.checked().unwrap_or(false))),
        )]);
        let t = app.world.spawn(LumenId("dark".into())).id();
        app.world.write_message(ToggleChanged {
            entity: t,
            checked: true,
        });
        app.tick();
        assert_eq!(
            app.world
                .resource::<PropertyStore>()
                .get_global_bool("dark_on"),
            Some(true)
        );
    }
}
