//! Host-neutral event object, binding registry, and propagation driver for
//! the dynamic DOM API (phase 4).
//!
//! `n.on("click", handler)` binds a handler to a node for an event type and
//! returns an off token; `off()` unbinds. Bindings live in a process-global
//! registry keyed by token so the generic dispatcher (and the C-ABI, which
//! has no script host) share one source of truth. The handler itself is
//! either a closure held by the active [`crate::ScriptHost`] (keyed by the
//! same token) or a native Rust callback (the C-ABI / SDK path).
//!
//! During dispatch the driver publishes the event into a process-global
//! current-event cell; a handler reads target / position / key / modifiers /
//! button / delta / value from it and mutates its `prevent_default` /
//! `stop_propagation` / `stop_immediate_propagation` flags through the free
//! functions here. A host may wrap those in a registered `Event` handle or
//! call them procedurally, as the C-ABI does.
//!
//! Propagation follows the DOM contract: capture from the root down to the
//! target, dispatch at the target, then bubble back up (for events that
//! bubble). `stop_propagation` halts movement to the next node after the
//! current node's handlers finish; `stop_immediate_propagation` also halts
//! the remaining handlers on the current node.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// The full event payload delivered to a handler. Positional / key / button /
/// delta / value fields carry meaning only for the event types that produce
/// them (a `keydown` has `key` + modifiers, a `wheel` has `delta`, an
/// `input` / `change` has `value`, and so on); unused fields stay at their
/// defaults.
#[derive(Clone, Debug, Default)]
pub struct EventData {
    /// Event type name (`"click"`, `"keydown"`, `"wheel"`, ...).
    pub event_type: String,
    /// The node the event originally targeted (packed handle).
    pub target: u64,
    /// The node whose handler is currently running (packed handle). The
    /// driver updates this as it walks the propagation path.
    pub current_target: u64,
    /// Pointer position relative to the target's top-left, logical pixels.
    pub local: (f64, f64),
    /// Pointer position in window (client) coordinates, logical pixels.
    pub client: (f64, f64),
    /// Logical key for keyboard events (`"a"`, `"Enter"`, `"ArrowLeft"`).
    pub key: String,
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub ctrl: bool,
    /// Alt / Option held.
    pub alt: bool,
    /// Super / Cmd / Windows held.
    pub super_: bool,
    /// Pointer button, the web `button` code: `0` primary, `1` middle, `2`
    /// secondary, `3` back, `4` forward, `5` pen eraser, `-1` none (a move).
    pub button: i64,
    /// The pointer behind a pointer event: its id, device kind, held
    /// buttons and pen / touch readings, as the web `PointerEvent` has them.
    pub pointer: lumen_core::input::PointerData,
    /// Wheel scroll delta, logical pixels.
    pub delta: (f64, f64),
    /// Text value for `input` / `change` events.
    pub value: String,
    /// What a custom event carries ([`crate::dom_events::CustomEvent`]);
    /// empty for the built-in events.
    pub detail: std::collections::HashMap<String, crate::ScriptValue>,
    /// Set by `prevent_default`: the event's default action is cancelled.
    pub default_prevented: bool,
    /// Set by `stop_propagation`: no further nodes receive the event.
    pub propagation_stopped: bool,
    /// Set by `stop_immediate_propagation`: no further handlers run at all.
    pub immediate_stopped: bool,
}

// ---------------------------------------------------------------------------
// Current-event cell + accessors (read + mutated by handlers).
// ---------------------------------------------------------------------------

static CURRENT_EVENT: OnceLock<RwLock<EventData>> = OnceLock::new();

fn current_cell() -> &'static RwLock<EventData> {
    CURRENT_EVENT.get_or_init(|| RwLock::new(EventData::default()))
}

/// Publish `data` as the current event before invoking handlers.
pub fn set_current_event(data: EventData) {
    if let Ok(mut g) = current_cell().write() {
        *g = data;
    }
}

/// Read the current event (cheap clone).
pub fn current_event() -> EventData {
    current_cell().read().map(|g| g.clone()).unwrap_or_default()
}

