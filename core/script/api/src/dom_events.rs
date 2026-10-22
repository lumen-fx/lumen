//! Runtime event dispatch for the dynamic DOM API (phase 4).
//!
//! These systems turn the input pipeline's typed messages (clicks, pointer
//! moves, wheel, keys, focus changes, text commits, scroll) into DOM events
//! and route each one through the capture -> target -> bubble propagation
//! driver in [`crate::event`], once, whatever the number of script hosts the
//! app runs. A handler bound with `n.on(type, handler)` runs here, in the host
//! that bound it or natively; commands it queues are forwarded onto the
//! [`ScriptCommandEvent`] bus so the normal appliers pick them up.
//!
//! The full event set: `click`, `dblclick`, `contextmenu`, `pointerdown` /
//! `pointerup` / `pointermove` / `pointercancel` / `pointerenter` /
//! `pointerleave`, `gotpointercapture` / `lostpointercapture`, `wheel`,
//! `keydown` / `keyup`, `input`, `change`, `focus`, `blur`, `submit`,
//! `scroll`.
//!
//! Sources and current limitations:
//! - Pointer events target the entity under the pointer: the one carrying
//!   `Hovered` for the primary pointer, the one in `PointerTargets` for a
//!   second finger. A pointer captured with `set_pointer_capture` targets
//!   its capturing node instead, wherever it is, and a touch is captured by
//!   the node it went down on, as on the web. `pointerenter` /
//!   `pointerleave` come from the hover chain (`HoverWithin`): as on the
//!   web, an element gets `pointerenter` when the pointer reaches it or one
//!   of its descendants and `pointerleave` when the pointer is over neither,
//!   so crossing from a button onto its icon fires `pointerenter` on the
//!   icon only. Entered elements hear it outermost first, left ones
//!   innermost first.
//! - `keydown` targets the focused entity (from the input router's
//!   `FocusedKey`); `keyup` targets the focused entity. With nothing focused
//!   both go to the document root, the way a browser sends them to the body,
//!   so a listener on the document hears app-wide shortcuts.
//! - `input` fires per edit, from the `TextEditApplied` signal the text
//!   pipeline raises (or a page, whose fields the browser edits): one event
//!   per keystroke, paste, or IME commit that changes the text, at most one
//!   per entity per tick, carrying the field's text after the edit. A pure
//!   caret move is not an edit and fires nothing.
//! - `change` and `submit` fire on commit, from the input router's
//!   `TextInputCommitted` signal; `submit` is the Enter-commit on a
//!   single-line input.
//! - A control that is not a text field (a color field) raises
//!   [`ControlInput`]: `input` for every new value, and `change` when the
//!   user commits the one they settled on.
//! - A widget's own internal parts ([`WidgetInternal`], a color panel's hex
//!   field) raise no `input`, `change` or `submit`; the widget raises its
//!   own.
//! - `scroll` comes from a changed scroll offset and does not bubble.
//! - `contextmenu` comes from the input layer's [`ContextMenuEvent`]: a
//!   secondary-button press (a release on Windows), the ContextMenu key or
//!   Shift+F10, or an assistive technology's request.
//!
//! - A custom event is one a module raises on an element with a
//!   [`CustomEvent`] message: a name of its own and a detail map. It
//!   travels the same capture -> target -> bubble path, so `n.on(name, ..)`
//!   in any host and a native handler hear it like a built-in one.
//!
//! Default actions: `click` (link navigation via `<a href>`), `contextmenu`
//! (the browser's own menu, on a page) and `submit` (form submission) have
//! one. `prevent_default` on a `click` or a `contextmenu` records the target
//! so whoever runs the default skips it; `submit`'s default is reserved
//! (there is no form-submission model yet). A desktop window opens no menu
//! of its own, so there a `contextmenu` does nothing unless a handler opens
//! one.

use bevy_ecs::component::Mutable;
use bevy_ecs::message::{Message, MessageReader, MessageRegistry, Messages};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::WidgetInternal;
use lumen_core::input::ControlInput;
use lumen_core::prelude::*;

use crate::ScriptHost;
use crate::event::{self, EventData};
use crate::runtime::ScriptCommandEvent;

/// Root-first ancestor chain (packed handles) of `entity`, excluding itself,
/// read from the current DOM snapshot. Empty when `entity` is not (yet) in
/// the snapshot.
fn ancestors_root_first(entity: Entity) -> Vec<u64> {
    let idx = lumen_core::node::dom_index_snapshot();
    let mut chain: Vec<u64> = idx
        .ancestors(entity)
        .into_iter()
        .map(|e| lumen_core::node::NodeHandle::new(e).pack())
        .collect();
    chain.reverse();
    chain
}

/// The web `button` code a pointer event carries.
fn button_code(button: PointerButton) -> i64 {
    button.code()
}

/// W3C-ish key name for the event object's `key`.
fn key_string(key: &Key) -> String {
    match key {
        Key::Character(s) => s.clone(),
        Key::Named(n) => match n {
            NamedKey::Tab => "Tab",
            NamedKey::Enter => "Enter",
            NamedKey::Escape => "Escape",
            NamedKey::Backspace => "Backspace",
            NamedKey::Space => "Space",
            NamedKey::ArrowUp => "ArrowUp",
            NamedKey::ArrowDown => "ArrowDown",
            NamedKey::ArrowLeft => "ArrowLeft",
            NamedKey::ArrowRight => "ArrowRight",
            NamedKey::Home => "Home",
            NamedKey::End => "End",
            NamedKey::Delete => "Delete",
        }
        .to_string(),
    }
}

/// An element event a module raises: the DOM's `CustomEvent`.
///
/// Write it as a message and it is delivered in [`ScriptSet::DomState`]
/// (`crate::ScriptSet`) to every handler bound with `n.on(name, ..)`, in
/// every script host, the C ABI and the SDKs, along the target's capture ->
/// target -> bubble path. A handler reads [`detail`](Self::detail) with
/// `ev.detail()`. After delivery a [`CustomEventDelivered`] reports whether a
/// handler called `prevent_default`, which is what a [`cancelable`]
/// (Self::cancelable) event asks: a module that asks "may this panel close?"
/// raises a cancelable event and acts on the answer.
///
/// The built-in event names (`click`, `input`, and the rest of the set this
/// module's documentation lists) belong to the input layer; a custom event
/// named after one is refused with a warning.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct CustomEvent {
    /// The element it fires on.
    pub target: Entity,
    /// The event type a handler binds: `n.on(name, ..)`.
    pub name: String,
    /// What it carries, read with `ev.detail()`.
    pub detail: std::collections::HashMap<String, crate::ScriptValue>,
    /// Whether it bubbles to the target's ancestors after the target.
    pub bubbles: bool,
    /// Whether a handler's `prevent_default` means something to whoever
    /// raised it.
    pub cancelable: bool,
}

impl CustomEvent {
    /// A bubbling, not cancelable event `name` on `target`, carrying
    /// nothing.
    pub fn new(target: Entity, name: impl Into<String>) -> Self {
        Self {
            target,
            name: name.into(),
            detail: std::collections::HashMap::new(),
            bubbles: true,
            cancelable: false,
        }
    }

    /// The same event carrying `value` under `key` in its detail.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<crate::ScriptValue>) -> Self {
        self.detail.insert(key.into(), value.into());
        self
    }
}

