//! The plugin that puts downloads into an app: the `download` script
//! namespace, and the three events a transfer reports through.
//!
//! The engine has no download surface of its own; everything an app observes
//! comes from here, through the generic seams every plugin uses:
//!
//! - the two script functions register through the app's `ScriptFnRegistry`,
//!   so every host (Rhai, Lua, candela) binds them before the program loads;
//! - progress, completion, and failure are [`PluginEvent`]s on the
//!   plugin-event bus, keyed by the tag the call named, so a per-tag
//!   `on("download_done", tag, fn)` registration wins over the
//!   `on_download_done(tag, path)` fallback, the routing every plugin event
//!   gets;
//! - the transfer runs on the installed [`SpawnService`]'s blocking pool, or
//!   on a short-lived thread when the app carries no async backend, so a
//!   download of any size never touches the frame.
//!
//! A script-function body runs inside a host call with no world access, so it
//! talks to the systems through the module's own queue: the body validates the
//! call, claims the tag, pushes a job, and wakes the event loop, and
//! [`tick_downloads`] starts what the queue holds on the next tick.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, EventLoopWaker, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::prelude::{IntoScheduleConfigs, NonSendMut, Res, TickStage};
use lumen_module::lumen_core::task::{Spawn, SpawnService};
use lumen_module::lumen_core::warn_line;
use lumen_module::lumen_script::{
    PluginEvent, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptSet, ScriptTy as T, ScriptValue,
    push_plugin_event,
};

use crate::transfer::{self, CANCELLED, Checksum, Control, Limits};

/// The namespace the function lives in: `download::to_file(..)` in Rhai and
/// candela, `download.to_file(..)` in Lua.
const NAMESPACE: &str = "download";

/// How many transfers run at once when the app configures nothing.
pub const DEFAULT_MAX_CONCURRENT: i64 = 4;

/// The most transfers an app can ask to run at once. Past this the connections
/// compete for the same link rather than finishing sooner.
pub const MAX_MAX_CONCURRENT: i64 = 64;

/// The shortest wall time between two progress events for one tag. A transfer
/// off a fast link delivers thousands of chunks a second, and a handler that
/// updated a signal for each of them would be the download's bottleneck.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// One accepted transfer, waiting for the tick that starts it.
struct Job {
    url: String,
    dest: PathBuf,
    tag: String,
    checksum: Checksum,
    control: Control,
}

/// The handles a script-function body captures: the job queue, the waker that
/// gets a parked event loop to run the tick that drains it, and the tags with
/// a transfer in flight, each with the switch that cancels it.
#[derive(Clone, Default)]
struct Shared {
    queue: Arc<Mutex<Vec<Job>>>,
    waker: Arc<Mutex<Option<EventLoopWaker>>>,
    live: Arc<Mutex<HashMap<String, Control>>>,
}

