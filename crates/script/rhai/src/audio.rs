//! Audio-control builtins for the Rhai host.
//!
//! Kept in its own module (registered from `RhaiHost::new` with a single
//! `audio::register(...)` call) so the audio surface adds essentially
//! nothing to the busy `builtins.rs` / `lib.rs` diff.
//!
//! Every function here is a *thin enqueue*: it pushes a backend-agnostic
//! [`ScriptCommand`] onto the shared command sink and returns. The actual
//! playback happens in the embedder's applier
//! (`lumenc::run::apply_audio_commands`) against the `lumen-audio`
//! `AudioService`. Because the capability is a `ScriptCommand` - not a
//! Rhai-only primitive - every binding (Rust SDK, Python/C#, C-ABI
//! plugins, and any future script host) drives the exact same seam.
//!
//! The reactive read-backs (`position` / `duration` / `playing`) are *not*
//! builtins: the embedder writes them into signals on the UI thread each
//! woken tick (see `poll_audio`), so scripts consume them through normal
//! `bind-*` / `derive()` machinery.

use std::sync::Arc;

use lumen_script::ScriptCommand;
use parking_lot::Mutex;
use rhai::{Engine, ImmutableString};

/// Register `audio_*` control builtins on `engine`, each pushing onto the
/// shared `sink`. Mirror of Qt's `QMediaPlayer` control slots
/// (`play`/`pause`/`stop`/`setPosition`) + `QAudioOutput::setVolume`.
pub(crate) fn register(engine: &mut Engine, sink: &Arc<Mutex<Vec<ScriptCommand>>>) {
    let s = sink.clone();
    engine.register_fn("audio_play", move |path: ImmutableString| {
        s.lock().push(ScriptCommand::AudioPlay {
            path: path.to_string(),
        });
    });

    let s = sink.clone();
    engine.register_fn("audio_pause", move || {
        s.lock().push(ScriptCommand::AudioPause);
    });

    let s = sink.clone();
    engine.register_fn("audio_resume", move || {
        s.lock().push(ScriptCommand::AudioResume);
    });

    let s = sink.clone();
    engine.register_fn("audio_stop", move || {
        s.lock().push(ScriptCommand::AudioStop);
    });

    // Seek accepts both float seconds (`audio_seek(30.5)`) and the integer
    // literal form (`audio_seek(30)`) Rhai produces for a bare int.
    let s = sink.clone();
    engine.register_fn("audio_seek", move |secs: f64| {
        s.lock().push(ScriptCommand::AudioSeek { secs });
    });
    let s = sink.clone();
    engine.register_fn("audio_seek", move |secs: i64| {
        s.lock()
            .push(ScriptCommand::AudioSeek { secs: secs as f64 });
    });

    // Volume 0.0..=1.0; accept an int too so `audio_volume(1)` works.
    let s = sink.clone();
    engine.register_fn("audio_volume", move |level: f64| {
        s.lock().push(ScriptCommand::AudioVolume {
            level: level as f32,
        });
    });
    let s = sink.clone();
    engine.register_fn("audio_volume", move |level: i64| {
        s.lock().push(ScriptCommand::AudioVolume {
            level: level as f32,
        });
    });
}