/// A [`CustomEvent`] that reached its handlers, and whether one of them
/// called `prevent_default`.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct CustomEventDelivered {
    /// The event as it was raised.
    pub event: CustomEvent,
    /// Whether a handler called `prevent_default` (always `false` for an
    /// event that is not [`cancelable`](CustomEvent::cancelable)).
    pub default_prevented: bool,
}

/// The names the built-in DOM events use, which a [`CustomEvent`] may not
/// take.
pub const BUILT_IN_EVENTS: &[&str] = &[
    "click",
    "dblclick",
    "contextmenu",
    "pointerdown",
    "pointerup",
    "pointermove",
    "pointercancel",
    "pointerenter",
    "pointerleave",
    "gotpointercapture",
    "lostpointercapture",
    "wheel",
    "keydown",
    "keyup",
    "input",
    "change",
    "focus",
    "blur",
    "submit",
    "scroll",
];

/// The DOM events read from this tick's input, waiting for delivery.
///
/// Reading the input and delivering it are two steps because a handler can
/// live in any of the app's hosts, and reaching a host needs the whole world:
/// the readers are ordinary systems that run beside the rest of the tick, and
/// the delivery is one exclusive pass that walks each event's propagation path
/// once, whatever the number of hosts.
#[derive(Resource, Default)]
pub struct PendingDomEvents {
    input: Vec<QueuedInput>,
    state: Vec<(EventData, Entity)>,
    custom: Vec<CustomEvent>,
}

/// One input event waiting for delivery.
struct QueuedInput {
    data: EventData,
    /// The entity under the pointer (or the focused one, for a key). A
    /// captured pointer's event goes to its capturing node instead, and can
    /// be delivered with nothing under it at all.
    target: Option<Entity>,
    /// What the event does to its pointer, for the capture bookkeeping;
    /// `None` for an event that is not a pointer's own.
    step: Option<PointerStep>,
}

/// What a pointer event does to its pointer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PointerStep {
    Down,
    Move,
    Up,
    Cancel,
}

/// How the dispatch reaches one installed host: offer it a handler token, and
/// forward the commands its handlers queued.
#[derive(Clone, Copy)]
pub struct EventHost {
    dispatch: fn(&mut World, u64) -> bool,
    drain: fn(&mut World),
}

/// The hosts DOM events are delivered into, one entry per installed host.
/// Empty for an app with no script: native (C-ABI and SDK) handlers still run.
#[derive(Resource, Default, Clone)]
pub struct EventHosts(Vec<EventHost>);

/// Put the host stored as the resource `H` on the list DOM events are
/// delivered into.
pub fn register_event_host<H: ScriptHost + Resource<Mutability = Mutable>>(world: &mut World) {
    world
        .get_resource_or_insert_with(EventHosts::default)
        .0
        .push(EventHost {
            dispatch: dispatch_into::<H>,
            drain: drain_into::<H>,
        });
}

/// Offer `token` to host `H`. A host answers for the tokens it minted and
/// passes on every other, so the first host that runs a handler is its owner.
fn dispatch_into<H: ScriptHost + Resource<Mutability = Mutable>>(
    world: &mut World,
    token: u64,
) -> bool {
    world
        .get_resource_mut::<H>()
        .is_some_and(|mut host| !matches!(host.dispatch_event_handler(token), Ok(false)))
}

/// Forward the commands host `H`'s handlers queued onto the command bus.
fn drain_into<H: ScriptHost + Resource<Mutability = Mutable>>(world: &mut World) {
    let commands = match world.get_resource_mut::<H>() {
        Some(mut host) => host.drain_commands(),
        None => return,
    };
    for c in commands {
        world.write_message(ScriptCommandEvent(c));
    }
}

/// Deliver the pointer, click, wheel and key events read this tick.
pub fn deliver_input_events(world: &mut World) {
    // Fresh per-tick prevented-default set for whoever runs a default
    // action (the anchor-nav executor, a page's context menu).
    event::clear_prevented_defaults();
    let Some(mut pending) = world.get_resource_mut::<PendingDomEvents>() else {
        return;
    };
    let events = std::mem::take(&mut pending.input);
    if events.is_empty() {
        return;
    }
    let hosts = world
        .get_resource::<EventHosts>()
        .cloned()
        .unwrap_or_default();
    for queued in events {
        match queued.step {
            Some(step) => deliver_pointer(world, &hosts, queued, step),
            None => {
                if let Some(target) = queued.target {
                    deliver(world, &hosts, queued.data, target);
                }
            }
        }
    }
}

/// Deliver the focus, hover, edit, commit and scroll events read this tick.
pub fn deliver_state_events(world: &mut World) {
    let Some(mut pending) = world.get_resource_mut::<PendingDomEvents>() else {
        return;
    };
    let events = std::mem::take(&mut pending.state);
    if events.is_empty() {
        return;
    }
    let hosts = world
        .get_resource::<EventHosts>()
        .cloned()
        .unwrap_or_default();
    for (data, target) in events {
        deliver(world, &hosts, data, target);
    }
}

/// Read this tick's [`CustomEvent`]s for delivery.
pub fn queue_custom_events(
    mut raised: MessageReader<CustomEvent>,
    mut pending: ResMut<PendingDomEvents>,
) {
    for event in raised.read() {
        if event.name.is_empty() || BUILT_IN_EVENTS.contains(&event.name.as_str()) {
            lumen_core::warn_line!(
                "custom event `{}` refused: the name is empty or a built-in event's",
                event.name
            );
            continue;
        }
        pending.custom.push(event.clone());
    }
}

/// Deliver the custom events queued this tick, and report each one with a
/// [`CustomEventDelivered`].
pub fn deliver_custom_events(world: &mut World) {
    let Some(mut pending) = world.get_resource_mut::<PendingDomEvents>() else {
        return;
    };
    let events = std::mem::take(&mut pending.custom);
    if events.is_empty() {
        return;
    }
    let hosts = world
        .get_resource::<EventHosts>()
        .cloned()
        .unwrap_or_default();
    for event in events {
        let mut data = base(event.target, &event.name);
        data.detail = event.detail.clone();
        let prevented = world.get_entity(event.target).is_ok()
            && dispatch(world, &hosts, data, event.target, event.bubbles)
                .is_some_and(|result| result.default_prevented);
        world.write_message(CustomEventDelivered {
            default_prevented: prevented && event.cancelable,
            event,
        });
    }
}

/// The live entity a packed node handle names, if it still exists.
fn live_entity(world: &World, node: u64) -> Option<Entity> {
    let handle = lumen_core::node::NodeHandle::unpack(node)?;
    world.get_entity(handle.entity).ok().map(|e| e.id())
}