impl Shared {
    /// Queue a job and wake the loop so the next tick starts it.
    fn push(&self, job: Job) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(job);
        }
        if let Ok(guard) = self.waker.lock()
            && let Some(w) = guard.as_ref()
        {
            w.wake();
        }
    }

    /// Claim `tag` for a transfer, or say why it cannot be claimed.
    ///
    /// One meaning per tag: a tag already downloading is not superseded,
    /// because the events both transfers would fire carry the same key and
    /// nothing downstream could tell them apart.
    fn claim(&self, tag: &str, max_concurrent: usize) -> Result<Control, String> {
        let Ok(mut live) = self.live.lock() else {
            return Err("the module's job table is poisoned".to_string());
        };
        if live.contains_key(tag) {
            return Err(format!(
                "a download tagged `{tag}` is already running; wait for it or use another tag"
            ));
        }
        if live.len() >= max_concurrent {
            return Err(format!(
                "{max_concurrent} downloads are already running, which is the limit; raise \
                 `max_concurrent` in the module's config or wait for one to finish"
            ));
        }
        let control = Control::default();
        live.insert(tag.to_string(), control.clone());
        Ok(control)
    }

    /// Release a tag once its transfer has ended. Only the transfer that
    /// holds the tag releases it: a cancelled one may end after the tag has
    /// been claimed again.
    fn release(&self, tag: &str, control: &Control) {
        if let Ok(mut live) = self.live.lock()
            && live.get(tag).is_some_and(|held| held.same(control))
        {
            live.remove(tag);
        }
    }

    /// Cancel the transfer running under `tag`, answering whether there was
    /// one to cancel.
    ///
    /// The tag and its concurrency slot are free once this returns, and the
    /// cancel is reported as the tag's `download_error`, the last event the
    /// transfer sends. A transfer that has already finished is not cancelled:
    /// its own outcome is on its way, and this answers false.
    fn cancel(&self, tag: &str) -> bool {
        let tag = tag.trim();
        let Ok(mut live) = self.live.lock() else {
            return false;
        };
        let Some(control) = live.get(tag) else {
            return false;
        };
        if !control.cancel() {
            return false;
        }
        live.remove(tag);
        drop(live);
        error(tag, CANCELLED.to_string());
        true
    }
}

/// The module's world state: what the tick system needs to start a job.
///
/// `NonSend` rather than a resource, because the `Resource` derive resolves
/// `bevy_ecs` by name and a module crate depends on `lumen-module` alone. The
/// state is only ever touched by the tick system, which runs on the main
/// thread; everything a transfer thread reaches for lives behind [`Shared`].
struct DownloadState {
    shared: Shared,
    limits: Limits,
    /// Whether the tick system has wired the event-loop waker yet (the
    /// resource appears only once a windowing backend runs).
    waker_wired: bool,
}

/// Downloads for a Lumen app: install it and `download::to_file` and
/// `download::cancel` exist.
///
/// Ships as the bundled `lumen-download` runtime module (an app declares
/// `lumen-download = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin in a static build. Without it the function
/// do not exist and a script call fails with the host's ordinary
/// unknown-function error.
pub struct DownloadPlugin {
    limits: Limits,
    max_concurrent: usize,
}

impl DownloadPlugin {
    /// Build from the module's `config` table.
    ///
    /// `timeout_ms` bounds how long a quiet server is waited on, before it
    /// answers and between the pieces of a body, `max_bytes` caps a body, and
    /// `max_concurrent` caps how many transfers run at once. A key that is absent, negative, or of another type leaves
    /// the default in place.
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        let positive = |key: &str| config.int(key).and_then(|v| u64::try_from(v).ok());
        Self {
            limits: Limits {
                timeout_ms: positive("timeout_ms").filter(|v| *v > 0),
                max_bytes: positive("max_bytes").filter(|v| *v > 0),
            },
            max_concurrent: max_concurrent_of(
                config
                    .int("max_concurrent")
                    .unwrap_or(DEFAULT_MAX_CONCURRENT),
            ),
        }
    }

    /// Build with explicit bounds. This is what a static build sets when it
    /// installs the plugin itself and has no `config` table to read.
    #[must_use]
    pub fn with_limits(limits: Limits, max_concurrent: i64) -> Self {
        Self {
            limits,
            max_concurrent: max_concurrent_of(max_concurrent),
        }
    }
}

impl Default for DownloadPlugin {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            max_concurrent: max_concurrent_of(DEFAULT_MAX_CONCURRENT),
        }
    }
}

/// A configured concurrency, clamped into the range the module supports.
fn max_concurrent_of(asked: i64) -> usize {
    usize::try_from(asked.clamp(1, MAX_MAX_CONCURRENT)).unwrap_or(1)
}

