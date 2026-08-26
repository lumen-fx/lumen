//! Host-generic script runtime.
//!
//! Everything in the scripting contract that is not compilation, value
//! conversion, or invocation lives here, parameterized over
//! `H: `[`ScriptHost`]: the 18-event dispatch surface with per-id handler
//! routing, the derivation fixed-point driver, the store->mirror sync
//! policy driver, timers, HTTP fetch plumbing, the load-failure banner
//! protocol, and the [`ScriptPlugin`] that wires it all into the tick.
//!
//! Concrete hosts (`lumen-script-candela`, `lumen-script-rhai`,
//! `lumen-script-lua`) provide engine + builtins + conversion and hand a
//! built host to [`ScriptPlugin::new`].
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
//!
//! ## Several hosts in one app
//!
//! An app can run more than one host at a time (one per script language it
//! ships). Each host is its own `Resource`, so every `::<H>` system above is
//! monomorphised once per active host. Embedders must therefore order against
//! [`ScriptSet`] rather than against a concrete `system::<H>`: a `.before(
//! apply_derivations::<Rhai>)` edge says nothing about the Lua host's
//! derivation pass, and the dirty-window guarantees collapse for every host it
//! does not name. The sets below hold the same systems for every active host,
//! so one edge covers all of them.
//!
//! Hosts share the world's `PropertyStore`, so a signal written by one is read
//! by the others on the same tick. Lifecycle and event callbacks (`on_start`,
//! `on_ready`, `on_click`, `on_timer`, ...) are delivered to every active host;
//! a host that does not define the handler ignores the call.

use bevy_ecs::component::Mutable;
use bevy_ecs::message::{Message, MessageReader, MessageRegistry, MessageWriter, Messages};
use bevy_ecs::prelude::*;
use lumen_core::net_capture::{self, NetEvent};
use lumen_core::prelude::*;
use lumen_core::time::Instant;
use lumen_core::warn_line;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use crate::PluginEvent;
use crate::dnd;
use crate::http::{
    DisabledHttpClient, HttpClient, HttpDispatch, HttpDone, HttpRequest, HttpResponse,
    ThreadDispatch,
};
use crate::script_fn::ScriptFnRegistry;
use crate::{CallOutcome, ScriptCommand, ScriptError, ScriptHost, ScriptValue};

/// One [`ScriptCommand`] flowing through the ECS message bus so app
/// systems can read it via `MessageReader<ScriptCommandEvent>`.
#[derive(Message, Clone, Debug)]
pub struct ScriptCommandEvent(pub ScriptCommand);

/// Make sure the [`ScriptCommandEvent`] stream exists in `world`.
///
/// A script host registers it when it is installed, and an app without one
/// still runs the appliers: the C ABI, the SDKs and the browser runtime write
/// to the same stream, and a reader whose message type was never registered
/// fails parameter validation, which ends the tick. Registering the same
/// message twice would age its buffer twice a tick and drop a message a tick
/// early, so every registration goes through here.
pub fn register_script_commands(world: &mut World) {
    if !world.contains_resource::<Messages<ScriptCommandEvent>>() {
        MessageRegistry::register_message::<ScriptCommandEvent>(world);
    }
}

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
// Ordering anchors
// ---------------------------------------------------------------------

