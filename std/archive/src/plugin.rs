//! The plugin that puts archive extraction into an app: the `archive` script
//! namespace, and the two events a finished job arrives on.
//!
//! The engine has no archive surface of its own; everything an app observes
//! comes from here, through the generic seams a plugin uses:
//!
//! - the one script function registers on the app's `ScriptFnRegistry`, so
//!   every host (Rhai, Lua, candela) binds it before the program loads;
//! - the unpacking runs on the installed [`SpawnService`]'s blocking pool, or
//!   a short-lived thread when the app carries no async backend, because an
//!   archive of any size would otherwise stall the frame;
//! - the outcome is a [`PluginEvent`] on the plugin-event bus: the script's
//!   `on_archive_done(tag, dest, count)` handler fires, and a per-tag
//!   `on("archive_done", tag, fn)` registration wins over it, the same
//!   routing every plugin event gets.
//!
//! A script-function body runs inside a host call with no world access, so
//! `archive::extract` decides whether to take the job and queues it; the
//! per-tick [`tick_archive`] drains the queue and starts the work. Deciding
//! in the body is what lets the call answer straight away: a job the module
//! took answers true, and one it refused answers false and reports why on
//! `archive_error`, so a script can branch on the value and still handle the
//! failure in one place.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::{Res, Resource};
use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::prelude::{IntoScheduleConfigs, TickStage};
use lumen_module::lumen_core::task::{Spawn, SpawnService};
use lumen_module::lumen_script::{
    PluginEvent, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptSet, ScriptStruct, ScriptTy as T,
    ScriptValue, push_plugin_event,
};

use crate::unpack;

/// The namespace the functions live in: `archive::extract(..)` in Rhai and
/// candela, `archive.extract(..)` in Lua.
const NAMESPACE: &str = "archive";

/// The event a finished extraction arrives on.
const DONE_EVENT: &str = "archive_done";

/// The handler a script defines when it does not register per tag.
const DONE_FALLBACK: &str = "on_archive_done";

/// The event a refused or failed extraction arrives on.
const ERROR_EVENT: &str = "archive_error";

/// The handler a script defines when it does not register per tag.
const ERROR_FALLBACK: &str = "on_archive_error";

/// How many extractions run at once by default.
pub const DEFAULT_MAX_CONCURRENT: i64 = 4;

/// The most an app can ask to run at once. Each job holds a thread for as
/// long as it reads, so the ceiling is what keeps one script from taking the
/// whole blocking pool.
pub const MAX_MAX_CONCURRENT: i64 = 64;

/// One accepted job, waiting for the tick that starts it.
struct Job {
    src: PathBuf,
    dest: PathBuf,
    tag: String,
    filter: Option<unpack::Filter>,
}

/// What the script-function body and the tick system share: the queue of
/// accepted jobs and the tags currently in flight.
#[derive(Clone)]
struct Shared {
    queue: Arc<Mutex<Vec<Job>>>,
    live: Arc<Mutex<HashSet<String>>>,
    max_concurrent: usize,
}

impl Shared {
    fn new(max_concurrent: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(Vec::new())),
            live: Arc::new(Mutex::new(HashSet::new())),
            max_concurrent,
        }
    }

    /// Take the job if there is room for it and no job is already running
    /// under that tag, and say why not otherwise.
    fn accept(&self, tag: &str) -> Result<(), String> {
        let Ok(mut live) = self.live.lock() else {
            return Err("the module's job table is unavailable".to_string());
        };
        if live.contains(tag) {
            return Err(format!("an extraction tagged `{tag}` is already running"));
        }
        if live.len() >= self.max_concurrent {
            return Err(format!(
                "{} extractions are already running, which is the limit this app configured",
                live.len()
            ));
        }
        live.insert(tag.to_string());
        Ok(())
    }

    /// Give a tag back once its job has reported.
    fn release(&self, tag: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(tag);
        }
    }
}

/// Archive extraction for a Lumen app: install it and `archive::extract`
/// exists.
///
/// Ships as the bundled `lumen-archive` runtime module (an app declares
/// `lumen-archive = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin in a static build. Without it the function
/// does not exist and a script call fails with the host's ordinary
/// unknown-function error.
pub struct ArchivePlugin {
    max_concurrent: usize,
}