impl Plugin for DownloadPlugin {
    fn build(self, app: &mut App) {
        let shared = Shared::default();
        app.add_script_fns(script_fns(&shared, self.max_concurrent));
        app.world.insert_non_send(DownloadState {
            shared,
            limits: self.limits,
            waker_wired: false,
        });
        // Ahead of the hosts' signal mirror sync, so a handler that a started
        // transfer's event reaches writes signals the same tick a `derive()`
        // over them recomputes: the ordering every module system takes.
        app.add_systems(
            TickStage::Systems,
            tick_downloads.before(ScriptSet::SyncSignals),
        );
    }
}

/// The `download` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
///
/// Every parameter is required, checksum included; an empty checksum is how a
/// call says it wants none. An optional trailing parameter would leave candela
/// nothing it can declare, and the whole namespace would degrade to untyped
/// variadic calls there.
fn script_fns(shared: &Shared, max_concurrent: usize) -> Vec<ScriptFn> {
    let cancelling = shared.clone();
    let shared = shared.clone();
    vec![
        ScriptFn::new("to_file")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "Download `url` to `path`, reporting under `tag`. `checksum` is \
                 `sha256:<64 hex digits>` or `sha1:<40 hex digits>`, or empty for no check. \
                 True when the transfer started.",
            )
            .param("url", T::Str)
            .param("path", T::Str)
            .param("tag", T::Str)
            .param("checksum", T::Str)
            .ret(T::Bool)
            .build(move |cx| {
                let url = cx.str_arg(0);
                let path = cx.str_arg(1);
                let tag = cx.str_arg(2);
                let checksum = cx.str_arg(3);
                Ok(ScriptValue::Bool(accept(
                    &shared,
                    max_concurrent,
                    url,
                    path,
                    tag,
                    &checksum,
                )))
            }),
        ScriptFn::new("cancel")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "Cancel the download running under a tag; it reports `download_error` with the \
                 message `cancelled`, and the tag is free again. False when nothing is running \
                 under that tag.",
            )
            .param("tag", T::Str)
            .ret(T::Bool)
            .build(move |cx| Ok(ScriptValue::Bool(cancelling.cancel(&cx.str_arg(0))))),
    ]
}

/// Take one `to_file` call as far as a queued job, or refuse it.
///
/// Where the refusal goes depends on whether there is anywhere to send it. A
/// call with no tag has no event key, so it degrades to one stderr line the
/// way every unroutable refusal does; anything else is that tag's business and
/// goes out as `download_error`. Either way the call answers false, and true
/// means a transfer is now running.
fn accept(
    shared: &Shared,
    max_concurrent: usize,
    url: String,
    path: String,
    tag: String,
    checksum: &str,
) -> bool {
    let tag = tag.trim().to_string();
    if tag.is_empty() {
        warn_line!("lumen-download: to_file needs a tag to report under; nothing was downloaded");
        return false;
    }
    if url.trim().is_empty() {
        error(&tag, "to_file was given no url".to_string());
        return false;
    }
    if path.trim().is_empty() {
        error(&tag, "to_file was given no destination path".to_string());
        return false;
    }
    let checksum = match transfer::parse_checksum(checksum) {
        Ok(c) => c,
        Err(message) => {
            error(&tag, message);
            return false;
        }
    };
    let control = match shared.claim(&tag, max_concurrent) {
        Ok(control) => control,
        Err(message) => {
            error(&tag, message);
            return false;
        }
    };
    shared.push(Job {
        url,
        dest: app_paths::resolve(path),
        tag,
        checksum,
        control,
    });
    true
}

/// Per-tick pump: wire the waker once, then start whatever the script queued.
fn tick_downloads(
    mut state: NonSendMut<DownloadState>,
    waker: Option<Res<EventLoopWaker>>,
    spawn: Option<Res<SpawnService>>,
) {
    let state = &mut *state;
    // Wire the loop waker lazily: the resource appears once a windowing
    // backend runs, after plugin build.
    if !state.waker_wired
        && let Some(w) = waker.as_deref()
    {
        if let Ok(mut slot) = state.shared.waker.lock() {
            *slot = Some(w.clone());
        }
        state.waker_wired = true;
    }

    let queued = match state.shared.queue.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    };
    for job in queued {
        spawn_transfer(
            spawn.as_ref().map(|s| s.handle()),
            job,
            state.limits,
            state.shared.clone(),
        );
    }
}