/// Ordering anchors for the host-generic script systems.
///
/// Every `::<H>` system [`ScriptPlugin`] registers joins one of these sets, so
/// an app running several hosts still has exactly one name to order against.
/// Order against the set, never against `system::<H>`: with more than one host
/// installed a concrete-system edge constrains that host alone and silently
/// leaves the others outside the one-tick dirty window.
///
/// The sets are pairwise disjoint, so `.after(one).before(another)` never
/// closes a cycle.
#[derive(bevy_ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScriptSet {
    /// [`sync_signals_into_host`]: store -> host mirror, per host.
    SyncSignals,
    /// A second [`sync_signals_into_host`] pass, late in the tick, installed
    /// only when the app runs more than one host.
    ///
    /// A host updates its own mirror as its builtins run, so with one host the
    /// early pass is enough. Across hosts it is not: a signal one host writes
    /// reaches the store only when the embedder's command applier runs, which
    /// is after the early pass, and the store's dirty flag is gone by the next
    /// tick. Without this second pass the other hosts would never observe the
    /// write at all. Ordered after the applier and before
    /// [`ScriptSet::Derivations`], so a derivation in one language recomputes
    /// on the same tick a dep written in another lands.
    SyncSignalsLate,
    /// [`tick_script`]: drain each host's command sink onto the message bus.
    Tick,
    /// [`apply_derivations`]: the per-host derived-signal fixed point.
    Derivations,
    /// Every event dispatcher that calls into a host
    /// ([`dispatch_clicks_and_doubles`], [`dispatch_close_to_script`], the
    /// toggle / slider / hotkey / menu / tray / DnD / text-input fanout).
    Dispatch,
    /// [`fire_due_timers`]: `on_timer` delivery, per host.
    Timers,
    /// [`fire_fetched_responses`]: `on_fetch` / `on_http` delivery, per host.
    Fetch,
    /// [`fire_plugin_events`]: delivery of the handler calls portable
    /// plugins pushed, per host.
    PluginEvents,
    /// [`fire_on_ready`]: the once-per-mount `on_ready` dispatch, per host.
    /// Registered by the embedder, not by [`ScriptPlugin`].
    Ready,
    /// [`fill_components`]: the use sites the build left for the script.
    /// Registered by the embedder, not by [`ScriptPlugin`].
    Fill,
    /// The embedder's DOM propagation of raw input
    /// ([`crate::dom_events::dispatch_pointer_and_key_events`]), per host.
    /// A `node.on("click", ...)` listener runs here, so this set produces
    /// script commands and every applier of them orders after it.
    DomInput,
    /// The embedder's DOM propagation of derived state
    /// ([`crate::dom_events::dispatch_state_events`]), per host. Kept
    /// apart from [`ScriptSet::DomInput`] because it runs at the far end
    /// of the tick, after the text edits that script commands produce;
    /// one set covering both could not be ordered against the appliers
    /// without a cycle.
    DomState,
}

