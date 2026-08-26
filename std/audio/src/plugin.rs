//! The plugin that puts audio into an app: the `audio_*` script functions,
//! the playback systems, the position signals, and the end-of-track event.
//!
//! The engine has no audio surface of its own; everything an app observes
//! comes from here, through the generic seams every plugin uses:
//!
//! - the script functions register through the app's `ScriptFnRegistry`, so
//!   every host (Rhai, Lua, candela) binds them before the program loads;
//! - the playhead reaches the UI as the `audio_position` / `audio_duration` /
//!   `audio_playing` signals, written into the shared [`PropertyStore`] each
//!   woken tick, ahead of the hosts' mirror sync so `derive()`s over them
//!   recompute the same tick;
//! - end of track is a [`PluginEvent`] on the plugin-event bus: the script's
//!   `on_audio_end(path)` handler fires, and a per-key
//!   `on("audio_end", path, fn)` registration wins over it, the same routing
//!   every plugin event gets.
//!
//! A script-function body runs inside a host call with no world access, so
//! the functions talk to the systems through the module's own command queue:
//! the body pushes a command and wakes the event loop, and [`tick_audio`]
//! drains the queue on the next tick. Track bytes are read on a short-lived
//! loader thread, so the UI thread never touches the filesystem; the loaded
//! bytes come back over a channel and start playing on the tick that receives
//! them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use lumen_module::lumen_core::app::{App, EventLoopWaker, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::prelude::{
    IntoScheduleConfigs, NonSendMut, PropertyStore, Res, ResMut, TickStage,
};
use lumen_module::lumen_script::{
    PluginEvent, ScriptFn, ScriptFnAppExt, ScriptNs, ScriptSet, ScriptTy as T, ScriptValue,
    push_plugin_event,
};

use crate::backend::RodioAudio;

/// One control request a script-function body queued for [`tick_audio`].
enum AudioCmd {
    /// Load and play the file at this path, as the script spelled it.
    Play(String),
    Pause,
    Resume,
    Stop,
    Seek(f64),
    Volume(f32),
}

/// The handles a script-function body captures: the command queue and the
/// waker that gets a parked event loop to run the tick that drains it.
#[derive(Clone, Default)]
struct Shared {
    queue: Arc<Mutex<Vec<AudioCmd>>>,
    waker: Arc<Mutex<Option<EventLoopWaker>>>,
}

impl Shared {
    /// Queue a command and wake the loop so the next tick applies it.
    fn push(&self, cmd: AudioCmd) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(cmd);
        }
        if let Ok(guard) = self.waker.lock()
            && let Some(w) = guard.as_ref()
        {
            w.wake();
        }
    }
}

/// One finished load off the loader thread: the path as the script spelled
/// it, the request generation it answers, and the bytes or the error.
type LoadResult = (String, u64, Result<Vec<u8>, std::io::Error>);

/// The module's world state, `NonSend` because rodio's device sink wraps a
/// `!Send` cpal stream.
struct AudioState {
    backend: RodioAudio,
    shared: Shared,
    loaded_tx: Sender<LoadResult>,
    loaded_rx: Receiver<LoadResult>,
    /// Monotonic play-request id: a load that finishes after a newer
    /// `audio_play` is stale and dropped, mirroring the request-id discipline
    /// the asset server applies to images.
    load_gen: Arc<AtomicU64>,
    /// The path of the playing track, as the script spelled it: the key the
    /// end-of-track event carries.
    current_path: String,
    /// Whether the tick system has wired the event-loop waker yet (the
    /// resource appears only once a windowing backend runs).
    waker_wired: bool,
}

/// Audio for a Lumen app: install it and the `audio_*` functions exist.
///
/// Ships as the bundled `lumen-audio` runtime module (an app declares
/// `lumen-audio = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin in a static build. Without it the
/// functions do not exist and a script call fails with the host's ordinary
/// unknown-function error.
pub struct AudioPlugin {
    open_device: bool,
}

impl AudioPlugin {
    /// The shipping shape: open the default output device (degrading to
    /// silent when there is none) and run the position ticker.
    #[must_use]
    pub fn new() -> Self {
        Self { open_device: true }
    }

    /// The deviceless shape: no output device is opened and no ticker thread
    /// runs. The whole surface still works - state, position, duration, the
    /// signals, the end event - there is just never any sound. For tests and
    /// deviceless embedders.
    #[must_use]
    pub fn inert() -> Self {
        Self { open_device: false }
    }
}

impl Default for AudioPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for AudioPlugin {
    fn build(self, app: &mut App) {
        let backend = if self.open_device {
            RodioAudio::new()
        } else {
            RodioAudio::disabled()
        };
        let shared = Shared::default();
        let (loaded_tx, loaded_rx) = channel();
        app.world.insert_non_send(AudioState {
            backend,
            shared: shared.clone(),
            loaded_tx,
            loaded_rx,
            load_gen: Arc::new(AtomicU64::new(0)),
            current_path: String::new(),
            waker_wired: false,
        });
        app.add_script_fns(script_fns(&shared));
        // Ahead of the hosts' signal mirror sync, so a `derive()` over the
        // position signals recomputes on the same tick they change - the
        // store -> mirror -> derive discipline every other signal follows.
        app.add_systems(
            TickStage::Systems,
            tick_audio.before(ScriptSet::SyncSignals),
        );
    }
}