/// Update the current event's `current_target` as the driver moves along
/// the propagation path.
pub fn set_current_target(node: u64) {
    if let Ok(mut g) = current_cell().write() {
        g.current_target = node;
    }
}

/// `ev.target()`.
pub fn event_target() -> u64 {
    current_cell().read().map(|g| g.target).unwrap_or(0)
}

/// `ev.current_target()`.
pub fn event_current_target() -> u64 {
    current_cell().read().map(|g| g.current_target).unwrap_or(0)
}

/// `ev.type()`.
pub fn event_type() -> String {
    current_cell()
        .read()
        .map(|g| g.event_type.clone())
        .unwrap_or_default()
}

/// Local position `(x, y)` (relative to the target).
pub fn event_position_local() -> (f64, f64) {
    current_cell().read().map(|g| g.local).unwrap_or((0.0, 0.0))
}

/// Client position `(x, y)` (window coordinates).
pub fn event_position_client() -> (f64, f64) {
    current_cell()
        .read()
        .map(|g| g.client)
        .unwrap_or((0.0, 0.0))
}

/// `ev.key()`.
pub fn event_key() -> String {
    current_cell()
        .read()
        .map(|g| g.key.clone())
        .unwrap_or_default()
}

/// `ev.modifiers()` as `(shift, ctrl, alt, super)`.
pub fn event_modifiers() -> (bool, bool, bool, bool) {
    current_cell()
        .read()
        .map(|g| (g.shift, g.ctrl, g.alt, g.super_))
        .unwrap_or((false, false, false, false))
}

/// `ev.button()`.
pub fn event_button() -> i64 {
    current_cell().read().map(|g| g.button).unwrap_or(-1)
}

/// The pointer behind the current pointer event: `ev.pointer_id()`,
/// `ev.pointer_type()`, `ev.pressure()`, `ev.tilt_x()` and the rest read
/// their field from this.
pub fn event_pointer() -> lumen_core::input::PointerData {
    current_cell().read().map(|g| g.pointer).unwrap_or_default()
}

/// `ev.delta()` as `(x, y)`.
pub fn event_delta() -> (f64, f64) {
    current_cell().read().map(|g| g.delta).unwrap_or((0.0, 0.0))
}

/// `ev.value()`.
pub fn event_value() -> String {
    current_cell()
        .read()
        .map(|g| g.value.clone())
        .unwrap_or_default()
}

/// `ev.detail()`: what a custom event carries, empty for a built-in one.
pub fn event_detail() -> std::collections::HashMap<String, crate::ScriptValue> {
    current_cell()
        .read()
        .map(|g| g.detail.clone())
        .unwrap_or_default()
}

/// `ev.prevent_default()`.
pub fn event_prevent_default() {
    if let Ok(mut g) = current_cell().write() {
        g.default_prevented = true;
    }
}

/// `ev.stop_propagation()`.
pub fn event_stop_propagation() {
    if let Ok(mut g) = current_cell().write() {
        g.propagation_stopped = true;
    }
}

/// `ev.stop_immediate_propagation()`. Implies `stop_propagation`.
pub fn event_stop_immediate_propagation() {
    if let Ok(mut g) = current_cell().write() {
        g.propagation_stopped = true;
        g.immediate_stopped = true;
    }
}

fn is_propagation_stopped() -> bool {
    current_cell()
        .read()
        .map(|g| g.propagation_stopped)
        .unwrap_or(false)
}

fn is_immediate_stopped() -> bool {
    current_cell()
        .read()
        .map(|g| g.immediate_stopped)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Binding registry.
// ---------------------------------------------------------------------------

/// How a bound handler is invoked at dispatch time.
#[derive(Clone)]
pub enum EventHandler {
    /// A closure held by the active [`crate::ScriptHost`], keyed by the
    /// binding's token. The generic dispatcher calls back into the host.
    Host,
    /// A native Rust callback (C-ABI / Rust SDK). Invoked directly; it reads
    /// the current event via the accessor free functions above.
    Native(Arc<dyn Fn() + Send + Sync>),
}

impl std::fmt::Debug for EventHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventHandler::Host => f.write_str("Host"),
            EventHandler::Native(_) => f.write_str("Native"),
        }
    }
}