/// Deliver one pointer's event the way the web does around pointer capture:
/// the capture a handler asked for takes effect just before the pointer's
/// next event (`lostpointercapture` at the node that had it,
/// `gotpointercapture` at the one that gets it), a captured pointer's event
/// goes to the capturing node, and the capture ends after the pointer's last
/// button comes up.
fn deliver_pointer(world: &mut World, hosts: &EventHosts, queued: QueuedInput, step: PointerStep) {
    let QueuedInput { data, target, .. } = queued;
    let id = data.pointer.id;
    if step == PointerStep::Down {
        event::pointer_capture_down(id);
        // A touch is captured by the node it went down on (the web's
        // implicit capture for direct manipulation), so a finger that slides
        // off keeps talking to the node it touched.
        if data.pointer.kind == PointerKind::Touch
            && let Some(t) = target
        {
            event::set_pointer_capture(lumen_core::node::NodeHandle::new(t).pack(), id);
        }
    }
    apply_capture_change(world, hosts, &data);
    let captured = event::active_pointer_capture(id).and_then(|node| {
        let live = live_entity(world, node);
        if live.is_none() {
            // The capturing node left the tree: the capture ends and the
            // document hears about it.
            event::forget_pointer_capture(id);
            if let Some(doc) = lumen_core::node::dom_index_snapshot().document() {
                deliver(
                    world,
                    hosts,
                    capture_event(&data, doc, "lostpointercapture"),
                    doc,
                );
            }
        }
        live
    });
    if let Some(to) = captured.or(target) {
        let mut data = data.clone();
        if Some(to) != target {
            // Local coordinates follow the node the event is delivered to.
            let origin = world
                .get::<Transform>(to)
                .map(|t| t.absolute)
                .unwrap_or(Vec2::ZERO);
            data.local = (
                data.client.0 - origin.x as f64,
                data.client.1 - origin.y as f64,
            );
            data.target = lumen_core::node::NodeHandle::new(to).pack();
        }
        deliver(world, hosts, data, to);
    }
    let released = match step {
        PointerStep::Cancel => true,
        PointerStep::Up => data.pointer.buttons == 0,
        PointerStep::Down | PointerStep::Move => false,
    };
    if released {
        event::pointer_capture_up(id);
        apply_capture_change(world, hosts, &data);
    }
}

/// Fire `lostpointercapture` / `gotpointercapture` for a capture change
/// pending on `data`'s pointer, and make the change.
fn apply_capture_change(world: &mut World, hosts: &EventHosts, data: &EventData) {
    let id = data.pointer.id;
    let Some((lost, got)) = event::take_pointer_capture_change(id) else {
        return;
    };
    if let Some(e) = lost.and_then(|node| live_entity(world, node)) {
        deliver(
            world,
            hosts,
            capture_event(data, e, "lostpointercapture"),
            e,
        );
    }
    if let Some(node) = got {
        match live_entity(world, node) {
            Some(e) => deliver(world, hosts, capture_event(data, e, "gotpointercapture"), e),
            None => event::forget_pointer_capture(id),
        }
    }
}

/// A `gotpointercapture` / `lostpointercapture` event for `entity`, carrying
/// the pointer of the event that caused it.
fn capture_event(cause: &EventData, entity: Entity, event_type: &str) -> EventData {
    let mut data = base(entity, event_type);
    data.pointer = cause.pointer;
    data.client = cause.client;
    data.button = -1;
    data
}

/// Deliver one already-built [`EventData`] targeting `target_entity`:
/// resolve the propagation path, run the driver once (native callbacks
/// directly, host closures through whichever host owns the token), forward
/// queued commands, and record a default-prevented click.
fn deliver(world: &mut World, hosts: &EventHosts, data: EventData, target_entity: Entity) {
    let bubbles = event::event_bubbles(&data.event_type);
    let etype = data.event_type.clone();
    let target_handle = data.target;
    if let Some(result) = dispatch(world, hosts, data, target_entity, bubbles)
        && result.default_prevented
        && event::has_default_action(&etype)
    {
        event::mark_default_prevented(&etype, target_handle);
    }
}

/// Run one event's propagation through every host and forward the commands
/// its handlers queued. `None` when nothing is bound to the event's type.
fn dispatch(
    world: &mut World,
    hosts: &EventHosts,
    data: EventData,
    target_entity: Entity,
    bubbles: bool,
) -> Option<EventData> {
    if !event::has_bindings_for(&data.event_type) {
        return None;
    }
    let ancestors = ancestors_root_first(target_entity);
    // A handler's `window.*` calls apply to the window the event happened in.
    let window = world
        .get_resource::<lumen_core::window::WindowPartitions>()
        .and_then(|p| p.window_of(target_entity));
    lumen_core::window_state::set_current(window);
    let result = event::dispatch(data, &ancestors, bubbles, |token| {
        for host in &hosts.0 {
            if (host.dispatch)(world, token) {
                break;
            }
        }
    });
    lumen_core::window_state::set_current(None);
    for host in &hosts.0 {
        (host.drain)(world);
    }
    Some(result)
}

/// What a field holds, read by [`field_text`].
pub(crate) type FieldText = (
    Option<&'static lumen_core::text_model::TextBuffer>,
    Option<&'static TextContent>,
);

/// The text a field holds after an edit: its live buffer where the text
/// pipeline keeps one, otherwise its text, which is what a backend that edits
/// the field itself (a page) writes.
pub(crate) fn field_text(
    (buffer, text): (
        Option<&lumen_core::text_model::TextBuffer>,
        Option<&TextContent>,
    ),
) -> Option<String> {
    buffer
        .map(ToString::to_string)
        .or_else(|| text.map(|t| t.0.clone()))
}

/// Build the base [`EventData`] shell for `entity` and `event_type` with the
/// packed target handle filled in.
fn base(entity: Entity, event_type: &str) -> EventData {
    EventData {
        event_type: event_type.to_string(),
        target: lumen_core::node::NodeHandle::new(entity).pack(),
        ..Default::default()
    }
}

/// Fill in where the pointer was: `position` in window coordinates, and
/// relative to `entity`'s box. A backend that measured the box itself hands
/// the relative position over as `local`; otherwise it comes from the box
/// Lumen's layout gave the entity.
fn with_position(
    mut data: EventData,
    transforms: &Query<&Transform>,
    entity: Entity,
    position: Vec2,
    local: Option<Vec2>,
) -> EventData {
    let local = local.unwrap_or_else(|| {
        let origin = transforms
            .get(entity)
            .map(|t| t.absolute)
            .unwrap_or(Vec2::ZERO);
        position - origin
    });
    data.local = (local.x as f64, local.y as f64);
    data.client = (position.x as f64, position.y as f64);
    data
}

fn set_mods(data: &mut EventData, mods: &Modifiers) {
    data.shift = mods.shift;
    data.ctrl = mods.ctrl;
    data.alt = mods.alt;
    data.super_ = mods.super_;
}

/// Register every input message the DOM event queues read, where the world
/// does not have it yet.
///
/// A window's input layer registers these; a server render has no input
/// layer, and its queue systems would fail parameter validation reading a
/// message nobody registered. Registered, they stay empty.
pub fn register_dom_event_messages(world: &mut World) {
    fn register<M: Message>(world: &mut World) {
        if !world.contains_resource::<Messages<M>>() {
            MessageRegistry::register_message::<M>(world);
        }
    }
    register::<ClickEvent>(world);
    register::<DoubleClickEvent>(world);
    register::<ContextMenuEvent>(world);
    register::<PointerPressed>(world);
    register::<PointerReleased>(world);
    register::<PointerMoved>(world);
    register::<PointerCancelled>(world);
    register::<MouseWheel>(world);
    register::<KeyPressed>(world);
    register::<KeyReleased>(world);
    register::<FocusedKey>(world);
    register::<lumen_core::text_events::TextEditApplied>(world);
    register::<TextInputCommitted>(world);
    register::<ControlInput>(world);
    #[cfg(not(target_arch = "wasm32"))]
    {
        register::<CustomEvent>(world);
        register::<CustomEventDelivered>(world);
    }
}