/// The `audio_*` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
fn script_fns(shared: &Shared) -> Vec<ScriptFn> {
    let cmd =
        |name: &str, doc: &str, params: &[(&str, T)], build: fn(&[ScriptValue]) -> AudioCmd| {
            let shared = shared.clone();
            let mut f = ScriptFn::new(name)
                .ns(ScriptNs::Builtin)
                .ret(T::Unit)
                .doc(doc);
            for (pname, ty) in params {
                f = f.param(*pname, ty.clone());
            }
            f.build(move |cx| {
                shared.push(build(cx.args()));
                Ok(ScriptValue::Unit)
            })
        };
    vec![
        cmd(
            "audio_play",
            "Play the audio file at that path.",
            &[("path", T::Str)],
            |args| AudioCmd::Play(str_arg(args, 0)),
        ),
        cmd("audio_pause", "Pause playback.", &[], |_| AudioCmd::Pause),
        cmd("audio_resume", "Resume playback.", &[], |_| {
            AudioCmd::Resume
        }),
        cmd("audio_stop", "Stop playback.", &[], |_| AudioCmd::Stop),
        cmd(
            "audio_seek",
            "Seek to that position, in seconds.",
            &[("secs", T::Float)],
            |args| AudioCmd::Seek(float_arg(args, 0)),
        ),
        cmd(
            "audio_volume",
            "Set the output volume, 0.0 to 1.0.",
            &[("level", T::Float)],
            |args| AudioCmd::Volume(float_arg(args, 0) as f32),
        ),
    ]
}

/// Argument `i` as a string; the same coercions `ScriptFnCx::str_arg` applies.
fn str_arg(args: &[ScriptValue], i: usize) -> String {
    args.get(i).map(ScriptValue::stringify).unwrap_or_default()
}

/// Argument `i` as a float; integers widen, numeric strings parse.
fn float_arg(args: &[ScriptValue], i: usize) -> f64 {
    match args.get(i) {
        Some(ScriptValue::F64(v)) => *v,
        Some(ScriptValue::I64(v)) => *v as f64,
        Some(ScriptValue::Str(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Per-tick pump: apply queued commands, take finished loads, advance the
/// transport, publish the signals, and fire the end-of-track event.
fn tick_audio(
    mut state: NonSendMut<AudioState>,
    store: Option<ResMut<PropertyStore>>,
    waker: Option<Res<EventLoopWaker>>,
) {
    let state = &mut *state;
    // Wire the loop waker lazily: the resource appears once a windowing
    // backend runs, after plugin build. Both the ticker thread and the
    // script-function bodies wake through it.
    if !state.waker_wired
        && let Some(w) = waker.as_deref()
    {
        state.backend.set_waker(w.clone());
        if let Ok(mut slot) = state.shared.waker.lock() {
            *slot = Some(w.clone());
        }
        state.waker_wired = true;
    }

    let queued = match state.shared.queue.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => Vec::new(),
    };
    for cmd in queued {
        match cmd {
            AudioCmd::Play(path) => {
                // Resolve the way every app-relative path resolves: against
                // the app directory, not the process's working directory.
                let resolved = app_paths::resolve(&path);
                state.current_path = path.clone();
                let generation = state.load_gen.fetch_add(1, Ordering::AcqRel) + 1;
                spawn_loader(
                    path,
                    resolved,
                    generation,
                    state.loaded_tx.clone(),
                    state.shared.waker.clone(),
                );
            }
            AudioCmd::Pause => state.backend.pause(),
            AudioCmd::Resume => state.backend.resume(),
            AudioCmd::Stop => state.backend.stop(),
            AudioCmd::Seek(secs) => state.backend.seek(secs),
            AudioCmd::Volume(level) => state.backend.set_volume(level),
        }
    }

    while let Ok((path, generation, outcome)) = state.loaded_rx.try_recv() {
        if generation != state.load_gen.load(Ordering::Acquire) {
            // A newer `audio_play` superseded this load.
            continue;
        }
        match outcome {
            Ok(bytes) => {
                if let Err(e) = state.backend.play_bytes(Arc::from(bytes)) {
                    eprintln!("lumen-audio: {e}");
                }
            }
            Err(e) => eprintln!("lumen-audio: track failed to load: {path}: {e}"),
        }
    }

    let snap = state.backend.refresh();
    if let Some(mut store) = store {
        // Stringified so the values flow through the same mirror/derive path
        // as script-written signals (`mirror_sync_str` parses them back into
        // the float/string the app's derives seeded).
        store.set_global_str("audio_position", format!("{:.3}", snap.position));
        store.set_global_str("audio_duration", format!("{:.3}", snap.duration));
        store.set_global_str("audio_playing", if snap.playing { "true" } else { "false" });
    }
    if snap.ended {
        // The generic delivery every plugin event gets: `on("audio_end",
        // path, fn)` wins per track, else `on_audio_end(path)` fires.
        push_plugin_event(&PluginEvent::Call {
            event: "audio_end".to_string(),
            key: state.current_path.clone(),
            fallback: "on_audio_end".to_string(),
            args: Vec::new(),
        });
    }
}

/// Read one track's bytes off-thread and hand them back to [`tick_audio`],
/// waking the loop so a parked app starts playback promptly.
fn spawn_loader(
    path: String,
    resolved: PathBuf,
    generation: u64,
    tx: Sender<LoadResult>,
    waker: Arc<Mutex<Option<EventLoopWaker>>>,
) {
    let spawned = std::thread::Builder::new()
        .name("lumen-audio-load".into())
        .spawn(move || {
            let outcome = std::fs::read(&resolved);
            let _ = tx.send((path, generation, outcome));
            if let Ok(guard) = waker.lock()
                && let Some(w) = guard.as_ref()
            {
                w.wake();
            }
        });
    if let Err(e) = spawned {
        eprintln!("lumen-audio: could not spawn the loader thread: {e}");
    }
}
