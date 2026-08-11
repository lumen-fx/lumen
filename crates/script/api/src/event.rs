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
//! functions here. rhai / lua wrap those in a registered `Event` handle;
//! candela and the C-ABI call them procedurally.
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
    /// Pointer button: `0` primary, `1` secondary, `2` middle, `-1` none.
    pub button: i64,
    /// Wheel scroll delta, logical pixels.
    pub delta: (f64, f64),
    /// Text value for `input` / `change` events.
    pub value: String,
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

/// Drop all bindings (mainly for test isolation).
pub fn clear_all_bindings() {
    if let Ok(mut b) = bindings().write() {
        b.clear();
    }
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
// `click`'s default action is link navigation (a `<a href>` click navigates
// the active page). The nav executor lives in the runtime's
// `navigate_on_anchor_click`; the phase-4 dispatcher records here which click
// targets had their default cancelled so that executor can skip them. The set
// is cleared at the start of every dispatch pass.
// ---------------------------------------------------------------------------

static PREVENTED_CLICKS: OnceLock<RwLock<HashSet<u64>>> = OnceLock::new();

fn prevented_clicks() -> &'static RwLock<HashSet<u64>> {
    PREVENTED_CLICKS.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Reset the per-tick prevented-click set. The dispatcher calls this before
/// processing a tick's events.
pub fn clear_prevented_clicks() {
    if let Ok(mut s) = prevented_clicks().write() {
        s.clear();
    }
}

/// Record that a click on `target` (packed handle) had its default action
/// cancelled via `prevent_default`.
pub fn mark_prevented_click(target: u64) {
    if let Ok(mut s) = prevented_clicks().write() {
        s.insert(target);
    }
}

/// Whether a click on `target` (packed handle) was default-prevented this
/// tick. The anchor-navigation executor consults this to honor
/// `prevent_default`.
pub fn is_click_default_prevented(target: u64) -> bool {
    prevented_clicks()
        .read()
        .map(|s| s.contains(&target))
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise the process-global registry / cell across the tests in this
    // module so they don't clobber each other.
    static GUARD: Mutex<()> = Mutex::new(());

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