/// The pointer messages [`queue_pointer_and_key_events`] reads, grouped so
/// the system stays under the parameter limit.
#[derive(bevy_ecs::system::SystemParam)]
pub struct PointerReaders<'w, 's> {
    presses: MessageReader<'w, 's, PointerPressed>,
    releases: MessageReader<'w, 's, PointerReleased>,
    moves: MessageReader<'w, 's, PointerMoved>,
    cancels: Option<MessageReader<'w, 's, PointerCancelled>>,
    targets: Option<Res<'w, PointerTargets>>,
}

/// Read this tick's pointer, click, wheel and key input as DOM events, for
/// [`deliver_input_events`]. Pointer events target the hovered entity (a
/// second finger, the entity in [`PointerTargets`]); key events target the
/// focused entity, or the document root when nothing is focused.
#[allow(clippy::too_many_arguments)]
pub fn queue_pointer_and_key_events(
    mut pending: ResMut<PendingDomEvents>,
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    mut menus: MessageReader<ContextMenuEvent>,
    mut pointers: PointerReaders,
    mut wheels: MessageReader<MouseWheel>,
    mut keyups: MessageReader<KeyReleased>,
    mut keydowns: MessageReader<FocusedKey>,
    mut key_presses: MessageReader<KeyPressed>,
    focus: Option<Res<FocusTracker>>,
    transforms: Query<&Transform>,
    hovered: Query<Entity, With<Hovered>>,
    focused: Query<Entity, With<Focused>>,
) {
    let queue = &mut pending.input;
    let hovered_entity = hovered.iter().next();
    let focused_entity = focused.iter().next();
    let plain = |data: EventData, target: Entity| QueuedInput {
        data,
        target: Some(target),
        step: None,
    };

    // click (targets the clicked entity directly).
    for c in clicks.read() {
        let mut data = with_position(
            base(c.entity, "click"),
            &transforms,
            c.entity,
            c.position,
            c.local,
        );
        data.button = button_code(c.button);
        queue.push(plain(data, c.entity));
    }
    // dblclick.
    for d in doubles.read() {
        let data = with_position(
            base(d.entity, "dblclick"),
            &transforms,
            d.entity,
            d.position,
            None,
        );
        queue.push(plain(data, d.entity));
    }
    // contextmenu targets the element the request was for: the one under
    // the pointer, or the focused one for a keyboard request.
    for m in menus.read() {
        let mut data = with_position(
            base(m.entity, "contextmenu"),
            &transforms,
            m.entity,
            m.position,
            m.local,
        );
        data.button = button_code(m.button);
        queue.push(plain(data, m.entity));
    }
    // pointerdown / up / move / cancel target the entity under their pointer,
    // in the order they happened: a quick stroke presses, moves and releases
    // inside one frame, and its moves belong between the press and the
    // release.
    let start = queue.len();
    let under = |pointer: &PointerData| {
        if pointer.is_primary {
            hovered_entity
        } else {
            pointers.targets.as_ref().and_then(|t| t.get(pointer.id))
        }
    };
    let pointer_event = |event_type: &str,
                         target: Option<Entity>,
                         position: Vec2,
                         local: Option<Vec2>,
                         pointer: PointerData,
                         button: i64| {
        let mut data = match target {
            Some(e) => with_position(base(e, event_type), &transforms, e, position, local),
            None => EventData {
                event_type: event_type.to_string(),
                client: (position.x as f64, position.y as f64),
                ..Default::default()
            },
        };
        data.pointer = pointer;
        data.button = button;
        data
    };
    for p in pointers.presses.read() {
        let target = under(&p.pointer);
        let data = pointer_event(
            "pointerdown",
            target,
            p.position,
            p.local,
            p.pointer,
            button_code(p.button),
        );
        queue.push(QueuedInput {
            data,
            target,
            step: Some(PointerStep::Down),
        });
    }
    for p in pointers.releases.read() {
        let target = under(&p.pointer);
        let data = pointer_event(
            "pointerup",
            target,
            p.position,
            p.local,
            p.pointer,
            button_code(p.button),
        );
        queue.push(QueuedInput {
            data,
            target,
            step: Some(PointerStep::Up),
        });
    }
    for p in pointers.moves.read() {
        let target = under(&p.pointer);
        // A move over nothing reaches a script only through a capture.
        if target.is_none() && !event::any_pointer_capture() {
            continue;
        }
        let data = pointer_event("pointermove", target, p.position, p.local, p.pointer, -1);
        queue.push(QueuedInput {
            data,
            target,
            step: Some(PointerStep::Move),
        });
    }
    for p in pointers.cancels.iter_mut().flat_map(|c| c.read()) {
        let target = under(&p.pointer);
        let data = pointer_event("pointercancel", target, p.position, None, p.pointer, -1);
        queue.push(QueuedInput {
            data,
            target,
            step: Some(PointerStep::Cancel),
        });
    }
    queue[start..].sort_by_key(|q| q.data.pointer.order);
    for w in wheels.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(base(e, "wheel"), &transforms, e, w.position, w.local);
        data.delta = (w.delta.x as f64, w.delta.y as f64);
        queue.push(plain(data, e));
    }
    // keydown targets the entity the input router routed the key to.
    for k in keydowns.read() {
        let mut data = base(k.entity, "keydown");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        queue.push(plain(data, k.entity));
    }
    // The router routes nothing while no element has focus (the same
    // `FocusTracker` it reads), and Tab is the focus move, not a key for
    // the page. Those keys go to the document root instead.
    let nothing_focused = focus.is_none_or(|f| f.0.is_none());
    let document = || lumen_core::node::dom_index_snapshot().document();
    for k in key_presses.read() {
        if !nothing_focused || matches!(k.key, Key::Named(NamedKey::Tab)) {
            continue;
        }
        let Some(doc) = document() else { continue };
        let mut data = base(doc, "keydown");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        queue.push(plain(data, doc));
    }
    // keyup targets the focused entity, or the document root.
    for k in keyups.read() {
        let Some(e) = focused_entity.or_else(document) else {
            continue;
        };
        let mut data = base(e, "keyup");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        queue.push(plain(data, e));
    }
}