/// Marker for the once-per-app half of [`ScriptPlugin::build`]: the shared
/// registries, the message registration, and the non-generic drain systems.
/// The second and later hosts skip that half, so a two-language app gets one
/// [`TimerRegistry`] and one `drain_timer_commands`, not two of each.
#[derive(Resource)]
struct ScriptSharedInstalled;

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
        // Bind the app's registered functions before the program compiles.
        // candela resolves every `host "..." { .. }` declaration while it
        // compiles the source, so a function registered after the load has
        // nothing left to bind to; Rhai and Lua would bind a name the compiled
        // program already failed to resolve. Sealing after the drain turns a
        // late registration into a warning instead of a silent miss.
        if app.world.contains_resource::<ScriptFnRegistry>() {
            let fns = app.world.resource::<ScriptFnRegistry>().for_lang(lang);
            for f in &fns {
                if let Err(e) = self.host.register_script_fn(f) {
                    warn_line!(
                        "{}: registering `{}` failed: {e}",
                        prefix(lang),
                        f.name.as_str()
                    );
                }
            }
            // A plugin that ships sugar over its namespace hands it over here
            // too, so the host can stage it ahead of the program.
            let preludes: Vec<(String, String)> = app
                .world
                .resource::<ScriptFnRegistry>()
                .preludes_for_lang(lang)
                .iter()
                .map(|p| (p.ns.clone(), p.source.clone()))
                .collect();
            for (ns, source) in &preludes {
                self.host.add_prelude(ns, source);
            }
            app.world.resource_mut::<ScriptFnRegistry>().seal();
        }
        if let Err(e) = self.host.load(&self.source, &self.uri) {
            // Unmissable, multi-line stderr banner - a load failure kills
            // every handler / signal / derivation while the window keeps
            // rendering, which historically read as "the app ignores
            // clicks" rather than "the script is dead".
            warn_line!(
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
            Err(e) => warn_line!("{}: on_start failed: {e}", prefix(lang)),
        }
        app.world.insert_resource(self.host);
        app.world.insert_resource(ScriptStartedAt(Instant::now()));
        // -- Once per app, however many hosts are installed ---------------
        if !app.world.contains_resource::<ScriptSharedInstalled>() {
            app.world.insert_resource(ScriptSharedInstalled);
            // Latch for the post-mount `on_ready` dispatch (see `fire_on_ready`).
            app.world.insert_resource(OnReadyFired::default());
            app.world.insert_resource(TimerRegistry::default());
            app.world.insert_resource(DueTimers::default());
            // The HTTP client is chosen at the composition point (the runtime
            // installs `lumen-http-ureq`; an embedder installs its own), so
            // only fall back to the disabled client when nothing put a
            // registry in first.
            if !app.world.contains_resource::<FetchRegistry>() {
                app.world.insert_resource(FetchRegistry::default());
            }
            app.world.insert_resource(PendingFetchReplies::default());
            // Portable plugins are dlopened native libraries; a browser page
            // has no way to load one, so the plugin-event pipeline does not
            // exist on wasm and its decode path stays out of the module a
            // site downloads.
            #[cfg(not(target_arch = "wasm32"))]
            {
                app.world.insert_resource(PendingPluginEvents::default());
                lumen_core::plugin_events::init_plugin_events();
            }
            register_script_commands(&mut app.world);
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
            // `lumen-input::InputPlugin` registers it in production;
            // self-register so a script host without the input layer still
            // brings up a valid schedule.
            app.world.init_resource::<bevy_ecs::message::Messages<
                lumen_core::text_events::TextEditApplied,
            >>();
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
            // Timer bookkeeping is host-neutral and runs once: it reschedules
            // repeating timers and drops one-shots BEFORE any host fires, so a
            // handler that cancels or re-arms the same name sees a clean slate,
            // and every host is offered the same due list.
            app.add_systems(
                TickStage::Systems,
                retire_due_timers
                    .after(ScriptSet::Tick)
                    .before(ScriptSet::Timers),
            );
            // HTTP replies land in a per-tick buffer rather than being taken
            // straight off the channel, so every host is offered each reply
            // instead of whichever host's system happened to run first.
            app.add_systems(
                TickStage::Systems,
                collect_fetch_replies
                    .after(drain_fetch_commands)
                    .before(ScriptSet::Fetch),
            );
            app.add_systems(
                TickStage::Systems,
                clear_fetch_replies.after(ScriptSet::Fetch),
            );
            // The plugin-event pipeline mirrors the fetch one above: one
            // host-neutral collect, one per-host delivery set, one clear.
            #[cfg(not(target_arch = "wasm32"))]
            {
                app.add_systems(
                    TickStage::Systems,
                    collect_plugin_events.before(ScriptSet::PluginEvents),
                );
                app.add_systems(
                    TickStage::Systems,
                    clear_plugin_events.after(ScriptSet::PluginEvents),
                );
            }
            app.add_systems(
                TickStage::Systems,
                drain_fetch_commands.after(ScriptSet::Tick),
            );
            // Must run after `fire_due_timers`: a repeating timer cancelled
            // from inside its own `on_timer` emits a `CancelTimer` during the
            // firing pass. Without this ordering the cancel could be drained a
            // tick late - after `retire_due_timers` had already re-armed the
            // timer and fired it one extra time. Draining right after the
            // firing pass applies the cancel on the same tick, before the next
            // re-fire.
            app.add_systems(
                TickStage::Systems,
                drain_timer_commands
                    .after(ScriptSet::Tick)
                    .after(ScriptSet::Timers),
            );
        }
        // -- Once per host -------------------------------------------------
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
                .in_set(ScriptSet::SyncSignals)
                .after(lumen_core::signals::push_toggle_to_signal)
                .after(lumen_core::signals::push_slider_to_signal)
                .after(lumen_core::signals::push_textinput_to_signal)
                .after(lumen_core::signals::push_scroll_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            tick_script::<H>
                .in_set(ScriptSet::Tick)
                .after(ScriptSet::SyncSignals),
        );
        app.add_systems(
            TickStage::Systems,
            apply_derivations::<H>
                .in_set(ScriptSet::Derivations)
                .after(ScriptSet::SyncSignals),
        );
        app.add_systems(
            TickStage::Systems,
            fire_due_timers::<H>.in_set(ScriptSet::Timers),
        );
        app.add_systems(
            TickStage::Systems,
            fire_fetched_responses::<H>.in_set(ScriptSet::Fetch),
        );
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            TickStage::Systems,
            fire_plugin_events::<H>.in_set(ScriptSet::PluginEvents),
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
                .in_set(ScriptSet::Dispatch)
                .after(lumen_input::dispatch_clicks)
                .after(lumen_primitives::press::detect_double_click),
        );
        app.add_systems(
            TickStage::Systems,
            (
                dispatch_long_press_to_script::<H>,
                dispatch_text_input_to_script::<H>,
                dispatch_file_drops_to_script::<H>,
                dnd::dispatch_drops_to_script::<H>,
                dnd::dispatch_drag_start_to_script::<H>,
                dispatch_file_picks_to_script::<H>,
                dispatch_hotkeys_to_script::<H>,
                // Ordered after the press so a chord pressed and released
                // inside one tick reaches the script in that order; both
                // take `ResMut<H>`, so without the edge the schedule is
                // free to run the release first.
                dispatch_hotkey_releases_to_script::<H>.after(dispatch_hotkeys_to_script::<H>),
                dispatch_notification_actions_to_script::<H>,
                dispatch_clipboard_reads_to_script::<H>,
                dispatch_menu_clicks_to_script::<H>,
                dispatch_dialog_closes_to_script::<H>,
                dispatch_tray_clicks_to_script::<H>,
                dispatch_toggle_to_script::<H>,
                dispatch_slider_to_script::<H>,
                dispatch_close_to_script::<H>,
            )
                .in_set(ScriptSet::Dispatch),
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

/// Fill every use site the build could not finish by calling the function it
/// names and putting the node that call returns in the marker's place.
///
/// A component element names a script function. Where instantiating the block
/// that function returns is the same as calling it, the build already put the
/// block in the tree and no marker was left. Where the function has to run,
/// because it works a value out or picks between blocks, the build left one
/// element carrying [`PendingFill`] and this is what runs it.
///
/// The call's own commands go out first, so the subtree it built exists by the
/// time the replacement names it; the reserved token the host minted resolves
/// inside the same batch. The marker is dropped on the way, so a filled site
/// fills once.
///
/// Every tick rather than once: a subtree spawned while the app runs can carry
/// a marker of its own.
///
/// Only the host that compiles markup blocks runs this, because that is the
/// only host a marker can name a function in. A function it does not declare
/// is a program that failed to load or an artifact that lost it: reported once
/// and the marker dropped, rather than retried against a program that will
/// never have it. Loud, because what it leaves behind is an empty element,
/// which is exactly what a reader would not notice.
pub fn fill_components<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    pending: Query<(Entity, &lumen_core::components::PendingFill)>,
    // Function names already complained about, so a tree that keeps spawning
    // the same unfillable use site says it once. One host fills, so this
    // needs no sharing.
    mut reported: Local<std::collections::HashSet<String>>,
    mut commands: Commands,
    mut events: MessageWriter<ScriptCommandEvent>,
) {
    if !H::FILLS_COMPONENTS {
        return;
    }
    for (entity, fill) in &pending {
        let args: Vec<ScriptValue> = fill
            .args
            .iter()
            .map(|value| ScriptValue::Str(value.clone()))
            .collect();
        let outcome = match host.call(&fill.function, &args) {
            Ok(outcome) => outcome,
            Err(e) => {
                warn_line!(
                    "{}: component {} failed: {e}",
                    prefix(host.lang()),
                    fill.function
                );
                commands
                    .entity(entity)
                    .remove::<lumen_core::components::PendingFill>();
                continue;
            }
        };
        if !outcome.found {
            if reported.insert(fill.function.clone()) {
                warn_line!(
                    "{}: component {} is named in the tree but the loaded program does not \
                     declare it; nothing was built there",
                    prefix(host.lang()),
                    fill.function
                );
            }
            commands
                .entity(entity)
                .remove::<lumen_core::components::PendingFill>();
            continue;
        }
        for c in outcome.commands {
            events.write(ScriptCommandEvent(c));
        }
        if let Some(new) = outcome.ret.as_ref().and_then(node_of) {
            events.write(ScriptCommandEvent(ScriptCommand::ReplaceWith {
                old: lumen_core::node::NodeHandle::new(entity).pack(),
                new,
            }));
        } else {
            warn_line!(
                "{}: component {} returned no node; a component returns one lmn! block",
                prefix(host.lang()),
                fill.function
            );
        }
        commands
            .entity(entity)
            .remove::<lumen_core::components::PendingFill>();
    }
}

