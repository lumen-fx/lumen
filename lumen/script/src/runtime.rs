//! Host-generic script runtime.
//!
//! Everything in the scripting contract that is not compilation, value
//! conversion, or invocation lives here, parameterized over
//! `H: `[`ScriptHost`]: the 18-event dispatch surface with per-id handler
//! routing, the derivation fixed-point driver, the store->mirror sync
//! policy driver, timers, HTTP fetch plumbing, the load-failure banner
//! protocol, and the [`ScriptPlugin`] that wires it all into the tick.
//!
//! Concrete hosts (`lumen-script-rhai`, future `-candela`) provide engine +
//! builtins + conversion and hand a built host to [`ScriptPlugin::new`].
//!
//! ## Ordering contract (7bfc0f2 - do not regress)
//!
//! ```text
//! Systems:
//!   1. control pushes (push_toggle/slider/textinput/scroll_to_signal)
//!   2. sync_signals_into_host .after(all pushes)   - type-preserving parse-back
//!   3. tick_script .after(sync) + all event dispatchers
//!   4. embedder's apply_script_commands .after(dispatchers) .before(derivations, readers)
//!   5. apply_derivations .after(sync)              - fixed point, <= 32 passes
//!   6. dirty-gated binding readers
//! A11ySync: clear_property_store_dirty
//! ```
//!
//! Dirty flags live exactly one tick; every consumer of "what changed"
//! must be ordered inside that window.

use bevy_ecs::component::Mutable;
use bevy_ecs::message::{Message, MessageReader, MessageRegistry, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use std::time::Instant;

use crate::dnd;
use crate::{CallOutcome, ScriptCommand, ScriptError, ScriptHost, ScriptValue};

/// One [`ScriptCommand`] flowing through the ECS message bus so app
/// systems can read it via `MessageReader<ScriptCommandEvent>`.
#[derive(Message, Clone, Debug)]
pub struct ScriptCommandEvent(pub ScriptCommand);

/// Inserted by [`ScriptPlugin::build`] when the initial script failed to
/// compile or evaluate. Embedders (lumenc's `run`) surface it
/// prominently - e.g. through the in-window error banner - so a dead
/// script never masquerades as a healthy app that just ignores clicks.
/// Absent when the script loaded cleanly.
#[derive(Resource, Debug, Clone)]
pub struct ScriptLoadFailure(pub String);

/// Wall-clock moment the script plugin was installed. Kept for embedders
/// that want elapsed-since-install (the FFI surface reads it); the
/// runtime itself is reactive-only and no longer computes a per-frame
/// `t`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ScriptStartedAt(pub Instant);

/// Per-call diagnostics prefix: `lumen-script-<lang>`. Reproduces the
/// historical `lumen-script-rhai:` stderr prefixes exactly for the Rhai
/// host.
pub(crate) fn prefix(lang: &str) -> String {
    format!("lumen-script-{lang}")
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

/// Generic script plugin: loads `source` into the supplied host, fires
/// `on_start` (re-stashing its commands into the sink so they flow
/// through the first tick's normal drain), installs the host as a
/// `Resource`, and registers the full dispatcher / derivation / timer /
/// fetch system set.
///
/// Host construction (engine limits, builtin registration, embedder
/// extensions) happens BEFORE this plugin: build the host, apply any
/// host-specific extensions, then hand it over. `lumen-script-rhai`'s
/// `ScriptRhaiPlugin` is a thin wrapper doing exactly that.
pub struct ScriptPlugin<H: ScriptHost + Resource<Mutability = Mutable>> {
    host: H,
    source: String,
    uri: String,
}

impl<H: ScriptHost + Resource<Mutability = Mutable>> ScriptPlugin<H> {
    /// Wrap a built host + script source. The source URI defaults to
    /// `"<inline>"` (matches the historical compile-error shape).
    pub fn new(host: H, source: impl Into<String>) -> Self {
        Self {
            host,
            source: source.into(),
            uri: "<inline>".to_string(),
        }
    }

    /// Override the source URI reported in compile errors.
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = uri.into();
        self
    }
}

impl<H: ScriptHost + Resource<Mutability = Mutable>> Plugin for ScriptPlugin<H> {
    fn build(mut self, app: &mut App) {
        let lang = self.host.lang();
        if let Err(e) = self.host.load(&self.source, &self.uri) {
            // Unmissable, multi-line stderr banner - a load failure kills
            // every handler / signal / derivation while the window keeps
            // rendering, which historically read as "the app ignores
            // clicks" rather than "the script is dead".
            eprintln!(
                "\n\
                 ================================================================\n\
                 lumen-script-{lang}: SCRIPT LOAD FAILED\n\
                 \n\
                   {e}\n\
                 \n\
                 The window will still open, but every event handler, signal,\n\
                 and derivation in this script is DISABLED.\n\
                 Run `lumenc check <dir>` to reproduce this error in CI.\n\
                 ================================================================\n"
            );
            app.world.insert_resource(ScriptLoadFailure(e.to_string()));
        }
        // Fire on_start once now that the program is loaded. Any commands
        // it produced are re-stashed into the sink and drained on the
        // first tick through the normal ScriptCommandEvent path
        // (apply_script_commands is downstream of the message bus, not
        // the sink).
        match self.host.call("on_start", &[]) {
            Ok(outcome) => self.host.push_commands(outcome.commands),
            Err(e) => eprintln!("{}: on_start failed: {e}", prefix(lang)),
        }
        app.world.insert_resource(self.host);
        app.world.insert_resource(ScriptStartedAt(Instant::now()));
        // Latch for the post-mount `on_ready` dispatch (see `fire_on_ready`).
        app.world.insert_resource(OnReadyFired::default());
        app.world.insert_resource(TimerRegistry::default());
        app.world.insert_resource(FetchRegistry::default());
        MessageRegistry::register_message::<ScriptCommandEvent>(&mut app.world);
        // Toggle / slider dispatchers below read these messages. In
        // production `lumen-primitives::ControlsPlugin` registers them
        // first; we self-register defensively so tests that drive the
        // script host without `ControlsPlugin` still bring up a valid
        // schedule. Idempotent: already-initialised resources stay.
        app.world
            .init_resource::<bevy_ecs::message::Messages<lumen_primitives::ToggleChanged>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<lumen_primitives::SliderChanged>>();
        // DnD dispatchers below read these. `lumen-os-dnd::DndPlugin`
        // registers them in production; self-register defensively so a
        // script host without DndPlugin still brings up a valid schedule.
        app.world
            .init_resource::<bevy_ecs::message::Messages<lumen_os_dnd::DropAccepted>>();
        app.world
            .init_resource::<bevy_ecs::message::Messages<lumen_os_dnd::DragStarted>>();
        // `dispatch_text_input_to_script` reads the keyboard-edit queue.
        // `lumen-input::InputPlugin` registers it in production; self-register
        // so a script host without the input layer still brings up a valid
        // schedule.
        app.world
            .init_resource::<bevy_ecs::message::Messages<lumen_core::text_events::TextEditApplied>>(
            );
        // Foundation property store: defensively init so bare tests that
        // skip `App::new()`'s standard resources still bring up a valid
        // schedule for the store consumers.
        app.world
            .init_resource::<lumen_core::property_store::PropertyStore>();
        // The foundation typed-property bus drain is registered globally
        // by `App::new()` in `TickStage::CommandDrain` (before `Systems`),
        // so typed writes from host builtins are already committed to
        // `PropertyStore` by the time `sync_signals_into_host` and
        // `apply_derivations` read it. Idempotent init.
        lumen_core::property_store::init_external_properties();
        // `.after(push_*)`: the two-way binding pushes (toggle flip,
        // slider drag, keystroke mirror) write the store mid-Systems, and
        // their dirty flags are cleared at end of tick. Unordered, this
        // mirror sync can run before the push on the write tick - the
        // host-local map then keeps the stale value, and since the key is
        // never dirty again, `apply_derivations` (which reads dep values
        // through the mirror) recomputes with the OLD value and the
        // derived signal freezes. The widget garden's `toggle_status`
        // label was the live repro.
        app.add_systems(
            TickStage::Systems,
            sync_signals_into_host::<H>
                .after(lumen_core::signals::push_toggle_to_signal)
                .after(lumen_core::signals::push_slider_to_signal)
                .after(lumen_core::signals::push_textinput_to_signal)
                .after(lumen_core::signals::push_scroll_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            tick_script::<H>.after(sync_signals_into_host::<H>),
        );
        app.add_systems(
            TickStage::Systems,
            apply_derivations::<H>.after(sync_signals_into_host::<H>),
        );
        app.add_systems(
            TickStage::Systems,
            fire_due_timers::<H>.after(tick_script::<H>),
        );
        app.add_systems(
            TickStage::Systems,
            drain_fetch_commands.after(tick_script::<H>),
        );
        app.add_systems(
            TickStage::Systems,
            fire_fetched_responses::<H>.after(drain_fetch_commands),
        );
        // Event dispatchers: forward Click / LongPress / DoubleClick to
        // the script's `on_click(id)` / `on_long_press(id)` /
        // `on_double_click(id)` functions with the entity's LumenId. Run
        // after lumen-input's dispatch_clicks so the Click message is
        // populated for this tick. Click + double-click are dispatched
        // together so we can suppress the trailing on_click when a
        // DoubleClickEvent fires on the same entity.
        app.add_systems(
            TickStage::Systems,
            dispatch_clicks_and_doubles::<H>
                .after(lumen_input::dispatch_clicks)
                .after(lumen_primitives::press::detect_double_click),
        );
        app.add_systems(TickStage::Systems, dispatch_long_press_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_text_input_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_file_drops_to_script::<H>);
        app.add_systems(TickStage::Systems, dnd::dispatch_drops_to_script::<H>);
        app.add_systems(TickStage::Systems, dnd::dispatch_drag_start_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_file_picks_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_hotkeys_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_menu_clicks_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_dialog_closes_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_tray_clicks_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_toggle_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_slider_to_script::<H>);
        app.add_systems(TickStage::Systems, dispatch_close_to_script::<H>);
        // Must run after `fire_due_timers`: a repeating timer cancelled
        // from inside its own `on_timer` emits a `CancelTimer` during the
        // firing pass. Without this ordering the cancel could be drained a
        // tick late - after `fire_due_timers` had already re-armed the timer
        // and fired it one extra time. Draining right after the firing pass
        // applies the cancel on the same tick, before the next re-fire.
        app.add_systems(
            TickStage::Systems,
            drain_timer_commands
                .after(tick_script::<H>)
                .after(fire_due_timers::<H>),
        );
    }
}

/// Hot-reload orchestration entry: swap the loaded program on the live
/// host resource via [`ScriptHost::replace`] (compile-first, atomic,
/// full rollback on eval failure - the host owns the atomicity).
/// Returns `None` when no host is installed (script-less app).
pub fn reload_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    world: &mut World,
    source: &str,
    uri: &str,
) -> Option<Result<(), ScriptError>> {
    world
        .get_resource_mut::<H>()
        .map(|mut host| host.replace(source, uri))
}

// ---------------------------------------------------------------------
// Core per-tick systems
// ---------------------------------------------------------------------

/// Per-tick driver that drains the host's command sink and forwards
/// every [`ScriptCommand`] onto the [`ScriptCommandEvent`] message bus.
/// On the first tick this also flushes the commands re-stashed by
/// `on_start` (see [`ScriptPlugin::build`]).
///
/// `pub` so the embedder (lumenc) can order its command applier
/// `.after(tick_script::<H>)` - required post dirty-gating so a signal
/// write produced here lands in `PropertyStore` *and* is observed by the
/// pull-binding readers within the same tick, before
/// `clear_property_store_dirty` empties the dirty queue in `A11ySync`.
pub fn tick_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageWriter<ScriptCommandEvent>,
) {
    for c in host.drain_commands() {
        events.write(ScriptCommandEvent(c));
    }
}