/// Read this tick's focus, hover, edit, commit and scroll changes as DOM
/// events, for [`deliver_state_events`].
#[allow(clippy::too_many_arguments)]
pub fn queue_state_events(
    mut pending: ResMut<PendingDomEvents>,
    mut edits: MessageReader<lumen_core::text_events::TextEditApplied>,
    mut commits: MessageReader<TextInputCommitted>,
    mut controls: MessageReader<ControlInput>,
    internal: Query<(), With<WidgetInternal>>,
    gained_focus: Query<Entity, Added<Focused>>,
    mut lost_focus: RemovedComponents<Focused>,
    gained_hover: Query<Entity, Added<HoverWithin>>,
    mut lost_hover: RemovedComponents<HoverWithin>,
    parents: Query<&ChildOf>,
    scrolled: Query<Entity, Changed<ScrollOffset>>,
    fields: Query<FieldText>,
) {
    // A binding keys on a node's packed handle (entity + generation), so a
    // despawned + recycled entity never matches a live binding; the
    // has-bindings gate inside `deliver` also makes an unbound remove a
    // no-op. No explicit liveness check is needed here.
    let queue = &mut pending.state;

    // focus / blur.
    for e in gained_focus.iter() {
        queue.push((base(e, "focus"), e));
    }
    for e in lost_focus.read() {
        queue.push((base(e, "blur"), e));
    }
    // pointerenter / pointerleave. Entered elements outermost first; the
    // hit test removes the chain innermost first, which is the order the
    // removals read back in.
    let mut entered: Vec<(usize, Entity)> = gained_hover
        .iter()
        .map(|e| (parents.iter_ancestors(e).count(), e))
        .collect();
    entered.sort_unstable();
    for (_, e) in entered {
        queue.push((base(e, "pointerenter"), e));
    }
    for e in lost_hover.read() {
        queue.push((base(e, "pointerleave"), e));
    }
    // input, once per edit that changed the text. An entity gets at most
    // one `input` per tick: an IME commit both mutates the buffer and
    // raises `TextInputCommitted`, and that is one edit to a handler, not
    // two. The value is the live buffer, so a handler reads the text as it
    // stands after the edit it was told about.
    let mut fired: Vec<Entity> = Vec::new();
    for ev in edits.read() {
        if matches!(ev.kind, lumen_core::text_events::AppliedKind::CursorMove)
            || fired.contains(&ev.entity)
            || internal.contains(ev.entity)
        {
            continue;
        }
        let Some(value) = fields.get(ev.entity).ok().and_then(field_text) else {
            continue;
        };
        fired.push(ev.entity);
        let mut data = base(ev.entity, "input");
        data.value = value;
        queue.push((data, ev.entity));
    }
    // change / submit from the commit signal (Enter on a single-line
    // input, or focus leaving a committed field).
    for c in commits.read() {
        if internal.contains(c.entity) {
            continue;
        }
        for etype in ["change", "submit"] {
            let mut data = base(c.entity, etype);
            data.value = c.text.clone();
            queue.push((data, c.entity));
        }
    }
    // input for each new value, and change for the commit, from a control
    // that is not a text field.
    for c in controls.read() {
        let etype = if c.committed { "change" } else { "input" };
        let mut data = base(c.entity, etype);
        data.value = c.value.clone();
        queue.push((data, c.entity));
    }
    // scroll (does not bubble).
    for e in scrolled.iter() {
        queue.push((base(e, "scroll"), e));
    }
}

