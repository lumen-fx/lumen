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
//! The full event set: `click`, `dblclick`, `pointerdown` / `pointerup` /
//! `pointermove` / `pointerenter` / `pointerleave`, `wheel`, `keydown` /
//! `keyup`, `input`, `change`, `focus`, `blur`, `submit`, `scroll`.
//!
//! Sources and current limitations:
//! - Pointer events target the entity currently under the cursor (`Hovered`);
//!   `pointerenter` / `pointerleave` come from the hover marker transitions.
//! - `keydown` targets the focused entity (from the input router's
//!   `FocusedKey`); `keyup` targets the focused entity.
//! - `input` fires per edit, from the `TextEditApplied` signal the text
//!   pipeline raises (or a page, whose fields the browser edits): one event
//!   per keystroke, paste, or IME commit that changes the text, at most one
//!   per entity per tick, carrying the field's text after the edit. A pure
//!   caret move is not an edit and fires nothing.
//! - `change` and `submit` fire on commit, from the input router's
//!   `TextInputCommitted` signal; `submit` is the Enter-commit on a
//!   single-line input.
//! - `scroll` comes from a changed scroll offset and does not bubble.
//!
//! Default actions: only `click` (link navigation via `<a href>`) and
//! `submit` (form submission) have one. `prevent_default` on a `click`
//! records the target so the runtime's anchor-navigation executor skips it;
//! `submit`'s default is reserved (there is no form-submission model yet).

use bevy_ecs::component::Mutable;
use bevy_ecs::message::{Message, MessageReader, MessageRegistry, Messages};
use bevy_ecs::prelude::*;
use glam::Vec2;
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

/// Map a [`PointerButton`] to the web `MouseEvent.button` code
/// (`0` primary / left, `1` middle, `2` secondary / right).
fn button_code(button: PointerButton) -> i64 {
    match button {
        PointerButton::Primary => 0,
        PointerButton::Middle => 1,
        PointerButton::Secondary => 2,
        PointerButton::Other(code) => code as i64,
    }
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

/// The DOM events read from this tick's input, waiting for delivery.
///
/// Reading the input and delivering it are two steps because a handler can
/// live in any of the app's hosts, and reaching a host needs the whole world:
/// the readers are ordinary systems that run beside the rest of the tick, and
/// the delivery is one exclusive pass that walks each event's propagation path
/// once, whatever the number of hosts.
#[derive(Resource, Default)]
pub struct PendingDomEvents {
    input: Vec<(EventData, Entity)>,
    state: Vec<(EventData, Entity)>,
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
    // Fresh per-tick prevented-click set for the anchor-nav executor.
    event::clear_prevented_clicks();
    deliver_pending(world, |pending| &mut pending.input);
}

/// Deliver the focus, hover, edit, commit and scroll events read this tick.
pub fn deliver_state_events(world: &mut World) {
    deliver_pending(world, |pending| &mut pending.state);
}

fn deliver_pending(
    world: &mut World,
    queue: fn(&mut PendingDomEvents) -> &mut Vec<(EventData, Entity)>,
) {
    let Some(mut pending) = world.get_resource_mut::<PendingDomEvents>() else {
        return;
    };
    let events = std::mem::take(queue(&mut pending));
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

/// Deliver one already-built [`EventData`] targeting `target_entity`:
/// resolve the propagation path, run the driver once (native callbacks
/// directly, host closures through whichever host owns the token), forward
/// queued commands, and record a default-prevented click.
fn deliver(world: &mut World, hosts: &EventHosts, data: EventData, target_entity: Entity) {
    if !event::has_bindings_for(&data.event_type) {
        return;
    }
    let ancestors = ancestors_root_first(target_entity);
    let bubbles = event::event_bubbles(&data.event_type);
    let etype = data.event_type.clone();
    let target_handle = data.target;
    let result = event::dispatch(data, &ancestors, bubbles, |token| {
        for host in &hosts.0 {
            if (host.dispatch)(world, token) {
                break;
            }
        }
    });
    for host in &hosts.0 {
        (host.drain)(world);
    }
    if etype == "click" && result.default_prevented {
        event::mark_prevented_click(target_handle);
    }
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
    register::<PointerPressed>(world);
    register::<PointerReleased>(world);
    register::<PointerMoved>(world);
    register::<MouseWheel>(world);
    register::<KeyReleased>(world);
    register::<FocusedKey>(world);
    register::<lumen_core::text_events::TextEditApplied>(world);
    register::<TextInputCommitted>(world);
}

/// Read this tick's pointer, click, wheel and key input as DOM events, for
/// [`deliver_input_events`]. Pointer events target the hovered entity; key
/// events target the focused entity.
#[allow(clippy::too_many_arguments)]
pub fn queue_pointer_and_key_events(
    mut pending: ResMut<PendingDomEvents>,
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    mut presses: MessageReader<PointerPressed>,
    mut releases: MessageReader<PointerReleased>,
    mut moves: MessageReader<PointerMoved>,
    mut wheels: MessageReader<MouseWheel>,
    mut keyups: MessageReader<KeyReleased>,
    mut keydowns: MessageReader<FocusedKey>,
    transforms: Query<&Transform>,
    hovered: Query<Entity, With<Hovered>>,
    focused: Query<Entity, With<Focused>>,
) {
    let queue = &mut pending.input;
    let hovered_entity = hovered.iter().next();
    let focused_entity = focused.iter().next();

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
        queue.push((data, c.entity));
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
        queue.push((data, d.entity));
    }
    // pointerdown / up / move / wheel target the hovered entity.
    for p in presses.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(base(e, "pointerdown"), &transforms, e, p.position, p.local);
        data.button = button_code(p.button);
        queue.push((data, e));
    }
    for p in releases.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(base(e, "pointerup"), &transforms, e, p.position, p.local);
        data.button = button_code(p.button);
        queue.push((data, e));
    }
    for p in moves.read() {
        let Some(e) = hovered_entity else { continue };
        let data = with_position(base(e, "pointermove"), &transforms, e, p.position, p.local);
        queue.push((data, e));
    }
    for w in wheels.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(base(e, "wheel"), &transforms, e, w.position, w.local);
        data.delta = (w.delta.x as f64, w.delta.y as f64);
        queue.push((data, e));
    }
    // keydown targets the entity the input router routed the key to.
    for k in keydowns.read() {
        let mut data = base(k.entity, "keydown");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        queue.push((data, k.entity));
    }
    // keyup targets the focused entity.
    for k in keyups.read() {
        let Some(e) = focused_entity else { continue };
        let mut data = base(e, "keyup");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        queue.push((data, e));
    }
}

/// Read this tick's focus, hover, edit, commit and scroll changes as DOM
/// events, for [`deliver_state_events`].
#[allow(clippy::too_many_arguments)]
pub fn queue_state_events(
    mut pending: ResMut<PendingDomEvents>,
    mut edits: MessageReader<lumen_core::text_events::TextEditApplied>,
    mut commits: MessageReader<TextInputCommitted>,
    gained_focus: Query<Entity, Added<Focused>>,
    mut lost_focus: RemovedComponents<Focused>,
    gained_hover: Query<Entity, Added<Hovered>>,
    mut lost_hover: RemovedComponents<Hovered>,
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
    // pointerenter / pointerleave.
    for e in gained_hover.iter() {
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
        for etype in ["change", "submit"] {
            let mut data = base(c.entity, etype);
            data.value = c.text.clone();
            queue.push((data, c.entity));
        }
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
        ) -> Result<crate::CallOutcome, crate::ScriptError> {
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