/// One-shot latch guarding [`fire_on_ready`]: flipped true the first time
/// `on_ready` is dispatched so it never fires again.
#[derive(Resource, Default)]
pub struct OnReadyFired(pub bool);

/// Dispatch the script's optional `on_ready()` exactly once, on the first tick
/// after the DOM index is published.
///
/// `on_start` runs at app-construction time, before any tick, when no element
/// is queryable yet: a `node_get_by_id` there returns 0. That forced DOM apps
/// to defer their first tree build behind a `set_timeout("boot", 0)` timer.
/// `on_ready` closes that gap: it runs after the first `build_dom_index`
/// publish, so a query inside it observes the mounted static tree and the app
/// can build its initial DOM directly. A script without `on_ready` is a no-op
/// (the host reports the missing handler as not-found), so existing `on_start`
/// apps are unaffected. Commands `on_ready` emits flow onto the event bus like
/// any handler's, and this system is ordered before the DOM-command collector
/// so those mutations apply on the same first tick.
pub fn fire_on_ready<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut fired: ResMut<OnReadyFired>,
    mut events: MessageWriter<ScriptCommandEvent>,
) {
    if fired.0 {
        return;
    }
    fired.0 = true;
    match host.call("on_ready", &[]) {
        Ok(outcome) => {
            for c in outcome.commands {
                events.write(ScriptCommandEvent(c));
            }
        }
        Err(e) => eprintln!("{}: on_ready failed: {e}", prefix(host.lang())),
    }
}

