//! Runtime event dispatch for the dynamic DOM API (phase 4).
//!
//! These host-generic systems turn the input pipeline's typed messages
//! (clicks, pointer moves, wheel, keys, focus changes, text commits, scroll)
//! into DOM events and route them through the capture -> target -> bubble
//! propagation driver in [`crate::event`]. A handler bound with
//! `n.on(type, handler)` runs here; commands it queues are forwarded onto the
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
//! - `input` fires per edit, from the text pipeline's `TextEditApplied`
//!   signal: one event per keystroke, paste, or IME commit that changes the
//!   text, at most one per entity per tick, carrying the live buffer. A pure
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
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
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

/// Deliver one already-built [`EventData`] targeting `target_entity`:
/// resolve the propagation path, run the driver (host closures via the trait,
/// native callbacks directly), forward queued commands, and record a
/// default-prevented click.
fn deliver<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: Option<&mut H>,
    out: &mut MessageWriter<ScriptCommandEvent>,
    data: EventData,
    target_entity: Entity,
) {
    if !event::has_bindings_for(&data.event_type) {
        return;
    }
    let ancestors = ancestors_root_first(target_entity);
    let bubbles = event::event_bubbles(&data.event_type);
    let etype = data.event_type.clone();
    let target_handle = data.target;
    // Native (C-ABI / SDK) bindings fire directly inside the driver; host
    // closures need the script host, which a script-less app does not have.
    let result = event::dispatch(data, &ancestors, bubbles, |token| {
        if let Some(h) = host.as_mut() {
            let _ = h.dispatch_event_handler(token);
        }
    });
    if let Some(h) = host.as_mut() {
        for c in h.drain_commands() {
            out.write(ScriptCommandEvent(c));
        }
    }
    if etype == "click" && result.default_prevented {
        event::mark_prevented_click(target_handle);
    }
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

fn with_position(
    mut data: EventData,
    transforms: &Query<&Transform>,
    entity: Entity,
    pos_x: f32,
    pos_y: f32,
) -> EventData {
    let (ax, ay) = transforms
        .get(entity)
        .map(|t| (t.absolute.x, t.absolute.y))
        .unwrap_or((0.0, 0.0));
    data.local = ((pos_x - ax) as f64, (pos_y - ay) as f64);
    data.client = (pos_x as f64, pos_y as f64);
    data
}

fn set_mods(data: &mut EventData, mods: &Modifiers) {
    data.shift = mods.shift;
    data.ctrl = mods.ctrl;
    data.alt = mods.alt;
    data.super_ = mods.super_;
}

/// Pointer / click / wheel / key dispatch. Pointer events target the hovered
/// entity; key events target the focused entity.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_pointer_and_key_events<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: Option<ResMut<H>>,
    mut out: MessageWriter<ScriptCommandEvent>,
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
    // Fresh per-tick prevented-click set for the anchor-nav executor.
    event::clear_prevented_clicks();

    let hovered_entity = hovered.iter().next();
    let focused_entity = focused.iter().next();

    // click (targets the clicked entity directly).
    for c in clicks.read() {
        let mut data = with_position(
            base(c.entity, "click"),
            &transforms,
            c.entity,
            c.position.x,
            c.position.y,
        );
        data.button = button_code(c.button);
        deliver(host.as_deref_mut(), &mut out, data, c.entity);
    }
    // dblclick.
    for d in doubles.read() {
        let data = with_position(
            base(d.entity, "dblclick"),
            &transforms,
            d.entity,
            d.position.x,
            d.position.y,
        );
        deliver(host.as_deref_mut(), &mut out, data, d.entity);
    }
    // pointerdown / up / move / wheel target the hovered entity.
    for p in presses.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(
            base(e, "pointerdown"),
            &transforms,
            e,
            p.position.x,
            p.position.y,
        );
        data.button = button_code(p.button);
        deliver(host.as_deref_mut(), &mut out, data, e);
    }
    for p in releases.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(
            base(e, "pointerup"),
            &transforms,
            e,
            p.position.x,
            p.position.y,
        );
        data.button = button_code(p.button);
        deliver(host.as_deref_mut(), &mut out, data, e);
    }
    for p in moves.read() {
        let Some(e) = hovered_entity else { continue };
        let data = with_position(
            base(e, "pointermove"),
            &transforms,
            e,
            p.position.x,
            p.position.y,
        );
        deliver(host.as_deref_mut(), &mut out, data, e);
    }
    for w in wheels.read() {
        let Some(e) = hovered_entity else { continue };
        let mut data = with_position(base(e, "wheel"), &transforms, e, w.position.x, w.position.y);
        data.delta = (w.delta.x as f64, w.delta.y as f64);
        deliver(host.as_deref_mut(), &mut out, data, e);
    }
    // keydown targets the entity the input router routed the key to.
    for k in keydowns.read() {
        let mut data = base(k.entity, "keydown");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        deliver(host.as_deref_mut(), &mut out, data, k.entity);
    }
    // keyup targets the focused entity.
    for k in keyups.read() {
        let Some(e) = focused_entity else { continue };
        let mut data = base(e, "keyup");
        data.key = key_string(&k.key);
        set_mods(&mut data, &k.modifiers);
        deliver(host.as_deref_mut(), &mut out, data, e);
    }
}