/// The packed node a component's return value names, if it names one.
///
/// A host that cannot carry a 64-bit handle returns the interned id instead,
/// which is what the node side-table resolves; the result may be a reserved
/// spawn token, which the applier maps inside the batch it arrived in.
fn node_of(value: &ScriptValue) -> Option<u64> {
    match value {
        ScriptValue::I64(id) => lumen_core::node::resolve_node_raw(i32::try_from(*id).ok()?),
        _ => None,
    }
}

/// Latch guarding [`fire_on_ready`]: holds the [`ScriptHost::lang`] of every
/// host that has already dispatched `on_ready`, so each active host fires
/// once per mount. Hot reload clears the set after respawning the tree, so a
/// script that builds DOM in `on_ready` rebuilds it on the fresh mount.
#[derive(Resource, Default)]
pub struct OnReadyFired(pub std::collections::HashSet<&'static str>);

/// Dispatch the script's optional `on_ready()` once per mount, on the first
/// tick after the DOM index is published.
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
    if !fired.0.insert(host.lang()) {
        return;
    }
    match host.call("on_ready", &[]) {
        Ok(outcome) => {
            for c in outcome.commands {
                events.write(ScriptCommandEvent(c));
            }
        }
        Err(e) => warn_line!("{}: on_ready failed: {e}", prefix(host.lang())),
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
            warn_line!(
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
                    warn_line!("{}: derive '{name}' failed: {e}", prefix(host.lang()));
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

/// Names of the timers due this tick, in sorted order. Rewritten every tick by
/// [`retire_due_timers`] and read by [`fire_due_timers`] on each active host.
#[derive(Resource, Default)]
pub struct DueTimers(pub Vec<String>);

/// Collect the timers whose deadline has passed, reschedule the repeating ones,
/// and drop the one-shots. Host-neutral and registered once, so each active
/// host is offered the same due list instead of the first one to run taking it.
///
/// Reschedule / remove happens BEFORE any host fires, so a handler that calls
/// `cancel_timer` or `set_interval` on the same name sees a clean slate. Due
/// timers are sorted by name for determinism.
pub fn retire_due_timers(mut timers: ResMut<TimerRegistry>, mut due_out: ResMut<DueTimers>) {
    let now = Instant::now();
    let mut due: Vec<String> = timers
        .timers
        .iter()
        .filter(|(_, t)| t.fire_at <= now)
        .map(|(name, _)| name.clone())
        .collect();
    due.sort();
    for name in &due {
        let next = timers
            .timers
            .get(name)
            .and_then(|t| t.repeat_every.map(|d| now + d));
        match next {
            Some(fire_at) => {
                if let Some(t) = timers.timers.get_mut(name) {
                    t.fire_at = fire_at;
                }
            }
            None => {
                timers.timers.remove(name);
            }
        }
    }
    due_out.0 = due;
}

/// Fire `on_timer(name)` for every timer [`retire_due_timers`] collected this
/// tick. A host that defines no matching handler ignores the call, so in an app
/// running several languages the timer reaches whichever host declared it.
pub fn fire_due_timers<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    due: Res<DueTimers>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for name in &due.0 {
        if let Err(e) = route_event(&mut *host, "timer", "on_timer", name, &mut out) {
            warn_line!("{}: on_timer({name}) failed: {e}", prefix(host.lang()));
        }
    }
}

// ---------------------------------------------------------------------
// HTTP fetch
// ---------------------------------------------------------------------

/// HTTP plumbing: start requests away from the world thread, marshal the
/// completed reply back onto the ECS/UI thread, and surface it to the script.
///
/// Both `fetch(url, tag)` (simple sugar) and `http(#{...})` (general form)
/// flow through this single registry, dispatcher, and completion channel -
/// there is exactly one async delivery mechanism. The worker only ever *sends*
/// the outcome down the channel; [`collect_fetch_replies`], running on the
/// world thread, moves it into [`PendingFetchReplies`], and
/// [`fire_fetched_responses`] is the only place a signal / handler is touched.
/// That worker->UI-thread hand-off mirrors Slint's `invoke_from_event_loop`
/// marshalling.
///
/// The registry holds the [`HttpDispatch`] every request runs on, which in turn
/// holds the [`HttpClient`]. A default registry dispatches
/// [`DisabledHttpClient`] onto a worker thread, so a build with no client
/// installed answers each request with the "no HTTP client" error instead of
/// hanging or silently dropping it. The runtime installs the client Lumen ships
/// (`lumen-http-ureq`) with [`FetchRegistry::with_client`] before the script
/// plugin builds; an embedder swaps in its own the same way, and a platform
/// with no thread to block swaps the dispatcher itself with
/// [`FetchRegistry::with_dispatch`].
#[derive(Resource)]
pub struct FetchRegistry {
    sender: crossbeam_channel::Sender<HttpOutcome>,
    receiver: crossbeam_channel::Receiver<HttpOutcome>,
    dispatch: Arc<dyn HttpDispatch>,
}

impl Default for FetchRegistry {
    fn default() -> Self {
        Self::with_client(Arc::new(DisabledHttpClient))
    }
}

impl FetchRegistry {
    /// Build a registry whose requests run on `client`, one worker thread per
    /// request.
    ///
    /// Insert it before the script plugin builds; the plugin only installs its
    /// own default when no registry is present.
    pub fn with_client(client: Arc<dyn HttpClient>) -> Self {
        Self::with_dispatch(Arc::new(ThreadDispatch::new(client)))
    }

    /// Build a registry whose requests run on `dispatch`, for a platform where
    /// a blocked worker thread is not how a request completes.
    pub fn with_dispatch(dispatch: Arc<dyn HttpDispatch>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            sender: tx,
            receiver: rx,
            dispatch,
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

struct HttpOutcome {
    tag: String,
    style: DeliveryStyle,
    /// `Ok` = the request completed and a reply (any status) came back.
    /// `Err` = transport failure (DNS, connect, timeout, bad method/url).
    result: Result<HttpResponse, String>,
}

/// Read `Fetch` / `Http` commands the script emitted this tick and hand each
/// to the registry's dispatcher. Other variants are no-ops here (they
/// flow to apply_script_commands and timer drains separately).
pub fn drain_fetch_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    fetcher: Res<FetchRegistry>,
) {
    for ev in events.read() {
        let (req, tag, style) = match &ev.0 {
            ScriptCommand::Fetch { url, tag } => (
                HttpRequest {
                    method: "GET".to_string(),
                    url: url.clone(),
                    headers: Vec::new(),
                    body: None,
                    timeout_ms: None,
                },
                tag.clone(),
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
                },
                tag.clone(),
                DeliveryStyle::Http,
            ),
            _ => continue,
        };
        // Dev-tooling capture (no-op unless the devtools sink is installed):
        // report the dispatch so the Network tab shows the in-flight request
        // before its reply lands.
        net_capture::record(NetEvent::Started {
            tag: tag.clone(),
            method: req.method.clone(),
            url: req.url.clone(),
        });
        let tx = fetcher.sender.clone();
        let label = tag.clone();
        let done: HttpDone = Box::new(move |result| {
            let _ = tx.send(HttpOutcome { tag, style, result });
        });
        fetcher
            .dispatch
            .dispatch(&label, req, MAX_HTTP_BODY_BYTES, done);
    }
}

/// Hard cap on the response body a client may buffer into memory, in bytes
/// (16 MiB).
///
/// A huge or open-ended (chunked / streaming) endpoint must not be able to
/// OOM the per-request worker: reads past this bound abort with an error
/// that is surfaced to the script as the reply's `error` field. Callers who
/// legitimately need larger payloads should stream, not `fetch`. The cap is
/// policy, so it lives here and is passed to every
/// [`HttpClient::send`](crate::http::HttpClient::send) call rather than being
/// each client's business.
const MAX_HTTP_BODY_BYTES: u64 = 16 * 1024 * 1024;

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

/// HTTP replies that finished this tick, waiting to be delivered to every
/// active host. Filled by [`collect_fetch_replies`] and emptied by
/// [`clear_fetch_replies`].
#[derive(Resource, Default)]
pub struct PendingFetchReplies(Vec<HttpOutcome>);

/// Move finished HTTP replies (marshalled back from the worker threads) off the
/// channel into [`PendingFetchReplies`], and record each for the devtools
/// network pane. Host-neutral and registered once, so a reply is offered to
/// every active host rather than taken by whichever ran first.
pub fn collect_fetch_replies(
    fetcher: Res<FetchRegistry>,
    mut pending: ResMut<PendingFetchReplies>,
) {
    while let Ok(outcome) = fetcher.receiver.try_recv() {
        // Dev-tooling capture (no-op unless the devtools sink is installed):
        // pair the reply with its in-flight request by `tag`.
        let (ok, status, error) = match &outcome.result {
            Ok(resp) => (true, resp.status, String::new()),
            Err(err) => (false, 0u16, err.clone()),
        };
        net_capture::record(NetEvent::Completed {
            tag: outcome.tag.clone(),
            ok,
            status,
            error,
        });
        pending.0.push(outcome);
    }
}

/// Drop this tick's delivered HTTP replies. Runs after every host's
/// [`fire_fetched_responses`].
pub fn clear_fetch_replies(mut pending: ResMut<PendingFetchReplies>) {
    pending.0.clear();
}

/// Invoke the script's completion handler once for each reply
/// [`collect_fetch_replies`] gathered. This is the only place a network reply
/// crosses into script/signal land - the worker never touches the world,
/// mirroring Slint's `invoke_from_event_loop`.
///
/// `fetch()` replies preserve the historical contract: `on_fetch(tag,
/// body)` on 2xx, `on_fetch_error(tag, msg)` on transport failure or
/// non-2xx. `http()` replies fire `on_http(tag, response)` with the
/// structured map for every completed request.
pub fn fire_fetched_responses<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    pending: Res<PendingFetchReplies>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for outcome in &pending.0 {
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
                    warn_line!(
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
                    warn_line!(
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
// Plugin events
// ---------------------------------------------------------------------

/// Handler calls that portable plugins pushed and this tick delivers,
/// mirroring [`PendingFetchReplies`]: filled by [`collect_plugin_events`],
/// offered to every active host by [`fire_plugin_events`], and emptied by
/// [`clear_plugin_events`].
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Default)]
pub struct PendingPluginEvents(Vec<PluginEvent>);

/// Move the events portable plugins pushed off the cross-thread bus
/// ([`lumen_core::plugin_events`]) into [`PendingPluginEvents`]. Host-neutral
/// and registered once, so a handler call is offered to every active host
/// rather than taken by whichever ran first; a [`PluginEvent::Commands`]
/// batch goes straight onto the command bus here instead, so it applies once
/// however many hosts run.
#[cfg(not(target_arch = "wasm32"))]
pub fn collect_plugin_events(
    mut pending: ResMut<PendingPluginEvents>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for bytes in lumen_core::plugin_events::drain_plugin_events() {
        match lumen_plugin_abi::codec::decode::<PluginEvent>(&bytes) {
            Ok(PluginEvent::Commands(commands)) => {
                for command in commands {
                    out.write(ScriptCommandEvent(command));
                }
            }
            Ok(event) => pending.0.push(event),
            Err(e) => warn_line!("lumen-script: a plugin event did not decode: {e}"),
        }
    }
}

/// Drop this tick's delivered plugin events. Runs after every host's
/// [`fire_plugin_events`].
#[cfg(not(target_arch = "wasm32"))]
pub fn clear_plugin_events(mut pending: ResMut<PendingPluginEvents>) {
    pending.0.clear();
}

/// Invoke the script's handler once for each call [`collect_plugin_events`]
/// gathered. Routing matches the fetch pipeline exactly: a per-key
/// `on(event, key, fn)` registration wins, else the event's fallback, and the
/// key rides as the handler's first argument ahead of the event's own.
#[cfg(not(target_arch = "wasm32"))]
pub fn fire_plugin_events<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    pending: Res<PendingPluginEvents>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for event in &pending.0 {
        let PluginEvent::Call {
            event,
            key,
            fallback,
            args,
        } = event
        else {
            // Commands never reach the buffer; `collect_plugin_events` put
            // them on the bus already.
            continue;
        };
        let target = resolve_handler(&*host, event, key, fallback);
        if target.is_empty() {
            continue;
        }
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(ScriptValue::Str(key.clone()));
        call_args.extend(args.iter().cloned());
        match host.call(&target, &call_args) {
            Ok(outcome) => forward_outcome(outcome, &mut out),
            Err(e) => warn_line!("{}: {target}({key}) failed: {e}", prefix(host.lang())),
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
            warn_line!("{}: on_click failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_double_click failed: {e}", prefix(host.lang()));
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
            Err(e) => warn_line!("{}: on_close failed: {e}", prefix(host.lang())),
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
            warn_line!("{}: on_toggle failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_slider failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_long_press failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_file_dropped failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_hotkey failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::HotkeyReleased`] to the script as
/// `on_hotkey_release(name)`, with the per-id
/// `on("hotkey_release", name, fn)` router applying first. Paired with
/// [`dispatch_hotkeys_to_script`] so one chord drives push-to-talk.
pub fn dispatch_hotkey_releases_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::HotkeyReleased>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event(
            &mut *host,
            "hotkey_release",
            "on_hotkey_release",
            &ev.name,
            &mut out,
        ) {
            warn_line!("{}: on_hotkey_release failed: {e}", prefix(host.lang()));
        }
    }
}

/// Forward [`lumen_core::input::NotificationActionInvoked`] to the
/// script as `on_notification_action(id, action_id)`, with the per-id
/// `on("notification_action", id, fn)` router applying first.
pub fn dispatch_notification_actions_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::NotificationActionInvoked>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event_two_args(
            &mut *host,
            "notification_action",
            "on_notification_action",
            &ev.id,
            &ev.action_id,
            &mut out,
        ) {
            warn_line!(
                "{}: on_notification_action failed: {e}",
                prefix(host.lang())
            );
        }
    }
}

/// Forward [`lumen_core::input::ClipboardRead`] results to the script as
/// `on_clipboard(tag, text)`, with the per-tag
/// `on("clipboard", tag, fn)` router applying first. A clipboard holding
/// no text still fires once, with an empty string, so a script can clear
/// a pending state.
pub fn dispatch_clipboard_reads_to_script<H: ScriptHost + Resource<Mutability = Mutable>>(
    mut host: ResMut<H>,
    mut events: MessageReader<lumen_core::input::ClipboardRead>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    for ev in events.read() {
        if let Err(e) = route_event_two_args(
            &mut *host,
            "clipboard",
            "on_clipboard",
            &ev.tag,
            &ev.text,
            &mut out,
        ) {
            warn_line!("{}: on_clipboard failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_menu failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: {fallback} failed: {e}", prefix(host.lang()));
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
            warn_line!("{}: on_tray failed: {e}", prefix(host.lang()));
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
                warn_line!("{}: unknown FilePicked kind '{other}'", prefix(host.lang()));
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
            warn_line!("{}: {handler} failed: {e}", prefix(host.lang()));
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
                warn_line!("{}: on_text_input failed: {err}", prefix(host.lang()));
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

    /// The registry is the seam: whatever [`HttpClient`] it holds is the one
    /// a queued `fetch()` runs on, and its reply reaches
    /// [`PendingFetchReplies`] unchanged. Drives the real systems with a
    /// recording client, so no socket is opened.
    #[test]
    fn requests_run_on_the_installed_client() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct RecordingClient {
            seen: Mutex<Vec<HttpRequest>>,
        }

        impl HttpClient for RecordingClient {
            fn send(&self, request: &HttpRequest, body_limit: u64) -> Result<HttpResponse, String> {
                self.seen.lock().unwrap().push(request.clone());
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("X-Cap".to_string(), body_limit.to_string())],
                    body: format!("{} {}", request.method, request.url),
                })
            }
        }

        let client = Arc::new(RecordingClient::default());
        let mut world = World::new();
        world.insert_resource(FetchRegistry::with_client(client.clone()));
        world.insert_resource(PendingFetchReplies::default());
        MessageRegistry::register_message::<ScriptCommandEvent>(&mut world);
        world.write_message(ScriptCommandEvent(ScriptCommand::Fetch {
            url: "http://example.invalid/thing".to_string(),
            tag: "t".to_string(),
        }));

        let mut schedule = Schedule::default();
        schedule.add_systems(drain_fetch_commands);
        schedule.run(&mut world);

        // The worker thread is the only asynchrony here, so block on the
        // registry channel until its reply is queued rather than spinning a
        // bounded number of times: on a loaded machine a spin runs out before
        // the worker is scheduled, and the asserts below then read a world
        // nothing has landed in yet. `Select::ready` waits for the message
        // without taking it, leaving it for the collector. It also orders the
        // client's own bookkeeping, since the worker sends only after
        // `HttpClient::send` returns.
        {
            let registry = world.resource::<FetchRegistry>();
            let mut ready = crossbeam_channel::Select::new();
            ready.recv(&registry.receiver);
            ready.ready();
        }

        let mut collect = Schedule::default();
        collect.add_systems(collect_fetch_replies);
        collect.run(&mut world);

        let seen = client.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "exactly one request reached the client");
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].url, "http://example.invalid/thing");

        let pending = world.resource::<PendingFetchReplies>();
        assert_eq!(pending.0.len(), 1, "the reply reached the world thread");
        let reply = pending.0[0].result.as_ref().expect("client returned Ok");
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, "GET http://example.invalid/thing");
        // The body cap is policy the runtime passes down, not a client default.
        assert_eq!(
            reply.headers[0].1,
            MAX_HTTP_BODY_BYTES.to_string(),
            "the client was handed the runtime's body cap"
        );
    }

    /// A build with no client installed answers every request with the
    /// rebuild hint, so `fetch()` never silently does nothing.
    #[test]
    fn default_registry_reports_the_missing_client() {
        let err = DisabledHttpClient
            .send(&HttpRequest::default(), MAX_HTTP_BODY_BYTES)
            .expect_err("the disabled client always errors");
        assert!(err.contains("http-fetch"), "error names the feature: {err}");
    }
}