/// Mirror the ECS-side global [`lumen_core::property_store::PropertyStore`]
/// cells into the host's rich-typed local mirror at the start of every
/// tick, walking only the keys dirtied this tick. Without this, the
/// script's signal reads see stale values when another system (a `<for>`
/// reconciler, a two-way bind push, ...) wrote to the property store
/// between the previous tick and this one.
///
/// Strings only - boolean / numeric / colour cells stay typed in the
/// store. Per-key overwrite policy (type-preserving parse-back) is
/// pinned by [`ScriptHost::mirror_sync_str`].
pub fn sync_signals_into_host<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    store: Res<lumen_core::property_store::PropertyStore>,
) {
    // Idle-tick fast path: nothing was written to the store this tick,
    // so the host-local mirror is already current. The dirty queue is
    // only cleared in A11ySync (after this system runs at the top of
    // Systems), so an empty queue means no global cell changed.
    if store.dirty_peek().is_empty() {
        return;
    }
    for key in store.dirty_peek() {
        let lumen_core::property_store::PropertyKey::Global(name) = key else {
            continue;
        };
        let Some(lumen_core::property_store::PropertyValue::Str(v)) = store.get(key) else {
            continue;
        };
        host.mirror_sync_str(name.as_ref(), v.as_ref());
    }
}

/// Re-evaluate every computed signal whose deps changed this tick.
///
/// Reads the per-tick `PropertyStore::dirty_global_names` set, snapshots
/// every derivation whose declared deps intersect that set (plus the
/// pending-initial set), evaluates each via
/// [`ScriptHost::eval_derivation`], and commits the result **directly
/// into the store** (marking it dirty) so binding readers ordered
/// `.after` this system observe the fresh derived value on the same tick
/// the dep changed.
///
/// Cascades (derived-of-derived) resolve within the tick: after each
/// evaluation wave, names whose stored value actually changed form the
/// dirty set for the next wave, looping to a fixed point (bounded by 32
/// passes; a self-referential cycle logs and stops). Pending-initial
/// names all run in wave 1 regardless of dirt; an erroring derivation
/// stays pending and retries next tick. Equal writes don't propagate
/// (`set_global_str` skips the dirty push).
///
/// Each write is also mirrored as a [`ScriptCommand::SetSignal`] event so
/// bare-plugin embedders that drain [`ScriptCommandEvent`] keep observing
/// derived values; under lumenc that replay is a next-tick equal-value
/// no-op.
pub fn apply_derivations<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut store: ResMut<lumen_core::property_store::PropertyStore>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    /// Upper bound on in-tick cascade waves. Real dependency chains are
    /// shallow (2-3); the bound only exists to break derivation cycles.
    const MAX_DERIVATION_PASSES: usize = 32;
    // Dirty-first: bail before touching the registry when nothing this
    // derivation could depend on changed. Only then snapshot the
    // matching subset (still outside any closure invocation so the host
    // holds no locks across re-entrant builtins).
    let mut pending = host.pending_initial();
    let mut dirty: std::collections::HashSet<String> =
        store.dirty_global_names().map(str::to_string).collect();
    if dirty.is_empty() && pending.is_empty() {
        return;
    }
    let mut evaluated: Vec<String> = Vec::new();
    let mut pass = 0;
    loop {
        let dirty_refs: std::collections::HashSet<&str> =
            dirty.iter().map(String::as_str).collect();
        let derivations = host.derivations_matching(&dirty_refs, &pending);
        if derivations.is_empty() {
            break;
        }
        if pass == MAX_DERIVATION_PASSES {
            eprintln!(
                "{}: derivation cascade exceeded {MAX_DERIVATION_PASSES} \
                 passes (cyclic derive()?); giving up for this tick",
                prefix(host.lang())
            );
            break;
        }
        pass += 1;
        // Names whose stored value actually changed this wave - the
        // dirty set for the next wave (derived-of-derived).
        let mut wrote: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (name, deps, closure) in derivations {
            let needs_initial = pending.contains(&name);
            match host.eval_derivation(&closure, &deps, &name) {
                Ok(text) => {
                    if store.set_global_str(&name, text.as_str()) {
                        wrote.insert(name.clone());
                    }
                    out.write(ScriptCommandEvent(ScriptCommand::SetSignal {
                        name: name.clone(),
                        value: text,
                    }));
                    if needs_initial {
                        evaluated.push(name);
                    }
                }
                Err(e) => {
                    eprintln!("{}: derive '{name}' failed: {e}", prefix(host.lang()));
                }
            }
        }
        // The initial-evaluation set is fully consumed by the first
        // wave (every pending name matches unconditionally); later
        // waves are dirty-driven only. Erroring derivations stay in
        // pending-initial (not `evaluated`) and retry next tick.
        pending.clear();
        dirty = wrote;
        if dirty.is_empty() {
            break;
        }
    }
    if !evaluated.is_empty() {
        host.clear_pending(&evaluated);
    }
}

// ---------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------

/// One active timer.
#[derive(Clone, Debug)]
struct ActiveTimer {
    /// Wall-clock moment the next fire is due.
    fire_at: Instant,
    /// Reschedule interval (`None` = one-shot).
    repeat_every: Option<std::time::Duration>,
}

/// Active timers keyed by name. Names are unique - setting a timer with
/// the same name replaces the previous one (matches set_interval /
/// set_timeout semantics in browsers).
#[derive(Resource, Default)]
pub struct TimerRegistry {
    timers: std::collections::HashMap<String, ActiveTimer>,
}

/// Read `SetTimer` / `CancelTimer` commands emitted by the script's
/// builtins this tick and update the [`TimerRegistry`] accordingly.
pub fn drain_timer_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut timers: ResMut<TimerRegistry>,
) {
    let now = Instant::now();
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::SetTimer {
                name,
                millis,
                repeat,
            } => {
                let dur = std::time::Duration::from_millis(*millis);
                timers.timers.insert(
                    name.clone(),
                    ActiveTimer {
                        fire_at: now + dur,
                        repeat_every: if *repeat { Some(dur) } else { None },
                    },
                );
            }
            ScriptCommand::CancelTimer { name } => {
                timers.timers.remove(name);
            }
            _ => {}
        }
    }
}

/// Fire `on_timer(name)` for every timer whose deadline has passed,
/// rescheduling repeating timers. Due timers fire in sorted-name order
/// (determinism); reschedule / remove happens BEFORE firing so handlers
/// that cancel_timer / set_interval on the same name see a clean slate.
pub fn fire_due_timers<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut timers: ResMut<TimerRegistry>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    let now = Instant::now();
    let mut due: Vec<String> = timers
        .timers
        .iter()
        .filter(|(_, t)| t.fire_at <= now)
        .map(|(name, _)| name.clone())
        .collect();
    // Stable order so test runs are deterministic.
    due.sort();
    for name in due {
        let next = timers
            .timers
            .get(&name)
            .and_then(|t| t.repeat_every.map(|d| now + d));
        match next {
            Some(fire_at) => {
                if let Some(t) = timers.timers.get_mut(&name) {
                    t.fire_at = fire_at;
                }
            }
            None => {
                timers.timers.remove(&name);
            }
        }
        if let Err(e) = route_event(&mut *host, "timer", "on_timer", &name, &mut out) {
            eprintln!("{}: on_timer({name}) failed: {e}", prefix(host.lang()));
        }
    }
}