impl ArchivePlugin {
    /// Build from the module's `config` table. `max_concurrent` is how many
    /// extractions may run at once, clamped into the range the module
    /// supports; anything else leaves the default in place.
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        match config.int("max_concurrent") {
            Some(limit) => Self::with_max_concurrent(limit),
            None => Self::default(),
        }
    }

    /// Build with an explicit concurrency limit, clamped into the supported
    /// range. This is what a static build sets when it installs the plugin
    /// itself and has no `config` table to read.
    #[must_use]
    pub fn with_max_concurrent(limit: i64) -> Self {
        let limit = limit.clamp(1, MAX_MAX_CONCURRENT);
        Self {
            max_concurrent: usize::try_from(limit).unwrap_or(1),
        }
    }
}

impl Default for ArchivePlugin {
    fn default() -> Self {
        Self::with_max_concurrent(DEFAULT_MAX_CONCURRENT)
    }
}

/// The queue the tick system drains, held as a resource so the system reaches
/// what the script-function bodies filled.
#[derive(Resource)]
struct ArchiveJobs(Shared);

impl Plugin for ArchivePlugin {
    fn build(self, app: &mut App) {
        let shared = Shared::new(self.max_concurrent);
        app.add_script_fns(script_fns(&shared));
        app.world.insert_resource(ArchiveJobs(shared));
        // Ahead of the hosts' signal mirror sync, so a job queued during a
        // handler starts on the tick that queued it rather than the next one.
        app.add_systems(
            TickStage::Systems,
            tick_archive.before(ScriptSet::SyncSignals),
        );
    }
}

/// The `archive` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
fn script_fns(shared: &Shared) -> Vec<ScriptFn> {
    let shared = shared.clone();
    vec![
        ScriptFn::new("extract")
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(
                "Unpack an archive into a directory; true when the job was taken. \
                 Options: include, the glob patterns of the files to keep.",
            )
            .param("src", T::Str)
            .param("dest", T::Str)
            .param("tag", T::Str)
            .param("opts", T::Struct(options_type()))
            .ret(T::Bool)
            .build(move |cx| {
                let tag = cx.str_arg(2);
                let filter = match extract_filter(cx.arg_ref(3)) {
                    Ok(filter) => filter,
                    Err(why) => {
                        report_error(&tag, &why);
                        return Ok(ScriptValue::Bool(false));
                    }
                };
                if let Err(why) = shared.accept(&tag) {
                    report_error(&tag, &why);
                    return Ok(ScriptValue::Bool(false));
                }
                let job = Job {
                    src: app_paths::resolve(cx.str_arg(0)),
                    dest: app_paths::resolve(cx.str_arg(1)),
                    tag,
                    filter,
                };
                if let Ok(mut queue) = shared.queue.lock() {
                    queue.push(job);
                } else {
                    shared.release(&job.tag);
                    report_error(&job.tag, "the module's job queue is unavailable");
                    return Ok(ScriptValue::Bool(false));
                }
                Ok(ScriptValue::Bool(true))
            }),
    ]
}

/// The name of the options struct, `archive::ExtractOptions` in candela.
const OPTIONS: &str = "ExtractOptions";

/// The fourth argument of `archive::extract`, described once for every host:
/// a candela struct, a Rhai or Lua map. An empty `include` keeps every file.
fn options_type() -> ScriptStruct {
    ScriptStruct::new(OPTIONS).field("include", T::Array(Box::new(T::Str)))
}