/// One `n.on(type, handler)` binding.
#[derive(Clone, Debug)]
pub struct EventBinding {
    /// Unbind token (also the host's closure key).
    pub token: u64,
    /// Bound node (packed handle).
    pub node: u64,
    /// Event type this binding listens for.
    pub event_type: String,
    /// `true` = capture-phase listener; `false` = bubble / target listener.
    pub capture: bool,
    /// How to invoke the handler.
    pub handler: EventHandler,
}

static EVENT_TOKEN: AtomicU64 = AtomicU64::new(1);
static BINDINGS: OnceLock<RwLock<Vec<EventBinding>>> = OnceLock::new();

fn bindings() -> &'static RwLock<Vec<EventBinding>> {
    BINDINGS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Mint a fresh, process-unique bind / off token (always `>= 1`).
pub fn mint_event_token() -> u64 {
    EVENT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// Register a host-closure binding (token, node, type, capture). The host
/// holds the closure itself keyed by `token`.
pub fn register_host_binding(token: u64, node: u64, event_type: String, capture: bool) {
    register(EventBinding {
        token,
        node,
        event_type,
        capture,
        handler: EventHandler::Host,
    });
}

/// Register a native-callback binding (the C-ABI / SDK path). Returns the
/// token for the caller to hand back as the off token.
pub fn register_native_binding(
    node: u64,
    event_type: String,
    capture: bool,
    callback: Arc<dyn Fn() + Send + Sync>,
) -> u64 {
    let token = mint_event_token();
    register(EventBinding {
        token,
        node,
        event_type,
        capture,
        handler: EventHandler::Native(callback),
    });
    token
}

fn register(binding: EventBinding) {
    if let Ok(mut b) = bindings().write() {
        // A re-registered token replaces its prior binding (idempotent).
        b.retain(|e| e.token != binding.token);
        b.push(binding);
    }
}

/// Remove the binding for `token`, returning it when present.
pub fn unregister_binding(token: u64) -> Option<EventBinding> {
    let mut b = bindings().write().ok()?;
    let idx = b.iter().position(|e| e.token == token)?;
    Some(b.remove(idx))
}

/// Drop every host-closure binding. Called on host reset, and on hot reload
/// before the script re-runs and re-binds.
pub fn clear_host_bindings() {
    if let Ok(mut b) = bindings().write() {
        b.retain(|e| !matches!(e.handler, EventHandler::Host));
    }
}

/// Remove every host-closure binding and return it, in registration order.
///
/// A hot reload takes the bindings out before it re-runs the script, then
/// hands the snapshot to [`restore_host_bindings`] so the ones the new script
/// did not re-bind keep firing. Native bindings are left in place; they belong
/// to the C-ABI / SDK caller, not to the script.
pub fn take_host_bindings() -> Vec<EventBinding> {
    let Ok(mut b) = bindings().write() else {
        return Vec::new();
    };
    let mut taken = Vec::new();
    b.retain(|e| {
        if matches!(e.handler, EventHandler::Host) {
            taken.push(e.clone());
            false
        } else {
            true
        }
    });
    taken
}

/// Carry `prior` bindings forward past a hot reload, and report the tokens
/// that were dropped instead.
///
/// A prior binding is dropped when the reloaded script already bound the same
/// node, event type, and phase; the new binding is the one that matches the
/// new source. Every other prior binding is re-registered, so a handler bound
/// from `on_start` (which the runtime fires once, at app construction) still
/// fires after a reload. The returned tokens are the dropped ones, for the
/// caller to purge from its own handler map.
///
/// Carried bindings go back in front of the new ones, preserving the order
/// handlers ran in before the reload.
pub fn restore_host_bindings(prior: Vec<EventBinding>) -> Vec<u64> {
    let Ok(mut b) = bindings().write() else {
        return Vec::new();
    };
    let mut dropped = Vec::new();
    let mut carried = Vec::new();
    for e in prior {
        let superseded = b
            .iter()
            .any(|n| n.node == e.node && n.event_type == e.event_type && n.capture == e.capture);
        if superseded {
            dropped.push(e.token);
        } else {
            carried.push(e);
        }
    }
    carried.append(&mut b);
    *b = carried;
    dropped
}

/// Drop all bindings, and every pointer capture with them (mainly for test
/// isolation).
pub fn clear_all_bindings() {
    if let Ok(mut b) = bindings().write() {
        b.clear();
    }
    clear_pointer_captures();
}

/// Snapshot of the bindings for `event_type`, in registration order.
fn bindings_for(event_type: &str) -> Vec<EventBinding> {
    bindings()
        .read()
        .map(|b| {
            b.iter()
                .filter(|e| e.event_type == event_type)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether any binding exists for `event_type` (fast dispatch gate).
pub fn has_bindings_for(event_type: &str) -> bool {
    bindings()
        .read()
        .map(|b| b.iter().any(|e| e.event_type == event_type))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Default-action coordination.
//
// Two events have a default action a handler can cancel. `click`'s is link
// navigation (a `<a href>` click navigates the active page), run by the
// runtime's `navigate_on_anchor_click`; `contextmenu`'s is the menu the
// browser would open on a page. The dispatcher records here which targets had
// their default cancelled, per event type, so whoever runs the default can
// skip them. The set is cleared at the start of every dispatch pass.
// ---------------------------------------------------------------------------

static PREVENTED_DEFAULTS: OnceLock<RwLock<HashSet<(String, u64)>>> = OnceLock::new();

fn prevented_defaults() -> &'static RwLock<HashSet<(String, u64)>> {
    PREVENTED_DEFAULTS.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Reset the per-tick prevented-default set. The dispatcher calls this before
/// processing a tick's events.
pub fn clear_prevented_defaults() {
    if let Ok(mut s) = prevented_defaults().write() {
        s.clear();
    }
}

/// Record that an `event_type` event on `target` (packed handle) had its
/// default action cancelled via `prevent_default`.
pub fn mark_default_prevented(event_type: &str, target: u64) {
    if let Ok(mut s) = prevented_defaults().write() {
        s.insert((event_type.to_string(), target));
    }
}

/// Whether an `event_type` event on `target` (packed handle) was
/// default-prevented this tick. The anchor-navigation executor asks this for
/// `click`, and a page asks it for `contextmenu` before the browser opens its
/// own menu.
pub fn is_default_prevented(event_type: &str, target: u64) -> bool {
    prevented_defaults()
        .read()
        .map(|s| s.contains(&(event_type.to_string(), target)))
        .unwrap_or(false)
}

/// Whether `event_type` has a default action a handler can cancel.
pub fn has_default_action(event_type: &str) -> bool {
    matches!(event_type, "click" | "contextmenu")
}

// ---------------------------------------------------------------------------
// Event-type metadata.
// ---------------------------------------------------------------------------

/// Whether `event_type` bubbles (fires bubble-phase handlers on ancestors
/// after the target). Mirrors the DOM: `focus` / `blur`, pointer
/// `enter` / `leave`, and `scroll` do not bubble; everything else does.
pub fn event_bubbles(event_type: &str) -> bool {
    !matches!(
        event_type,
        "focus" | "blur" | "pointerenter" | "pointerleave" | "scroll"
    )
}

// ---------------------------------------------------------------------------
// Propagation driver.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Capture,
    Target,
    Bubble,
}

/// Drive a full capture -> target -> bubble dispatch for `data`.
///
/// `ancestors_root_first` is the target's ancestor chain packed-handle list,
/// ordered root-first and EXCLUDING the target itself. Capture handlers on
/// those ancestors fire top-down, then all handlers on the target, then (for
/// a bubbling event) bubble handlers on the ancestors bottom-up. `invoke_host`
/// runs a host-closure binding by token; native bindings are invoked
/// directly. Returns the final [`EventData`] so the caller can read the
/// `default_prevented` flag and apply (or skip) the default action.
pub fn dispatch(
    data: EventData,
    ancestors_root_first: &[u64],
    bubbles: bool,
    mut invoke_host: impl FnMut(u64),
) -> EventData {
    let etype = data.event_type.clone();
    let target = data.target;
    set_current_event(data);

    let all = bindings_for(&etype);
    if all.is_empty() {
        return current_event();
    }

    let mut plan: Vec<(u64, Phase)> = Vec::new();
    for &n in ancestors_root_first {
        plan.push((n, Phase::Capture));
    }
    plan.push((target, Phase::Target));
    if bubbles {
        for &n in ancestors_root_first.iter().rev() {
            plan.push((n, Phase::Bubble));
        }
    }

    'outer: for (node, phase) in plan {
        set_current_target(node);
        for b in all.iter().filter(|b| b.node == node) {
            let want = match phase {
                Phase::Capture => b.capture,
                Phase::Bubble => !b.capture,
                Phase::Target => true,
            };
            if !want {
                continue;
            }
            match &b.handler {
                EventHandler::Native(f) => f(),
                EventHandler::Host => invoke_host(b.token),
            }
            if is_immediate_stopped() {
                break 'outer;
            }
        }
        if is_propagation_stopped() {
            break;
        }
    }

    current_event()
}

// ---------------------------------------------------------------------------
// Pointer capture.
//
// `node.set_pointer_capture(id)` aims every later event of pointer `id` at
// the node until the pointer's last button comes up, whatever is under the
// pointer, the web's `setPointerCapture`. The state follows the web's two
// steps: a call sets the *pending* target, which `has_pointer_capture` reads
// at once; the event driver turns pending into *active* just before it
// delivers that pointer's next event, firing `lostpointercapture` at the old
// target and `gotpointercapture` at the new one. Process-wide like the
// bindings, so every host and the C ABI share it.
// ---------------------------------------------------------------------------

/// One pointer's capture state.
#[derive(Clone, Copy, Debug)]
struct PointerCapture {
    /// The pointer id.
    id: u64,
    /// A button is held: the only time a capture can be set.
    down: bool,
    /// The node a call asked for (packed handle).
    pending: Option<u64>,
    /// The node events go to now (packed handle).
    active: Option<u64>,
}

static CAPTURES: std::sync::Mutex<Vec<PointerCapture>> = std::sync::Mutex::new(Vec::new());

/// Set while any capture is pending or active, so a pointer move with none
/// skips the lock.
static ANY_CAPTURE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn with_captures<R>(f: impl FnOnce(&mut Vec<PointerCapture>) -> R) -> R {
    let mut guard = CAPTURES.lock().unwrap_or_else(|e| e.into_inner());
    let r = f(&mut guard);
    guard.retain(|c| c.down || c.pending.is_some() || c.active.is_some());
    ANY_CAPTURE.store(
        guard
            .iter()
            .any(|c| c.pending.is_some() || c.active.is_some()),
        Ordering::Relaxed,
    );
    r
}

fn entry(captures: &mut Vec<PointerCapture>, id: u64) -> &mut PointerCapture {
    if let Some(i) = captures.iter().position(|c| c.id == id) {
        return &mut captures[i];
    }
    captures.push(PointerCapture {
        id,
        down: false,
        pending: None,
        active: None,
    });
    captures.last_mut().expect("just pushed")
}

/// `node.set_pointer_capture(pointer_id)`: aim pointer `pointer_id`'s events
/// at `node` (packed handle) from its next event until its last button comes
/// up. Does nothing, and returns `false`, while the pointer has no button
/// held, as on the web.
pub fn set_pointer_capture(node: u64, pointer_id: u64) -> bool {
    with_captures(|captures| {
        let c = entry(captures, pointer_id);
        if !c.down {
            return false;
        }
        c.pending = Some(node);
        true
    })
}

/// `node.release_pointer_capture(pointer_id)`: give pointer `pointer_id`
/// back to hit testing, when `node` holds it. Another node's capture is
/// left alone.
pub fn release_pointer_capture(node: u64, pointer_id: u64) {
    with_captures(|captures| {
        let c = entry(captures, pointer_id);
        if c.pending == Some(node) {
            c.pending = None;
        }
    });
}

/// `node.has_pointer_capture(pointer_id)`: whether `node` holds pointer
/// `pointer_id`, counting a capture set during this very handler.
pub fn has_pointer_capture(node: u64, pointer_id: u64) -> bool {
    with_captures(|captures| {
        captures
            .iter()
            .any(|c| c.id == pointer_id && c.pending == Some(node))
    })
}

/// Every pointer whose events go to a node now, as `(pointer id, node)`.
/// A page reads this to hand the same capture to the browser.
pub fn active_pointer_captures() -> Vec<(u64, u64)> {
    if !ANY_CAPTURE.load(Ordering::Relaxed) {
        return Vec::new();
    }
    with_captures(|captures| {
        captures
            .iter()
            .filter_map(|c| c.active.map(|node| (c.id, node)))
            .collect()
    })
}

/// Whether any pointer is captured or about to be.
pub(crate) fn any_pointer_capture() -> bool {
    ANY_CAPTURE.load(Ordering::Relaxed)
}

/// Pointer `id` went down: captures can be set on it from here.
pub(crate) fn pointer_capture_down(id: u64) {
    with_captures(|captures| entry(captures, id).down = true);
}

/// Pointer `id`'s last button came up (or it was cancelled): its capture is
/// released at the next [`take_pointer_capture_change`].
pub(crate) fn pointer_capture_up(id: u64) {
    with_captures(|captures| {
        let c = entry(captures, id);
        c.down = false;
        c.pending = None;
    });
}

/// The node pointer `id`'s events go to now, if one holds it.
pub(crate) fn active_pointer_capture(id: u64) -> Option<u64> {
    if !ANY_CAPTURE.load(Ordering::Relaxed) {
        return None;
    }
    with_captures(|captures| captures.iter().find(|c| c.id == id).and_then(|c| c.active))
}

/// Make pointer `id`'s pending capture the active one. Returns the node that
/// lost it and the node that got it, when that changed anything.
pub(crate) fn take_pointer_capture_change(id: u64) -> Option<(Option<u64>, Option<u64>)> {
    if !ANY_CAPTURE.load(Ordering::Relaxed) {
        return None;
    }
    with_captures(|captures| {
        let c = captures.iter_mut().find(|c| c.id == id)?;
        if c.pending == c.active {
            return None;
        }
        let lost = c.active;
        c.active = c.pending;
        Some((lost, c.active))
    })
}

/// Drop pointer `id`'s capture outright: the node holding it left the tree.
pub(crate) fn forget_pointer_capture(id: u64) {
    with_captures(|captures| {
        let c = entry(captures, id);
        c.pending = None;
        c.active = None;
    });
}

/// Drop every capture, as a reset does.
pub fn clear_pointer_captures() {
    with_captures(Vec::clear);
}

/// Serialises the process-global binding registry and current-event cell
/// across every test that touches them: the ones in this module and the
/// dispatch tests in [`crate::dom_events`]. One lock, or a
/// `clear_all_bindings` in one module wipes what another just registered.
#[cfg(test)]
pub(crate) static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::TEST_GUARD as GUARD;
    use super::*;
    use std::sync::Mutex;

    fn record(log: &Arc<Mutex<Vec<String>>>, tag: &'static str) -> Arc<dyn Fn() + Send + Sync> {
        let log = log.clone();
        Arc::new(move || log.lock().unwrap().push(tag.to_string()))
    }

    #[test]
    fn native_handler_fires_with_event_fields() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let seen = Arc::new(Mutex::new((0u64, String::new())));
        let s = seen.clone();
        let cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            *s.lock().unwrap() = (event_target(), event_key());
        });
        register_native_binding(10, "keydown".into(), false, cb);
        let data = EventData {
            event_type: "keydown".into(),
            target: 10,
            key: "Enter".into(),
            ..Default::default()
        };
        dispatch(data, &[], event_bubbles("keydown"), |_| {});
        assert_eq!(*seen.lock().unwrap(), (10, "Enter".to_string()));
        clear_all_bindings();
    }

    #[test]
    fn capture_then_target_then_bubble_order() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let log = Arc::new(Mutex::new(Vec::new()));
        // Tree: root(1) -> mid(2) -> leaf(3). Bind capture + bubble at each.
        register_native_binding(1, "click".into(), true, record(&log, "root-cap"));
        register_native_binding(1, "click".into(), false, record(&log, "root-bub"));
        register_native_binding(2, "click".into(), true, record(&log, "mid-cap"));
        register_native_binding(2, "click".into(), false, record(&log, "mid-bub"));
        register_native_binding(3, "click".into(), true, record(&log, "leaf-cap"));
        register_native_binding(3, "click".into(), false, record(&log, "leaf-bub"));
        let data = EventData {
            event_type: "click".into(),
            target: 3,
            ..Default::default()
        };
        dispatch(data, &[1, 2], true, |_| {});
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "root-cap", "mid-cap", // capture down
                "leaf-cap", "leaf-bub", // target (registration order)
                "mid-bub", "root-bub", // bubble up
            ]
        );
        clear_all_bindings();
    }

    #[test]
    fn stop_propagation_halts_after_current_node() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        register_native_binding(
            2,
            "click".into(),
            false,
            Arc::new(move || {
                l.lock().unwrap().push("mid".to_string());
                event_stop_propagation();
            }),
        );
        register_native_binding(1, "click".into(), false, record(&log, "root"));
        let data = EventData {
            event_type: "click".into(),
            target: 2,
            ..Default::default()
        };
        dispatch(data, &[1], true, |_| {});
        // mid runs, stops propagation; root (bubble) never runs.
        assert_eq!(*log.lock().unwrap(), vec!["mid"]);
        clear_all_bindings();
    }

    #[test]
    fn stop_immediate_halts_remaining_on_same_node() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        register_native_binding(
            5,
            "click".into(),
            false,
            Arc::new(move || {
                l.lock().unwrap().push("first".to_string());
                event_stop_immediate_propagation();
            }),
        );
        register_native_binding(5, "click".into(), false, record(&log, "second"));
        let data = EventData {
            event_type: "click".into(),
            target: 5,
            ..Default::default()
        };
        dispatch(data, &[], true, |_| {});
        assert_eq!(*log.lock().unwrap(), vec!["first"]);
        clear_all_bindings();
    }

    #[test]
    fn prevent_default_records_on_returned_event() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        register_native_binding(7, "click".into(), false, Arc::new(event_prevent_default));
        let data = EventData {
            event_type: "click".into(),
            target: 7,
            ..Default::default()
        };
        let out = dispatch(data, &[], true, |_| {});
        assert!(out.default_prevented);
        clear_all_bindings();
    }

    #[test]
    fn unbind_stops_delivery() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let log = Arc::new(Mutex::new(Vec::new()));
        let tok = register_native_binding(9, "click".into(), false, record(&log, "hit"));
        assert!(unregister_binding(tok).is_some());
        let data = EventData {
            event_type: "click".into(),
            target: 9,
            ..Default::default()
        };
        dispatch(data, &[], true, |_| {});
        assert!(log.lock().unwrap().is_empty());
        clear_all_bindings();
    }

    #[test]
    fn non_bubbling_event_skips_ancestors() {
        let _g = GUARD.lock().unwrap();
        clear_all_bindings();
        let log = Arc::new(Mutex::new(Vec::new()));
        register_native_binding(1, "focus".into(), false, record(&log, "root-bub"));
        register_native_binding(2, "focus".into(), false, record(&log, "target"));
        let data = EventData {
            event_type: "focus".into(),
            target: 2,
            ..Default::default()
        };
        dispatch(data, &[1], event_bubbles("focus"), |_| {});
        // focus does not bubble: only the target handler runs.
        assert_eq!(*log.lock().unwrap(), vec!["target"]);
        clear_all_bindings();
    }
}