// ---------------------------------------------------------------------
// HTTP fetch
// ---------------------------------------------------------------------

/// HTTP plumbing: spawn off-thread requests, marshal the completed
/// reply back onto the ECS/UI thread, and surface it to the script.
/// One std::thread per request - fine for the typical "a few API calls
/// per UI action" workload; can move to a pool later if it becomes a
/// bottleneck.
///
/// Both `fetch(url, tag)` (simple sugar) and `http(#{...})` (general form)
/// flow through this single registry, worker pool discipline, and
/// completion channel - there is exactly one async delivery mechanism.
/// The worker only ever *sends* the outcome down the channel; the
/// [`fire_fetched_responses`] system, running on the world thread,
/// drains it and is the only place a signal / handler is touched. That
/// worker->UI-thread hand-off mirrors Slint's `invoke_from_event_loop`
/// marshalling.
#[derive(Resource)]
pub struct FetchRegistry {
    sender: crossbeam_channel::Sender<HttpOutcome>,
    receiver: crossbeam_channel::Receiver<HttpOutcome>,
}

impl Default for FetchRegistry {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            sender: tx,
            receiver: rx,
        }
    }
}

/// How a completed request should be delivered back to the script.
#[derive(Clone, Copy)]
enum DeliveryStyle {
    /// `fetch()` sugar: fire `on_fetch(tag, body)` /
    /// `on_fetch_error(tag, msg)`, treating a non-2xx status as an error
    /// (preserves the historical `fetch()` contract).
    Fetch,
    /// `http()`: fire `on_http(tag, response)` with a structured map;
    /// a non-2xx status is a *completed* reply, not an error.
    Http,
}

/// A request handed to a worker thread. Method + url + headers + body in:
/// the input half of the Qt `QNetworkRequest` shape.
#[cfg_attr(not(feature = "http-fetch"), allow(dead_code))]
struct HttpRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout_ms: Option<u64>,
    tag: String,
}

/// The reply half of the Qt `QNetworkReply` shape: status + headers +
/// body out.
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct HttpOutcome {
    tag: String,
    style: DeliveryStyle,
    /// `Ok` = the request completed and a reply (any status) came back.
    /// `Err` = transport failure (DNS, connect, timeout, bad method/url).
    result: Result<HttpResponse, String>,
}

/// Read `Fetch` / `Http` commands the script emitted this tick and start
/// an off-thread request for each. Other variants are no-ops here (they
/// flow to apply_script_commands and timer drains separately).
pub fn drain_fetch_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    fetcher: Res<FetchRegistry>,
) {
    for ev in events.read() {
        let (req, style) = match &ev.0 {
            ScriptCommand::Fetch { url, tag } => (
                HttpRequest {
                    method: "GET".to_string(),
                    url: url.clone(),
                    headers: Vec::new(),
                    body: None,
                    timeout_ms: None,
                    tag: tag.clone(),
                },
                DeliveryStyle::Fetch,
            ),
            ScriptCommand::Http {
                method,
                url,
                headers,
                body,
                timeout_ms,
                tag,
            } => (
                HttpRequest {
                    method: method.clone(),
                    url: url.clone(),
                    headers: headers.clone(),
                    body: body.clone(),
                    timeout_ms: *timeout_ms,
                    tag: tag.clone(),
                },
                DeliveryStyle::Http,
            ),
            _ => continue,
        };
        // Dev-tooling capture (no-op unless the devtools sink is installed):
        // report the dispatch so the Network tab shows the in-flight request
        // before its reply lands.
        lumen_core::net_capture::record(lumen_core::net_capture::NetEvent::Started {
            tag: req.tag.clone(),
            method: req.method.clone(),
            url: req.url.clone(),
        });
        let tx = fetcher.sender.clone();
        let tag = req.tag.clone();
        std::thread::Builder::new()
            .name(format!("lumen-http:{tag}"))
            .spawn(move || {
                let result = perform_http(&req);
                let _ = tx.send(HttpOutcome {
                    tag: req.tag,
                    style,
                    result,
                });
            })
            .expect("spawn http thread");
    }
}

/// Blocking HTTP request -> structured reply. Runs on the per-request
/// worker thread. A 4xx / 5xx is returned as an `Ok(HttpResponse)` (the
/// caller decides what a non-2xx means); only transport failures map to
/// `Err`. Uses the workspace `ureq` dep - no new HTTP client is pulled
/// in for this.
/// Hard cap on the response body we buffer into memory, in bytes (16 MiB).
/// A huge or open-ended (chunked / streaming) endpoint must not be able to
/// OOM the per-request worker: reads past this bound abort with an error
/// that is surfaced to the script as the reply's `error` field. Callers who
/// legitimately need larger payloads should stream, not `fetch`.
#[cfg(feature = "http-fetch")]
const MAX_HTTP_BODY_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(feature = "http-fetch")]
fn perform_http(req: &HttpRequest) -> Result<HttpResponse, String> {
    perform_http_capped(req, MAX_HTTP_BODY_BYTES)
}

#[cfg(feature = "http-fetch")]
fn perform_http_capped(req: &HttpRequest, body_limit: u64) -> Result<HttpResponse, String> {
    use std::time::Duration;

    let method = ureq::http::Method::from_bytes(req.method.to_ascii_uppercase().as_bytes())
        .map_err(|_| format!("invalid HTTP method: {}", req.method))?;

    // `http_status_as_error(false)` = web-`fetch` semantics: a 4xx / 5xx
    // is a completed reply, not an `Err`. `timeout_global` applies the
    // per-request deadline (Qt `QNetworkRequest` transfer timeout).
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(req.timeout_ms.map(Duration::from_millis))
        .build();
    let agent: ureq::Agent = config.into();

    let mut builder = ureq::http::Request::builder()
        .method(method)
        .uri(req.url.as_str());
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }

    // GET-style requests with no body send `()`; anything with an
    // explicit body sends the string. Both `()` and `String` implement
    // `AsSendBody`, so `agent.run` accepts either.
    let reply = match &req.body {
        Some(b) => agent.run(builder.body(b.clone()).map_err(|e| e.to_string())?),
        None => agent.run(builder.body(()).map_err(|e| e.to_string())?),
    };
    let mut reply = reply.map_err(|e| e.to_string())?;

    let status = reply.status().as_u16();
    let headers = reply
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    // Bounded read: `Body::read_to_string()` would use ureq's implicit
    // default; make the cap explicit and named so a body past `body_limit`
    // deterministically errors (surfaced as the reply's `error`) instead of
    // relying on the transport's default and risking an OOM if that default
    // ever changes. `lossy_utf8(true)` preserves the previous non-UTF-8
    // behaviour (invalid bytes -> `?`).
    let body = reply
        .body_mut()
        .with_config()
        .limit(body_limit)
        .lossy_utf8(true)
        .read_to_string()
        .map_err(|e| format!("read body (cap {body_limit} bytes): {e}"))?;

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Size-trimmed builds (`--no-default-features`): every `fetch()` /
/// `http()` call resolves to the error path with a clear rebuild hint
/// instead of silently vanishing.
#[cfg(not(feature = "http-fetch"))]
fn perform_http(_req: &HttpRequest) -> Result<HttpResponse, String> {
    Err(
        "the script runtime was built without the `http-fetch` feature; \
         fetch() / http() are disabled in this binary"
            .to_string(),
    )
}