/// Run one transfer off the tick loop and report it as it goes.
///
/// The work runs on the engine's spawn seam, the blocking pool of whatever
/// [`SpawnService`] the app installed, like every other module that leaves
/// the tick loop. An app with no async backend gets a short-lived plain thread
/// instead; the transfer must still leave the loop, because a download is
/// unbounded in time by definition.
fn spawn_transfer(spawn: Option<Arc<dyn Spawn>>, job: Job, limits: Limits, shared: Shared) {
    // Cancelled while it waited for this tick: the cancel has already been
    // reported and the tag freed, so there is nothing to start.
    if job.control.is_cancelled() {
        return;
    }
    // Kept outside the closure so a job that never starts still frees its tag
    // and says so; the closure owns everything else.
    let unstarted = (job.tag.clone(), job.control.clone(), shared.clone());
    let run = move || {
        let Job {
            url,
            dest,
            tag,
            checksum,
            control,
        } = job;
        // Throttled: a fast link delivers chunks far faster than a UI can
        // read them, and every event costs a handler call on the tick thread.
        let mut last = Instant::now();
        let mut progress = |received, total: Option<u64>| {
            if last.elapsed() >= PROGRESS_INTERVAL && !control.is_cancelled() {
                last = Instant::now();
                report_progress(&tag, received, total);
            }
        };
        let outcome = transfer::to_file(&url, &dest, &checksum, &limits, &control, &mut progress);
        // A cancel that got in first has reported and freed the tag already;
        // this transfer says nothing more. Otherwise the tag is freed before
        // the outcome goes out, so a handler can start the next transfer
        // under it.
        if !control.finish() {
            return;
        }
        shared.release(&tag, &control);
        match outcome {
            Ok(done) => {
                // One last figure, unthrottled, so a progress bar reaches its
                // end before the done handler runs.
                report_progress(&tag, done.received, done.total);
                push_plugin_event(PluginEvent::Call {
                    event: "download_done".to_string(),
                    key: tag.clone(),
                    fallback: "on_download_done".to_string(),
                    args: vec![ScriptValue::Str(done.path.display().to_string())],
                });
            }
            Err(message) => error(&tag, message),
        }
    };
    match spawn {
        Some(spawn) => spawn.spawn_blocking(Box::new(run)),
        None => {
            let spawned = std::thread::Builder::new()
                .name("lumen-download".into())
                .spawn(run);
            if let Err(e) = spawned {
                let (tag, control, shared) = unstarted;
                if control.finish() {
                    shared.release(&tag, &control);
                    error(&tag, format!("no thread to download on: {e}"));
                }
            }
        }
    }
}

/// Report how far one transfer has got. `total` is -1 when the server declared
/// no size, which is what a chunked or connection-delimited body looks like.
fn report_progress(tag: &str, received: u64, total: Option<u64>) {
    push_plugin_event(PluginEvent::Call {
        event: "download_progress".to_string(),
        key: tag.to_string(),
        fallback: "on_download_progress".to_string(),
        args: vec![
            ScriptValue::I64(i64::try_from(received).unwrap_or(i64::MAX)),
            ScriptValue::I64(total.and_then(|t| i64::try_from(t).ok()).unwrap_or(-1)),
        ],
    });
}

/// Report that one tag's download will not happen.
fn error(tag: &str, message: String) {
    push_plugin_event(PluginEvent::Call {
        event: "download_error".to_string(),
        key: tag.to_string(),
        fallback: "on_download_error".to_string(),
        args: vec![ScriptValue::Str(message)],
    });
}