/// The filter the options ask for, or `None` when they keep every file. The
/// value arrives with every field present, so a field of the wrong kind is a
/// host that did not check it; it is refused rather than guessed at.
fn extract_filter(value: &ScriptValue) -> Result<Option<unpack::Filter>, String> {
    let ScriptValue::Map(map) = value else {
        return Err(format!("options must be an {OPTIONS}"));
    };
    let Some(ScriptValue::Array(patterns)) = map.get("include") else {
        return Err("option `include` must be a list of strings".to_string());
    };
    if patterns.is_empty() {
        return Ok(None);
    }
    patterns
        .iter()
        .map(|pattern| match pattern {
            ScriptValue::Str(text) => Ok(text.clone()),
            _ => Err("option `include` must be a list of strings".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|patterns| Some(unpack::Filter::new(patterns)))
}

/// Per-tick pump: start whatever the script queued since the last tick.
fn tick_archive(jobs: Option<Res<ArchiveJobs>>, spawn: Option<Res<SpawnService>>) {
    let Some(jobs) = jobs else {
        return;
    };
    let queued = match jobs.0.queue.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    };
    for job in queued {
        spawn_extraction(spawn.as_ref().map(|s| s.handle()), jobs.0.clone(), job);
    }
}

/// Unpack one archive off the tick loop and report what happened.
///
/// The work runs on the engine's spawn seam, the blocking pool of whatever
/// [`SpawnService`] the app installed, like every other module that leaves
/// the tick loop. An app with no async backend gets a short-lived plain
/// thread instead; the read must still leave the tick loop, because an
/// archive of any size would otherwise stall the frame.
fn spawn_extraction(spawn: Option<Arc<dyn Spawn>>, shared: Shared, job: Job) {
    let work = move || {
        let outcome = unpack::extract(&job.src, &job.dest, job.filter.as_ref());
        shared.release(&job.tag);
        match outcome {
            Ok(unpacked) => {
                push_plugin_event(PluginEvent::Call {
                    event: DONE_EVENT.to_string(),
                    key: job.tag,
                    fallback: DONE_FALLBACK.to_string(),
                    args: vec![
                        ScriptValue::Str(job.dest.to_string_lossy().into_owned()),
                        ScriptValue::I64(i64::try_from(unpacked.files).unwrap_or(i64::MAX)),
                    ],
                });
            }
            Err(message) => report_error(&job.tag, &message),
        }
    };
    match spawn {
        Some(spawn) => spawn.spawn_blocking(Box::new(work)),
        None => {
            let spawned = std::thread::Builder::new()
                .name("lumen-archive".into())
                .spawn(work);
            if let Err(e) = spawned {
                eprintln!("lumen-archive: could not spawn the extraction thread: {e}");
            }
        }
    }
}

/// Deliver one failure the way every plugin event is delivered: the per-tag
/// `on("archive_error", tag, fn)` registration wins, else `on_archive_error`.
fn report_error(tag: &str, message: &str) {
    push_plugin_event(PluginEvent::Call {
        event: ERROR_EVENT.to_string(),
        key: tag.to_string(),
        fallback: ERROR_FALLBACK.to_string(),
        args: vec![ScriptValue::Str(message.to_string())],
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn options(include: Vec<ScriptValue>) -> ScriptValue {
        ScriptValue::Map([("include".to_string(), ScriptValue::Array(include))].into())
    }

    fn text(s: &str) -> ScriptValue {
        ScriptValue::Str(s.to_string())
    }

    /// No fields set, an empty map, and an empty Lua table all keep every
    /// file.
    #[test]
    fn no_options_keep_every_file() {
        for value in [
            ScriptValue::Map(Default::default()),
            ScriptValue::Array(Vec::new()),
        ] {
            assert_eq!(extract_filter(&options_type().complete(&value)), Ok(None));
        }
    }

    /// The patterns read into the filter the unpacker matches with.
    #[test]
    fn include_reads_into_the_filter() {
        let filter = extract_filter(&options(vec![text("*.so"), text("*.dll")]))
            .expect("valid options")
            .expect("a filter");
        assert!(filter.matches(Path::new("linux/x64/liblwjgl.so")));
        assert!(filter.matches(Path::new("lwjgl.dll")));
        assert!(!filter.matches(Path::new("META-INF/MANIFEST.MF")));
    }

    /// A pattern that is not a string is refused rather than guessed at.
    #[test]
    fn a_pattern_of_the_wrong_kind_is_refused() {
        let error = extract_filter(&options(vec![ScriptValue::I64(1)])).expect_err("refused");
        assert!(error.contains("include"), "{error}");
        assert!(extract_filter(&ScriptValue::Unit).is_err());
    }
}