/// Build the structured `on_http` response map from a completed request.
/// Shape is `web`-`fetch`-like / Qt `QNetworkReply`-like:
/// `#{ ok, status, headers, body, error }`. Header names are lowercased
/// (HTTP header names are case-insensitive; lowercasing gives scripts a
/// stable key to index by).
fn http_response_to_value(result: &Result<HttpResponse, String>) -> ScriptValue {
    use std::collections::HashMap;
    let mut map: HashMap<String, ScriptValue> = HashMap::new();
    match result {
        Ok(resp) => {
            let ok = (200..300).contains(&resp.status);
            let mut headers: HashMap<String, ScriptValue> = HashMap::new();
            for (k, v) in &resp.headers {
                headers.insert(k.to_ascii_lowercase(), ScriptValue::Str(v.clone()));
            }
            map.insert("ok".to_string(), ScriptValue::Bool(ok));
            map.insert("status".to_string(), ScriptValue::I64(resp.status as i64));
            map.insert("headers".to_string(), ScriptValue::Map(headers));
            map.insert("body".to_string(), ScriptValue::Str(resp.body.clone()));
            map.insert("error".to_string(), ScriptValue::Str(String::new()));
        }
        Err(err) => {
            // Transport failure (DNS, connect, timeout): still a
            // structured reply the script can branch on, never a panic.
            map.insert("ok".to_string(), ScriptValue::Bool(false));
            map.insert("status".to_string(), ScriptValue::I64(0));
            map.insert("headers".to_string(), ScriptValue::Map(HashMap::new()));
            map.insert("body".to_string(), ScriptValue::Str(String::new()));
            map.insert("error".to_string(), ScriptValue::Str(err.clone()));
        }
    }
    ScriptValue::Map(map)
}

