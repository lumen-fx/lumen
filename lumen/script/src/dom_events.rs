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
//! - `input`, `change`, and `submit` are all produced from the input router's
//!   commit signal (`TextInputCommitted`): `input` / `change` fire on commit
//!   rather than per keystroke, and `submit` is the Enter-commit on a
//!   single-line input. A finer-grained `input` stream is deferred.
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
    mut commits: MessageReader<TextInputCommitted>,
    gained_focus: Query<Entity, Added<Focused>>,
    mut lost_focus: RemovedComponents<Focused>,
    gained_hover: Query<Entity, Added<Hovered>>,
    mut lost_hover: RemovedComponents<Hovered>,
    scrolled: Query<Entity, Changed<ScrollOffset>>,
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
    // input / change / submit from the commit signal.
    for c in commits.read() {
        for etype in ["input", "change", "submit"] {
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