/// Focus / blur / enter / leave / input / change / submit / scroll dispatch.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_state_events<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: Option<ResMut<H>>,
    mut out: MessageWriter<ScriptCommandEvent>,
    mut edits: MessageReader<lumen_core::text_events::TextEditApplied>,
    mut commits: MessageReader<TextInputCommitted>,
    gained_focus: Query<Entity, Added<Focused>>,
    mut lost_focus: RemovedComponents<Focused>,
    gained_hover: Query<Entity, Added<Hovered>>,
    mut lost_hover: RemovedComponents<Hovered>,
    scrolled: Query<Entity, Changed<ScrollOffset>>,
    buffers: Query<&lumen_core::text_model::TextBuffer>,
) {
    // A binding keys on a node's packed handle (entity + generation), so a
    // despawned + recycled entity never matches a live binding; the
    // has-bindings gate inside `deliver` also makes an unbound remove a
    // no-op. No explicit liveness check is needed here.

    // focus / blur.
    for e in gained_focus.iter() {
        deliver(host.as_deref_mut(), &mut out, base(e, "focus"), e);
    }
    for e in lost_focus.read() {
        deliver(host.as_deref_mut(), &mut out, base(e, "blur"), e);
    }
    // pointerenter / pointerleave.
    for e in gained_hover.iter() {
        deliver(host.as_deref_mut(), &mut out, base(e, "pointerenter"), e);
    }
    for e in lost_hover.read() {
        deliver(host.as_deref_mut(), &mut out, base(e, "pointerleave"), e);
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
        let Ok(buf) = buffers.get(ev.entity) else {
            continue;
        };
        fired.push(ev.entity);
        let mut data = base(ev.entity, "input");
        data.value = buf.to_string();
        deliver(host.as_deref_mut(), &mut out, data, ev.entity);
    }
    // change / submit from the commit signal (Enter on a single-line
    // input, or focus leaving a committed field).
    for c in commits.read() {
        for etype in ["change", "submit"] {
            let mut data = base(c.entity, etype);
            data.value = c.text.clone();
            deliver(host.as_deref_mut(), &mut out, data, c.entity);
        }
    }
    // scroll (does not bubble).
    for e in scrolled.iter() {
        deliver(host.as_deref_mut(), &mut out, base(e, "scroll"), e);
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

    /// A host the dispatcher never sees: the systems take
    /// `Option<ResMut<H>>` and these tests leave the resource out, so the
    /// events route through native bindings only. The type exists to name
    /// `H`.
    ///
    /// `pub(crate)` so `runtime`'s derivation tests can build on it instead
    /// of writing a second `ScriptHost` stub with the same unimplemented /
    /// trivial bodies.
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
        fn drain_commands(&mut self) -> Vec<crate::ScriptCommand> {
            Vec::new()
        }
        fn push_commands(&mut self, _cmds: Vec<crate::ScriptCommand>) {}
        fn mirror_get(&self, _name: &str) -> Option<crate::ScriptValue> {
            None
        }
        fn mirror_set(&mut self, _name: &str, _value: crate::ScriptValue) {}
        fn mirror_sync_str(&mut self, _name: &str, _value: &str) {}
        fn handler_for(&self, _event: &str, _key: &str) -> Option<String> {
            None
        }
        fn derivations_matching(
            &self,
            _dirty: &std::collections::HashSet<&str>,
            _pending: &std::collections::HashSet<String>,
        ) -> Vec<(String, Vec<String>, Self::Closure)> {
            Vec::new()
        }
        fn pending_initial(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::new()
        }
        fn clear_pending(&mut self, _evaluated: &[String]) {}
        fn register_script_fn(&mut self, _f: &crate::ScriptFn) -> Result<(), crate::ScriptError> {
            unimplemented!("no host in these tests")
        }
        fn lang(&self) -> &'static str {
            "test"
        }
        fn builtins(&self) -> &'static [crate::BuiltinFn] {
            &[]
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
        world
            .run_system_once(dispatch_state_events::<NoHost>)
            .expect("system ran");
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