/// Drain finished HTTP replies (marshalled back from the worker threads)
/// and invoke the script's completion handler once for each. This is the
/// only place a network reply crosses into script/signal land - the
/// worker never touches the world, mirroring Slint's
/// `invoke_from_event_loop`.
///
/// `fetch()` replies preserve the historical contract: `on_fetch(tag,
/// body)` on 2xx, `on_fetch_error(tag, msg)` on transport failure or
/// non-2xx. `http()` replies fire `on_http(tag, response)` with the
/// structured map for every completed request.
pub fn fire_fetched_responses<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    fetcher: Res<FetchRegistry>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    while let Ok(outcome) = fetcher.receiver.try_recv() {
        // Dev-tooling capture (no-op unless the devtools sink is installed):
        // pair the reply with its in-flight request by `tag`.
        {
            let (ok, status, error) = match &outcome.result {
                Ok(resp) => (true, resp.status, String::new()),
                Err(err) => (false, 0u16, err.clone()),
            };
            lumen_core::net_capture::record(lumen_core::net_capture::NetEvent::Completed {
                tag: outcome.tag.clone(),
                ok,
                status,
                error,
            });
        }
        match outcome.style {
            DeliveryStyle::Fetch => {
                let (event_name, fallback_fn, payload) = match &outcome.result {
                    Ok(resp) if (200..300).contains(&resp.status) => {
                        ("fetch", "on_fetch", resp.body.clone())
                    }
                    Ok(resp) => (
                        "fetch_error",
                        "on_fetch_error",
                        format!("HTTP status {}", resp.status),
                    ),
                    Err(err) => ("fetch_error", "on_fetch_error", err.clone()),
                };
                if let Err(e) = route_event_two_args(
                    &mut *host,
                    event_name,
                    fallback_fn,
                    &outcome.tag,
                    &payload,
                    &mut out,
                ) {
                    eprintln!(
                        "{}: {fallback_fn}({}) failed: {e}",
                        prefix(host.lang()),
                        outcome.tag
                    );
                }
            }
            DeliveryStyle::Http => {
                let response = http_response_to_value(&outcome.result);
                if let Err(e) = route_event_key_value(
                    &mut *host,
                    "http",
                    "on_http",
                    &outcome.tag,
                    response,
                    &mut out,
                ) {
                    eprintln!(
                        "{}: on_http({}) failed: {e}",
                        prefix(host.lang()),
                        outcome.tag
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// Event routing helpers
// ---------------------------------------------------------------------

/// Push a [`CallOutcome`]'s commands onto the message bus.
fn forward_outcome(outcome: CallOutcome, out: &mut MessageWriter<ScriptCommandEvent>) {
    for c in outcome.commands {
        out.write(ScriptCommandEvent(c));
    }
}

/// Resolve the actual function name to call: per-id handler (registered
/// via `on(event, key, fn_name)`) if present, else the supplied
/// fallback. `key` doubles as the "id_str" for entity-keyed events and
/// the "tag/name" for fetch/timer events.
fn resolve_handler<H: ScriptHost>(host: &H, event_name: &str, key: &str, fallback: &str) -> String {
    host.handler_for(event_name, key)
        .unwrap_or_else(|| fallback.to_string())
}

/// Dispatch a single-arg event through the host's per-id handler
/// registry. If `on(event, key, fn_name)` registered a specific handler
/// for `(event_name, key)`, call that fn name with `key` as the single
/// arg. Otherwise fall through to the global `<fallback_fn>(key)`.
fn route_event<H: ScriptHost + Resource<Mutability = Mutable>>(
    host: &mut H,
    event_name: &str,
    fallback_fn: &str,
    id_str: &str,
    out: &mut MessageWriter<ScriptCommandEvent>,
) -> Result<(), ScriptError> {
    let target = resolve_handler(&*host, event_name, id_str, fallback_fn);
    let outcome = host.call(&target, &[ScriptValue::Str(id_str.to_string())])?;
    forward_outcome(outcome, out);
    Ok(())
}

/// Two-arg variant of [`route_event`]: per-id handler lookup keyed by
/// the first arg, falling back to the global handler. Used for
/// `on_text_input(id, text)`, `on_file_dropped(id, path)`,
/// `on_fetch(tag, body)` / `on_fetch_error(tag, msg)`.
pub(crate) fn route_event_two_args<H: ScriptHost + Resource<Mutability = Mutable>>(
    host: &mut H,
    event_name: &str,
    fallback_fn: &str,
    key: &str,
    arg2: &str,
    out: &mut MessageWriter<ScriptCommandEvent>,
) -> Result<(), ScriptError> {
    let target = resolve_handler(&*host, event_name, key, fallback_fn);
    let outcome = host.call(
        &target,
        &[
            ScriptValue::Str(key.to_string()),
            ScriptValue::Str(arg2.to_string()),
        ],
    )?;
    forward_outcome(outcome, out);
    Ok(())
}

/// Variant of [`route_event_two_args`] whose second argument is an
/// arbitrary structured [`ScriptValue`] rather than a string. Used for
/// `on_http(tag, response)`, where `response` is a
/// `#{ ok, status, headers, body, error }` map. Per-id handler lookup is
/// keyed by `key` (the request tag), falling back to the global handler.
fn route_event_key_value<H: ScriptHost + Resource<Mutability = Mutable>>(
    host: &mut H,
    event_name: &str,
    fallback_fn: &str,
    key: &str,
    value: ScriptValue,
    out: &mut MessageWriter<ScriptCommandEvent>,
) -> Result<(), ScriptError> {
    let target = resolve_handler(&*host, event_name, key, fallback_fn);
    let outcome = host.call(&target, &[ScriptValue::Str(key.to_string()), value])?;
    forward_outcome(outcome, out);
    Ok(())
}

/// Per-id-handler-aware variant for `(id: String, value: bool)` events;
/// currently `on_toggle(id, checked)`.
fn route_event_id_bool<H: ScriptHost + Resource<Mutability = Mutable>>(
    host: &mut H,
    event_name: &str,
    fallback_fn: &str,
    id_str: &str,
    value: bool,
    out: &mut MessageWriter<ScriptCommandEvent>,
) -> Result<(), ScriptError> {
    let target = resolve_handler(&*host, event_name, id_str, fallback_fn);
    let outcome = host.call(
        &target,
        &[
            ScriptValue::Str(id_str.to_string()),
            ScriptValue::Bool(value),
        ],
    )?;
    forward_outcome(outcome, out);
    Ok(())
}

/// Per-id-handler-aware variant for `(id: String, value: f64)` events;
/// currently `on_slider(id, value)`.
fn route_event_id_f64<H: ScriptHost + Resource<Mutability = Mutable>>(
    host: &mut H,
    event_name: &str,
    fallback_fn: &str,
    id_str: &str,
    value: f64,
    out: &mut MessageWriter<ScriptCommandEvent>,
) -> Result<(), ScriptError> {
    let target = resolve_handler(&*host, event_name, id_str, fallback_fn);
    let outcome = host.call(
        &target,
        &[
            ScriptValue::Str(id_str.to_string()),
            ScriptValue::F64(value),
        ],
    )?;
    forward_outcome(outcome, out);
    Ok(())
}

// ---------------------------------------------------------------------
// Event dispatchers
// ---------------------------------------------------------------------

/// Collect `items` into a `Vec` preserving first-seen order and dropping
/// duplicates. Used to give double-click dispatch a deterministic order
/// (message order) instead of the arbitrary iteration order of a `HashSet`.
fn collect_ordered_unique(items: impl IntoIterator<Item = Entity>) -> Vec<Entity> {
    let mut out: Vec<Entity> = Vec::new();
    for e in items {
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// Forward this tick's `ClickEvent` / `DoubleClickEvent` messages to the
/// script's `on_click(id)` / `on_double_click(id)` handlers. When both
/// fire for the same entity in the same tick the trailing `on_click` is
/// suppressed - a double-click counts as exactly one `on_double_click`,
/// not two clicks plus one double.
///
/// Exposed so the embedder can order same-tick consumers against it -
/// the reactive-binding readers run `.after` this system so a signal a
/// handler writes here is committed and reflected by `bind="..."` on the
/// very tick the click fired.
pub fn dispatch_clicks_and_doubles<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut clicks: MessageReader<ClickEvent>,
    mut doubles: MessageReader<DoubleClickEvent>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    // Collect double-click targets first; the second-of-pair Click was
    // also pushed this tick (dispatch_clicks fires Click on every
    // release, regardless of double-click status).
    //
    // Ordered, deduped list in first-seen message order. Iterating a
    // `HashSet` directly gives a nondeterministic dispatch order when two
    // entities are double-clicked in one tick, which violates the
    // deterministic-dispatch invariant (the single-click path below is
    // already message-ordered).
    let double_order: Vec<Entity> = collect_ordered_unique(doubles.read().map(|ev| ev.entity));

    let mut first_click_to_fire: Vec<ClickEvent> = Vec::new();
    for click in clicks.read() {
        // Doubled entities drop their single Clicks entirely -
        // double-click is the canonical signal; the script receives
        // on_double_click only. (`double_order` is tiny - at most a couple
        // of double-clicks per tick - so a linear `contains` is cheaper
        // than a `HashSet`.)
        if !double_order.contains(&click.entity) {
            first_click_to_fire.push(*click);
        }
    }

    for click in first_click_to_fire {
        let id_str = ids.get(click.entity).map(|i| i.0.as_str()).unwrap_or("");
        if let Err(e) = route_event(&mut *host, "click", "on_click", id_str, &mut out) {
            eprintln!("{}: on_click failed: {e}", prefix(host.lang()));
        }
    }
    for entity in double_order {
        let id_str = ids.get(entity).map(|i| i.0.as_str()).unwrap_or("");
        if let Err(e) = route_event(
            &mut *host,
            "double_click",
            "on_double_click",
            id_str,
            &mut out,
        ) {
            eprintln!("{}: on_double_click failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward the window backend's [`lumen_core::input::CloseRequest`] to
/// the script's `on_close()` lifecycle hook - *before* the backend tears
/// anything down, so scripts get a last chance to persist state.
///
/// Close veto: an `on_close` that returns `false` keeps the window open
/// (a fresh `CloseRequest { vetoed: true }` is written for the backend's
/// post-tick veto check). Any other return value, or no `on_close`
/// function at all, lets the close proceed.
///
/// Public so embedders can order their `ScriptCommandEvent` consumers
/// `.after` this dispatcher: on a committed close the veto tick is the
/// app's *last* tick, so commands `on_close` emits must be applied in
/// the same tick or they are silently dropped at exit.
pub fn dispatch_close_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut msgs: ResMut<bevy_ecs::message::Messages<lumen_core::input::CloseRequest>>,
    mut cursor: Local<bevy_ecs::message::MessageCursor<lumen_core::input::CloseRequest>>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    // Veto responses (`vetoed: true`) circulate on the same bus; only
    // genuine backend-emitted requests trigger the hook.
    let requests = cursor.read(&msgs).filter(|ev| !ev.vetoed).count();
    let mut veto = false;
    for _ in 0..requests {
        match host.call("on_close", &[]) {
            Ok(outcome) => {
                if matches!(&outcome.ret, Some(ScriptValue::Bool(false))) {
                    veto = true;
                }
                forward_outcome(outcome, &mut out);
            }
            Err(e) => eprintln!("{}: on_close failed: {e}", prefix(host.lang())),
        }
    }
    if veto {
        msgs.write(lumen_core::input::CloseRequest { vetoed: true });
    }
}

/// Forward [`lumen_primitives::ToggleChanged`] as `on_toggle(id,
/// checked)` (bool), with per-id handler routing via
/// `on("toggle", "<id>", "<fn>")`.
pub fn dispatch_toggle_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_primitives::ToggleChanged>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let id_str = ids.get(ev.entity).map(|i| i.0.as_str()).unwrap_or("");
        if let Err(e) = route_event_id_bool(
            &mut *host,
            "toggle",
            "on_toggle",
            id_str,
            ev.checked,
            &mut out,
        ) {
            eprintln!("{}: on_toggle failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_primitives::SliderChanged`] as `on_slider(id, value)`
/// (f64), with per-id handler routing via `on("slider", "<id>", "<fn>")`.
pub fn dispatch_slider_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_primitives::SliderChanged>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let id_str = ids.get(ev.entity).map(|i| i.0.as_str()).unwrap_or("");
        if let Err(e) = route_event_id_f64(
            &mut *host,
            "slider",
            "on_slider",
            id_str,
            ev.value as f64,
            &mut out,
        ) {
            eprintln!("{}: on_slider failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`LongPressEvent`] as `on_long_press(id)`.
pub fn dispatch_long_press_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<LongPressEvent>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let id_str = ids.get(ev.entity).map(|i| i.0.as_str()).unwrap_or("");
        if let Err(e) = route_event(&mut *host, "long_press", "on_long_press", id_str, &mut out) {
            eprintln!("{}: on_long_press failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`FileDropped`] as `on_file_dropped(id, path)`.
pub fn dispatch_file_drops_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<FileDropped>,
    ids: Query<&LumenId>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let id_str = ids.get(ev.entity).map(|i| i.0.as_str()).unwrap_or("");
        let path_str = ev.path.to_string_lossy().to_string();
        if let Err(e) = route_event_two_args(
            &mut *host,
            "file_dropped",
            "on_file_dropped",
            id_str,
            &path_str,
            &mut out,
        ) {
            eprintln!("{}: on_file_dropped failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::HotkeyFired`] to the script as
/// `on_hotkey(name)`, with the per-id `on("hotkey", name, fn)` router
/// applying first.
pub fn dispatch_hotkeys_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::HotkeyFired>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event(&mut *host, "hotkey", "on_hotkey", &ev.name, &mut out) {
            eprintln!("{}: on_hotkey failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::MenuClicked`] to the script as
/// `on_menu(id)`, with the per-id `on("menu", id, fn)` router applying
/// first.
pub fn dispatch_menu_clicks_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::MenuClicked>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event(&mut *host, "menu", "on_menu", &ev.id, &mut out) {
            eprintln!("{}: on_menu failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::DialogClosed`] (W5 dialog contract) to
/// the script: `accepted = true` routes as `on_dialog_accepted(id)` /
/// `on("dialog_accepted", id, fn)`, `accepted = false` as
/// `on_dialog_rejected(id)` / `on("dialog_rejected", id, fn)`. The
/// emitter guarantees exactly one message per open->close cycle.
pub fn dispatch_dialog_closes_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::DialogClosed>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let (event_name, fallback) = if ev.accepted {
            ("dialog_accepted", "on_dialog_accepted")
        } else {
            ("dialog_rejected", "on_dialog_rejected")
        };
        if let Err(e) = route_event(&mut *host, event_name, fallback, &ev.id, &mut out) {
            eprintln!("{}: {fallback} failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forwards [`lumen_core::input::TrayClicked`] as `on_tray(id)` and
/// routes per-id handlers registered via `on("tray", "<id>", "<fn>")`.
pub fn dispatch_tray_clicks_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::TrayClicked>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event(&mut *host, "tray", "on_tray", &ev.id, &mut out) {
            eprintln!("{}: on_tray failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::FilePicked`] dialog results to the
/// script. Open / Save / PickFolder fire `on_file_picked(tag, path)` /
/// `on_folder_picked(tag, path)`; multi-open joins the path list with
/// `|` and fires `on_files_picked(tag, joined)`. Cancellation still
/// fires once with an empty path so scripts can clear a "loading" state.
pub fn dispatch_file_picks_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::FilePicked>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        let (event_name, handler) = match ev.kind {
            "open" | "save" => ("file_picked", "on_file_picked"),
            "open_multi" => ("files_picked", "on_files_picked"),
            "folder" => ("folder_picked", "on_folder_picked"),
            other => {
                eprintln!("{}: unknown FilePicked kind '{other}'", prefix(host.lang()));
                continue;
            }
        };
        let joined = ev
            .paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("|");
        if let Err(e) =
            route_event_two_args(&mut *host, event_name, handler, &ev.tag, &joined, &mut out)
        {
            eprintln!("{}: {handler} failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward text changes as `on_text_input(id, text)`.
///
/// Fires on every edit that changes the text, which is what a live preview
/// needs, and once more when the field is committed with Enter. An entity
/// gets at most one call per tick: an IME commit both mutates the buffer
/// and raises [`TextInputCommitted`], and the script should see that as one
/// edit, not two.
pub fn dispatch_text_input_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut edits: MessageReader<lumen_core::text_events::TextEditApplied>,
    mut commits: MessageReader<TextInputCommitted>,
    ids: Query<&LumenId>,
    buffers: Query<&lumen_core::text_model::TextBuffer>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    use lumen_core::text_events::AppliedKind;

    let mut fired: Vec<Entity> = Vec::new();
    let fire =
        |host: &mut H, out: &mut MessageWriter<ScriptCommandEvent>, e: Entity, text: &str| {
            let id_str = ids.get(e).map(|i| i.0.as_str()).unwrap_or("");
            if let Err(err) =
                route_event_two_args(host, "text_input", "on_text_input", id_str, text, out)
            {
                eprintln!("{}: on_text_input failed: {err}", prefix(host.lang()));
            }
        };

    for ev in edits.read() {
        // A pure caret move is not a text change.
        if matches!(ev.kind, AppliedKind::CursorMove) || fired.contains(&ev.entity) {
            continue;
        }
        let Ok(buf) = buffers.get(ev.entity) else {
            continue;
        };
        fired.push(ev.entity);
        fire(&mut host, &mut out, ev.entity, &buf.to_string());
    }
    for ev in commits.read() {
        if fired.contains(&ev.entity) {
            continue;
        }
        fired.push(ev.entity);
        fire(&mut host, &mut out, ev.entity, &ev.text);
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    /// Double-click dispatch order is deterministic: the collected targets
    /// follow message (insertion) order, not `HashSet` iteration order, and
    /// duplicates collapse to their first occurrence.
    #[test]
    fn double_click_targets_are_message_ordered_and_deduped() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();

        // Deliberately out of id order, with a repeat.
        let order = collect_ordered_unique([c, a, b, a, c]);
        assert_eq!(order, vec![c, a, b], "first-seen order, deduped");

        // Stable across repeated runs regardless of entity id values.
        for _ in 0..8 {
            assert_eq!(collect_ordered_unique([b, c, a]), vec![b, c, a]);
        }
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::ScriptValue;

    /// The structured-map builder is pure and must expose exactly the
    /// `web`-`fetch`-like fields the script branches on.
    #[test]
    fn response_map_success_shape() {
        let resp = HttpResponse {
            status: 201,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: "{\"ok\":true}".to_string(),
        };
        let ScriptValue::Map(m) = http_response_to_value(&Ok(resp)) else {
            panic!("expected a map");
        };
        assert_eq!(m.get("ok"), Some(&ScriptValue::Bool(true)));
        assert_eq!(m.get("status"), Some(&ScriptValue::I64(201)));
        assert_eq!(
            m.get("body"),
            Some(&ScriptValue::Str("{\"ok\":true}".to_string()))
        );
        assert_eq!(m.get("error"), Some(&ScriptValue::Str(String::new())));
        // Header names are lowercased for stable indexing.
        let ScriptValue::Map(h) = m.get("headers").unwrap() else {
            panic!("headers not a map");
        };
        assert_eq!(
            h.get("content-type"),
            Some(&ScriptValue::Str("application/json".to_string()))
        );
    }

    /// A non-2xx is a completed reply (`ok=false`, real status), not an
    /// error - the script can branch on it.
    #[test]
    fn response_map_non_2xx_is_not_error() {
        let resp = HttpResponse {
            status: 404,
            headers: vec![],
            body: "nope".to_string(),
        };
        let ScriptValue::Map(m) = http_response_to_value(&Ok(resp)) else {
            panic!("expected a map");
        };
        assert_eq!(m.get("ok"), Some(&ScriptValue::Bool(false)));
        assert_eq!(m.get("status"), Some(&ScriptValue::I64(404)));
        assert_eq!(m.get("error"), Some(&ScriptValue::Str(String::new())));
    }

    /// A transport failure surfaces as structured data (`status=0`,
    /// populated `error`) rather than a panic.
    #[test]
    fn response_map_transport_error_shape() {
        let ScriptValue::Map(m) = http_response_to_value(&Err("dns boom".to_string())) else {
            panic!("expected a map");
        };
        assert_eq!(m.get("ok"), Some(&ScriptValue::Bool(false)));
        assert_eq!(m.get("status"), Some(&ScriptValue::I64(0)));
        assert_eq!(
            m.get("error"),
            Some(&ScriptValue::Str("dns boom".to_string()))
        );
    }

    /// End-to-end transport against a loopback server (no external
    /// endpoint). Verifies method + body round-trip in and status +
    /// header + body out - the full Qt-`QNetworkReply`-shaped reply.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn perform_http_round_trips_over_loopback() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();

        // Minimal one-shot HTTP/1.1 server: read the request head + body,
        // echo the request body back with a custom header and 200.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read until the header terminator, then keep reading until we
            // have the full Content-Length body (head + body can arrive in
            // separate packets).
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 1024];
            let mut content_len: Option<usize> = None;
            let body = loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break String::new();
                }
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(idx) = text.find("\r\n\r\n") {
                    if content_len.is_none() {
                        content_len = text[..idx].lines().find_map(|l| {
                            l.split_once(':').and_then(|(k, v)| {
                                k.trim()
                                    .eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                        });
                    }
                    let body_so_far = text.len() - (idx + 4);
                    if body_so_far >= content_len.unwrap_or(0) {
                        break text[idx + 4..].to_string();
                    }
                }
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Echo-Method: POST\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let req = HttpRequest {
            method: "post".to_string(), // case-insensitive
            url: format!("http://{addr}/echo"),
            headers: vec![("X-Test".to_string(), "1".to_string())],
            body: Some("hello-body".to_string()),
            timeout_ms: Some(5000),
            tag: "t".to_string(),
        };
        let resp = perform_http(&req).expect("transport ok");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "hello-body");
        assert!(
            resp.headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-echo-method") && v == "POST"),
            "server saw the POST + echoed header: {:?}",
            resp.headers
        );
        server.join().unwrap();
    }

    /// A connection to a closed loopback port is a transport `Err`, not a
    /// panic (surfaced to scripts as `error` with `status=0`).
    #[cfg(feature = "http-fetch")]
    #[test]
    fn perform_http_connection_refused_is_err() {
        // Bind then drop to obtain a port nothing is listening on.
        let addr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let req = HttpRequest {
            method: "GET".to_string(),
            url: format!("http://{addr}/"),
            headers: vec![],
            body: None,
            timeout_ms: Some(2000),
            tag: "t".to_string(),
        };
        assert!(perform_http(&req).is_err());
    }

    /// A response body larger than the buffer cap aborts with an `Err`
    /// (bounded read, no OOM) and, once folded into the structured reply,
    /// surfaces as `ok=false` with a non-empty `error` - never a panic and
    /// never an unbounded allocation.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn perform_http_body_over_cap_errors_not_oom() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();

        // One-shot server: reads (and discards) the request, replies 200
        // with a 4 KiB body - comfortably above the tiny test cap below.
        const BODY_LEN: usize = 4096;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch); // consume request head
            let body = "A".repeat(BODY_LEN);
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {BODY_LEN}\r\n\r\n{body}");
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        });

        let req = HttpRequest {
            method: "GET".to_string(),
            url: format!("http://{addr}/big"),
            headers: vec![],
            body: None,
            timeout_ms: Some(5000),
            tag: "t".to_string(),
        };

        // Cap well below the body size: the read must abort with an error.
        let result = perform_http_capped(&req, 64);
        assert!(
            result.is_err(),
            "body over cap must return Err (bounded read, no OOM), got Ok"
        );

        // Folded into the script-facing reply it is a clean structured
        // failure, not a panic.
        let value = http_response_to_value(&result);
        let ScriptValue::Map(m) = value else {
            panic!("expected a map reply");
        };
        assert_eq!(m.get("ok"), Some(&ScriptValue::Bool(false)));
        match m.get("error") {
            Some(ScriptValue::Str(e)) => assert!(!e.is_empty(), "error message populated"),
            other => panic!("expected non-empty error string, got {other:?}"),
        }

        let _ = server.join();
    }
}