#[cfg(test)]
pub(crate) mod text_event_tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::text_events::{AppliedKind, TextEditApplied};
    use lumen_core::text_model::TextBuffer;
    use std::sync::{Arc, Mutex, MutexGuard};

    /// The binding registry and the current-event cell are process-wide,
    /// so every test that touches them takes the same turn-taking lock.
    fn serial() -> MutexGuard<'static, ()> {
        event::TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A host stub with unimplemented / trivial bodies, for the tests that
    /// need a type to name a host by. `pub(crate)` so `runtime`'s derivation
    /// tests can build on it instead of writing a second one.
    #[derive(Resource)]
    pub(crate) struct NoHost;

    impl ScriptHost for NoHost {
        type Closure = ();
        fn compile_check(&self, _source: &str, _uri: &str) -> Result<(), crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn load(&mut self, _source: &str, _uri: &str) -> Result<(), crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn replace(&mut self, _source: &str, _uri: &str) -> Result<(), crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn reset(&mut self) {
            unimplemented!("no host in these tests")
        }
        fn call(
            &mut self,
            _fn_name: &str,
            _args: &[crate::ScriptValue],
        ) -> Result<crate::CallOutcome, crate::CallFailure> {
            unimplemented!("no host in these tests")
        }
        fn call_closure(
            &mut self,
            _closure: &Self::Closure,
            _args: &[crate::ScriptValue],
        ) -> Result<crate::ScriptValue, crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn register_script_fn(&mut self, _f: &crate::ScriptFn) -> Result<(), crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn lang(&self) -> &'static str {
            "test"
        }
    }

    /// Record `(event type, value)` for every event delivered to `node`.
    fn watch(node: u64, types: &[&str]) -> Arc<Mutex<Vec<(String, String)>>> {
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        for t in types {
            let sink = Arc::clone(&seen);
            event::register_native_binding(
                node,
                (*t).to_string(),
                false,
                Arc::new(move || {
                    if let Ok(mut v) = sink.lock() {
                        v.push((event::event_type(), event::event_value()));
                    }
                }),
            );
        }
        seen
    }

    /// One tick. `run_system_once` builds fresh system state every call, so
    /// its readers start at the front of the buffer; draining after each
    /// run is what keeps a tick from re-reading the previous tick's
    /// messages.
    fn drive(world: &mut World) {
        world.init_resource::<PendingDomEvents>();
        world.init_resource::<Messages<ControlInput>>();
        world
            .run_system_once(queue_state_events)
            .expect("system ran");
        deliver_state_events(world);
        world.resource_mut::<Messages<TextEditApplied>>().clear();
        world.resource_mut::<Messages<TextInputCommitted>>().clear();
    }

    #[test]
    fn input_fires_per_edit_and_change_only_on_commit() {
        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        world.init_resource::<Messages<TextEditApplied>>();
        world.init_resource::<Messages<TextInputCommitted>>();
        let field = world.spawn(TextBuffer::single_line("ab")).id();
        let node = lumen_core::node::NodeHandle::new(field).pack();
        let seen = watch(node, &["input", "change", "submit"]);

        // One edit this tick: `input` only, carrying the live buffer.
        world
            .resource_mut::<Messages<TextEditApplied>>()
            .write(TextEditApplied {
                entity: field,
                version: 1,
                kind: AppliedKind::Insert,
                before_byte: 1,
                after_byte: 2,
            });
        drive(&mut world);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![("input".to_string(), "ab".to_string())],
        );

        // A caret move is not an edit.
        seen.lock().unwrap().clear();
        world
            .resource_mut::<Messages<TextEditApplied>>()
            .write(TextEditApplied {
                entity: field,
                version: 2,
                kind: AppliedKind::CursorMove,
                before_byte: 2,
                after_byte: 0,
            });
        drive(&mut world);
        assert!(seen.lock().unwrap().is_empty(), "caret moves fire nothing");

        // A commit fires change + submit, and no second `input`.
        world
            .resource_mut::<Messages<TextInputCommitted>>()
            .write(TextInputCommitted {
                entity: field,
                text: "ab".to_string(),
            });
        drive(&mut world);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("change".to_string(), "ab".to_string()),
                ("submit".to_string(), "ab".to_string()),
            ],
        );
        event::clear_all_bindings();
    }

    /// `pointerenter` / `pointerleave` follow the hover chain, as on the
    /// web: moving from a button onto its icon enters the icon only, the
    /// button hears nothing, and leaving both fires `pointerleave` on the
    /// icon first. Entering from outside fires outermost first.
    #[test]
    fn pointerenter_and_pointerleave_follow_the_hover_chain() {
        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        world.init_resource::<Messages<TextEditApplied>>();
        world.init_resource::<Messages<TextInputCommitted>>();
        let button = world.spawn_empty().id();
        let icon = world.spawn(ChildOf(button)).id();
        let seen: Arc<Mutex<Vec<(String, &'static str)>>> = Arc::default();
        for (e, name) in [(button, "button"), (icon, "icon")] {
            let node = lumen_core::node::NodeHandle::new(e).pack();
            for t in ["pointerenter", "pointerleave"] {
                let sink = Arc::clone(&seen);
                event::register_native_binding(
                    node,
                    t.to_string(),
                    false,
                    Arc::new(move || sink.lock().unwrap().push((event::event_type(), name))),
                );
            }
        }
        let hover = |world: &mut World, hit: Option<Entity>| {
            world
                .run_system_once(
                    move |mut commands: Commands,
                          parents: Query<&ChildOf>,
                          chain: Query<Entity, With<HoverWithin>>| {
                        lumen_core::input::reconcile_hover_chain(
                            &mut commands,
                            hit,
                            &parents,
                            &chain,
                        );
                    },
                )
                .unwrap();
        };
        // A registered system keeps its change ticks and removal cursor
        // between runs, as the schedule does; `run_system_once` would see
        // every marker as newly added.
        world.init_resource::<PendingDomEvents>();
        world.init_resource::<Messages<ControlInput>>();
        let queue = world.register_system(queue_state_events);
        let drive = |world: &mut World| {
            world.run_system(queue).expect("system ran");
            deliver_state_events(world);
        };
        let take = |seen: &Arc<Mutex<Vec<(String, &'static str)>>>| {
            std::mem::take(&mut *seen.lock().unwrap())
                .into_iter()
                .map(|(t, n)| format!("{t} {n}"))
                .collect::<Vec<_>>()
        };

        hover(&mut world, Some(button));
        drive(&mut world);
        assert_eq!(take(&seen), ["pointerenter button"]);

        hover(&mut world, Some(icon));
        drive(&mut world);
        assert_eq!(
            take(&seen),
            ["pointerenter icon"],
            "the button stays entered"
        );

        hover(&mut world, None);
        drive(&mut world);
        assert_eq!(take(&seen), ["pointerleave icon", "pointerleave button"]);

        hover(&mut world, Some(icon));
        drive(&mut world);
        assert_eq!(take(&seen), ["pointerenter button", "pointerenter icon"]);
    }

    #[test]
    fn repeated_edits_in_one_tick_fire_input_once() {
        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        world.init_resource::<Messages<TextEditApplied>>();
        world.init_resource::<Messages<TextInputCommitted>>();
        let field = world.spawn(TextBuffer::single_line("hi")).id();
        let node = lumen_core::node::NodeHandle::new(field).pack();
        let seen = watch(node, &["input"]);

        // An IME commit mutates the buffer and raises the commit signal in
        // the same tick; a handler sees one edit, not two.
        for version in 1..=3 {
            world
                .resource_mut::<Messages<TextEditApplied>>()
                .write(TextEditApplied {
                    entity: field,
                    version,
                    kind: AppliedKind::Insert,
                    before_byte: 0,
                    after_byte: 1,
                });
        }
        drive(&mut world);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "one `input` per entity per tick"
        );
        event::clear_all_bindings();
    }

    /// With nothing focused a key goes to the document root, as a browser
    /// sends it to the body, so a listener on the document hears it. A
    /// focused element still takes the key itself.
    #[test]
    fn keys_with_nothing_focused_reach_the_document() {
        use bevy_ecs::system::RunSystemOnce;
        use lumen_core::node::{DomIndex, DomRecord, publish_dom_index};

        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        register_dom_event_messages(&mut world);
        world.init_resource::<FocusTracker>();
        world.init_resource::<PendingDomEvents>();
        let root = world.spawn_empty().id();
        let button = world.spawn_empty().id();
        let record = |entity: Entity, parent: Option<Entity>, children: Vec<Entity>| DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: String::new(),
            id: None,
            classes: Vec::new(),
            parent,
            children,
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        };
        publish_dom_index(DomIndex::build(vec![
            record(root, None, vec![button]),
            record(button, Some(root), Vec::new()),
        ]));
        let doc = lumen_core::node::NodeHandle::new(root).pack();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        for t in ["keydown", "keyup"] {
            let sink = Arc::clone(&seen);
            event::register_native_binding(
                doc,
                t.to_string(),
                false,
                Arc::new(move || {
                    let target = lumen_core::node::NodeHandle::unpack(event::event_target())
                        .map(|h| h.entity);
                    sink.lock()
                        .unwrap()
                        .push(format!("{} {:?}", event::event_type(), target));
                }),
            );
        }
        let press = |world: &mut World, key: Key| {
            world.write_message(KeyPressed {
                key: key.clone(),
                modifiers: Modifiers::default(),
                repeat: false,
                time_stamp: lumen_core::time::Instant::now(),
            });
            world.write_message(KeyReleased {
                key,
                modifiers: Modifiers::default(),
                time_stamp: lumen_core::time::Instant::now(),
            });
            world
                .run_system_once(queue_pointer_and_key_events)
                .expect("system ran");
            deliver_input_events(world);
            world.resource_mut::<Messages<KeyPressed>>().clear();
            world.resource_mut::<Messages<KeyReleased>>().clear();
        };

        press(&mut world, Key::Named(NamedKey::Escape));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                format!("keydown {:?}", Some(root)),
                format!("keyup {:?}", Some(root)),
            ],
            "an unfocused key reaches the document"
        );

        // A focused element takes the key: the router's `FocusedKey` is the
        // keydown, and the raw press is not delivered a second time.
        seen.lock().unwrap().clear();
        world.resource_mut::<FocusTracker>().0 = Some(button);
        world.entity_mut(button).insert(Focused);
        world.write_message(FocusedKey {
            entity: button,
            key: Key::Named(NamedKey::Escape),
            modifiers: Modifiers::default(),
            repeat: false,
        });
        press(&mut world, Key::Named(NamedKey::Escape));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                format!("keydown {:?}", Some(button)),
                format!("keyup {:?}", Some(button)),
            ],
            "a focused key bubbles up from the element"
        );
        event::clear_all_bindings();
        publish_dom_index(DomIndex::default());
    }

    /// One pointer tick: queue this tick's pointer messages, deliver them,
    /// and clear the buffers so the next tick starts empty.
    fn pointer_tick(world: &mut World) {
        world
            .run_system_once(queue_pointer_and_key_events)
            .expect("system ran");
        deliver_input_events(world);
        world.resource_mut::<Messages<PointerPressed>>().clear();
        world.resource_mut::<Messages<PointerReleased>>().clear();
        world.resource_mut::<Messages<PointerMoved>>().clear();
        world.resource_mut::<Messages<PointerCancelled>>().clear();
    }

    /// A world with two boxes side by side, `a` at x 0..100 and `b` at
    /// x 100..200, ready for pointer ticks.
    fn two_boxes() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        register_dom_event_messages(&mut world);
        world.init_resource::<PendingDomEvents>();
        world.init_resource::<PointerTargets>();
        let at = |x: f32| Transform {
            absolute: Vec2::new(x, 0.0),
            size: Vec2::new(100.0, 100.0),
            ..Default::default()
        };
        let a = world.spawn(at(0.0)).id();
        let b = world.spawn(at(100.0)).id();
        (world, a, b)
    }

    /// What a handler saw: `(type, target, local x, pressure, buttons)`.
    type Seen = Arc<Mutex<Vec<(String, Option<Entity>, f64, f32, u32)>>>;

    fn record(node: u64, types: &[&str], seen: &Seen) {
        for t in types {
            let sink = Arc::clone(seen);
            event::register_native_binding(
                node,
                (*t).to_string(),
                false,
                Arc::new(move || {
                    let target = lumen_core::node::NodeHandle::unpack(event::event_target())
                        .map(|h| h.entity);
                    let p = event::event_pointer();
                    sink.lock().unwrap().push((
                        event::event_type(),
                        target,
                        event::event_position_local().0,
                        p.pressure,
                        p.buttons,
                    ));
                }),
            );
        }
    }

    fn pen(buttons: u32, pressure: f32) -> PointerData {
        PointerData {
            id: 7,
            kind: PointerKind::Pen,
            is_primary: true,
            buttons,
            pressure,
            tilt_x: 30.0,
            ..PointerData::default()
        }
    }

    /// `set_pointer_capture` from a `pointerdown` handler aims the pointer's
    /// later events at the node wherever the pointer goes, as on the web:
    /// `gotpointercapture` before the next event, `lostpointercapture` after
    /// the last button comes up, and the pen's readings on every event.
    #[test]
    fn a_captured_pointer_keeps_talking_to_its_node() {
        let _guard = serial();
        event::clear_all_bindings();
        let (mut world, a, b) = two_boxes();
        let node_a = lumen_core::node::NodeHandle::new(a).pack();
        let node_b = lumen_core::node::NodeHandle::new(b).pack();
        let seen: Seen = Arc::default();
        record(
            node_a,
            &[
                "pointermove",
                "pointerup",
                "gotpointercapture",
                "lostpointercapture",
            ],
            &seen,
        );
        record(node_b, &["pointermove"], &seen);
        let captured = Arc::new(Mutex::new(None::<bool>));
        let flag = Arc::clone(&captured);
        event::register_native_binding(
            node_a,
            "pointerdown".to_string(),
            false,
            Arc::new(move || {
                let id = event::event_pointer().id;
                assert!(!event::has_pointer_capture(node_a, id));
                event::set_pointer_capture(node_a, id);
                *flag.lock().unwrap() = Some(event::has_pointer_capture(node_a, id));
            }),
        );

        // No button down: a capture cannot be set.
        assert!(!event::set_pointer_capture(node_a, 7));

        world.entity_mut(a).insert(Hovered);
        world.write_message(PointerPressed {
            position: Vec2::new(50.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: pen(1, 0.4),
        });
        pointer_tick(&mut world);
        assert_eq!(
            *captured.lock().unwrap(),
            Some(true),
            "has_pointer_capture reads the capture at once"
        );

        // The pen slides onto `b`: the move still goes to `a`, after `a` is
        // told it got the pointer, with `a`'s local coordinates.
        world.entity_mut(a).remove::<Hovered>();
        world.entity_mut(b).insert(Hovered);
        world.write_message(PointerMoved {
            position: Vec2::new(150.0, 10.0),
            local: None,
            pointer: pen(1, 0.8),
        });
        pointer_tick(&mut world);
        // And off every element (or out of the window): still `a`.
        world.entity_mut(b).remove::<Hovered>();
        world.write_message(PointerMoved {
            position: Vec2::new(500.0, 10.0),
            local: None,
            pointer: pen(1, 0.9),
        });
        pointer_tick(&mut world);
        world.write_message(PointerReleased {
            position: Vec2::new(500.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: pen(0, 0.0),
        });
        pointer_tick(&mut world);
        // Released: a move over `b` goes to `b` again.
        world.entity_mut(b).insert(Hovered);
        world.write_message(PointerMoved {
            position: Vec2::new(150.0, 10.0),
            local: None,
            pointer: pen(0, 0.0),
        });
        pointer_tick(&mut world);

        let s = |t: &str| t.to_string();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (s("gotpointercapture"), Some(a), 0.0, 0.8, 1),
                (s("pointermove"), Some(a), 150.0, 0.8, 1),
                (s("pointermove"), Some(a), 500.0, 0.9, 1),
                (s("pointerup"), Some(a), 500.0, 0.0, 0),
                (s("lostpointercapture"), Some(a), 0.0, 0.0, 0),
                (s("pointermove"), Some(b), 50.0, 0.0, 0),
            ]
        );
        assert!(!event::has_pointer_capture(node_a, 7));
        assert!(!event::any_pointer_capture());
        event::clear_all_bindings();
    }

    /// A stroke that presses, moves and releases inside one tick reaches the
    /// handlers in that order, though the three come in on three message
    /// queues.
    #[test]
    fn a_stroke_inside_one_tick_keeps_its_order() {
        let _guard = serial();
        event::clear_all_bindings();
        let (mut world, a, _) = two_boxes();
        let node_a = lumen_core::node::NodeHandle::new(a).pack();
        let seen: Seen = Arc::default();
        record(node_a, &["pointerdown", "pointermove", "pointerup"], &seen);
        world.entity_mut(a).insert(Hovered);
        let down = pen(1, 0.2).stamped();
        let moved = pen(1, 0.6).stamped();
        let up = pen(0, 0.0).stamped();
        world.write_message(PointerReleased {
            position: Vec2::new(60.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: up,
        });
        world.write_message(PointerMoved {
            position: Vec2::new(60.0, 10.0),
            local: None,
            pointer: moved,
        });
        world.write_message(PointerPressed {
            position: Vec2::new(50.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: down,
        });
        pointer_tick(&mut world);
        let order: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(t, ..)| t.clone())
            .collect();
        assert_eq!(order, ["pointerdown", "pointermove", "pointerup"]);
        event::clear_all_bindings();
    }

    /// A finger is captured by the node it went down on, and a second
    /// finger's events go to the node under it, not to the hovered one.
    #[test]
    fn touches_are_captured_where_they_land_and_aimed_per_finger() {
        let _guard = serial();
        event::clear_all_bindings();
        let (mut world, a, b) = two_boxes();
        let node_a = lumen_core::node::NodeHandle::new(a).pack();
        let node_b = lumen_core::node::NodeHandle::new(b).pack();
        let seen: Seen = Arc::default();
        for node in [node_a, node_b] {
            record(
                node,
                &["pointerdown", "pointermove", "pointercancel"],
                &seen,
            );
        }
        let finger = |id: u64, is_primary: bool| PointerData {
            id,
            kind: PointerKind::Touch,
            is_primary,
            buttons: 1,
            pressure: 0.5,
            ..PointerData::default()
        };
        // Finger 2 down on `a` (hovered), finger 3 down on `b`.
        world.entity_mut(a).insert(Hovered);
        world.resource_mut::<PointerTargets>().set(3, Some(b));
        world.write_message(PointerPressed {
            position: Vec2::new(10.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: finger(2, true),
        });
        world.write_message(PointerPressed {
            position: Vec2::new(110.0, 10.0),
            button: PointerButton::Primary,
            local: None,
            pointer: finger(3, false),
        });
        pointer_tick(&mut world);
        // Finger 2 slides over `b`: still `a`'s.
        world.entity_mut(a).remove::<Hovered>();
        world.entity_mut(b).insert(Hovered);
        world.write_message(PointerMoved {
            position: Vec2::new(120.0, 10.0),
            local: None,
            pointer: finger(2, true),
        });
        world.write_message(PointerCancelled {
            position: Vec2::new(120.0, 10.0),
            pointer: finger(2, true),
        });
        pointer_tick(&mut world);
        let s = |t: &str| t.to_string();
        let got: Vec<(String, Option<Entity>)> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(t, e, ..)| (t.clone(), *e))
            .collect();
        assert_eq!(
            got,
            vec![
                (s("pointerdown"), Some(a)),
                (s("pointerdown"), Some(b)),
                (s("pointermove"), Some(a)),
                (s("pointercancel"), Some(a)),
            ]
        );
        event::clear_all_bindings();
    }

    /// A context-menu request reaches a `contextmenu` handler bubbling up
    /// from the element it was for, with the pointer position and the DOM
    /// `button`, and a handler's `prevent_default` is recorded so a page can
    /// keep the browser's own menu shut.
    #[test]
    fn context_menu_requests_fire_contextmenu() {
        use bevy_ecs::system::RunSystemOnce;
        use lumen_core::node::{DomIndex, DomRecord, publish_dom_index};

        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        register_dom_event_messages(&mut world);
        world.init_resource::<PendingDomEvents>();
        let root = world.spawn_empty().id();
        let canvas = world
            .spawn(Transform {
                absolute: Vec2::new(100.0, 50.0),
                size: Vec2::new(200.0, 200.0),
                baseline_y: None,
            })
            .id();
        let record = |entity: Entity, parent: Option<Entity>, children: Vec<Entity>| DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: String::new(),
            id: None,
            classes: Vec::new(),
            parent,
            children,
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        };
        publish_dom_index(DomIndex::build(vec![
            record(root, None, vec![canvas]),
            record(canvas, Some(root), Vec::new()),
        ]));
        let doc = lumen_core::node::NodeHandle::new(root).pack();
        let target = lumen_core::node::NodeHandle::new(canvas).pack();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        event::register_native_binding(
            doc,
            "contextmenu".to_string(),
            false,
            Arc::new(move || {
                let (cx, cy) = event::event_position_client();
                let (lx, ly) = event::event_position_local();
                sink.lock().unwrap().push(format!(
                    "{} button={} client=({cx},{cy}) local=({lx},{ly})",
                    event::event_type(),
                    event::event_button(),
                ));
                event::event_prevent_default();
            }),
        );

        world.write_message(ContextMenuEvent {
            entity: canvas,
            position: Vec2::new(130.0, 70.0),
            button: PointerButton::Secondary,
            local: None,
        });
        world
            .run_system_once(queue_pointer_and_key_events)
            .expect("system ran");
        deliver_input_events(&mut world);

        assert_eq!(
            *seen.lock().unwrap(),
            vec!["contextmenu button=2 client=(130,70) local=(30,20)".to_string()],
        );
        assert!(
            event::is_default_prevented("contextmenu", target),
            "the handler cancelled the default menu",
        );
        assert!(
            !event::is_default_prevented("click", target),
            "a cancel is recorded for its own event type only",
        );
        event::clear_all_bindings();
        publish_dom_index(DomIndex::default());
    }

    /// A module's custom event bubbles from its target to a handler on an
    /// ancestor with its detail, a handler's `prevent_default` is reported
    /// for a cancelable event only, a non-bubbling one stops at its target,
    /// and a built-in event's name is refused.
    #[test]
    fn custom_events_travel_like_dom_events() {
        use bevy_ecs::system::RunSystemOnce;
        use lumen_core::node::{DomIndex, DomRecord, publish_dom_index};

        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        register_dom_event_messages(&mut world);
        world.init_resource::<PendingDomEvents>();
        let dock = world.spawn_empty().id();
        let panel = world.spawn_empty().id();
        let record = |entity: Entity, parent: Option<Entity>, children: Vec<Entity>| DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: String::new(),
            id: None,
            classes: Vec::new(),
            parent,
            children,
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        };
        publish_dom_index(DomIndex::build(vec![
            record(dock, None, vec![panel]),
            record(panel, Some(dock), Vec::new()),
        ]));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        for name in ["panelclose", "panelshow", "click"] {
            let sink = Arc::clone(&seen);
            event::register_native_binding(
                lumen_core::node::NodeHandle::new(dock).pack(),
                name.to_string(),
                false,
                Arc::new(move || {
                    let detail = event::event_detail();
                    sink.lock().unwrap().push(format!(
                        "{} reason={:?}",
                        event::event_type(),
                        detail.get("reason"),
                    ));
                    event::event_prevent_default();
                }),
            );
        }

        let mut close = CustomEvent::new(panel, "panelclose").with("reason", "user");
        close.cancelable = true;
        world.write_message(close.clone());
        world.write_message(CustomEvent::new(panel, "panelshow"));
        let mut quiet = CustomEvent::new(panel, "panelshow");
        quiet.bubbles = false;
        world.write_message(quiet);
        world.write_message(CustomEvent::new(panel, "click"));
        world
            .run_system_once(queue_custom_events)
            .expect("system ran");
        deliver_custom_events(&mut world);

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "panelclose reason=Some(Str(\"user\"))".to_string(),
                "panelshow reason=None".to_string(),
            ],
            "the cancelable and the bubbling event reach the dock; the one \
             that does not bubble and the one with a built-in name do not"
        );
        let delivered: Vec<(String, bool)> = world
            .resource_mut::<Messages<CustomEventDelivered>>()
            .drain()
            .map(|d| (d.event.name, d.default_prevented))
            .collect();
        assert_eq!(
            delivered,
            vec![
                ("panelclose".to_string(), true),
                ("panelshow".to_string(), false),
                ("panelshow".to_string(), false),
            ],
            "a cancel counts only on a cancelable event"
        );
        event::clear_all_bindings();
        publish_dom_index(DomIndex::default());
    }

    /// A control that is not a text field raises `input` for each new
    /// value and `change` for the commit, and a widget's internal text field
    /// raises nothing of its own.
    #[test]
    fn control_input_fires_input_and_change() {
        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        register_dom_event_messages(&mut world);
        let field = world.spawn_empty().id();
        let hex = world
            .spawn((TextBuffer::single_line("#000000"), WidgetInternal))
            .id();
        let seen = watch(
            lumen_core::node::NodeHandle::new(field).pack(),
            &["input", "change"],
        );
        let inner = watch(
            lumen_core::node::NodeHandle::new(hex).pack(),
            &["input", "change", "submit"],
        );

        world.write_message(ControlInput {
            entity: field,
            value: "#112233".to_string(),
            committed: false,
        });
        world.write_message(ControlInput {
            entity: field,
            value: "#445566".to_string(),
            committed: true,
        });
        world.write_message(TextEditApplied {
            entity: hex,
            version: 1,
            kind: AppliedKind::Insert,
            before_byte: 0,
            after_byte: 1,
        });
        world.write_message(TextInputCommitted {
            entity: hex,
            text: "#000000".to_string(),
        });
        drive(&mut world);
        world.resource_mut::<Messages<ControlInput>>().clear();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("input".to_string(), "#112233".to_string()),
                ("change".to_string(), "#445566".to_string()),
            ],
        );
        assert!(
            inner.lock().unwrap().is_empty(),
            "internal parts raise nothing"
        );
        event::clear_all_bindings();
    }

    #[test]
    fn edit_without_a_buffer_fires_nothing() {
        let _guard = serial();
        event::clear_all_bindings();
        let mut world = World::new();
        world.init_resource::<Messages<ScriptCommandEvent>>();
        world.init_resource::<Messages<TextEditApplied>>();
        world.init_resource::<Messages<TextInputCommitted>>();
        let ghost = world.spawn_empty().id();
        let node = lumen_core::node::NodeHandle::new(ghost).pack();
        let seen = watch(node, &["input"]);

        world
            .resource_mut::<Messages<TextEditApplied>>()
            .write(TextEditApplied {
                entity: ghost,
                version: 1,
                kind: AppliedKind::Insert,
                before_byte: 0,
                after_byte: 1,
            });
        drive(&mut world);
        assert!(seen.lock().unwrap().is_empty());
        event::clear_all_bindings();
    }
}
