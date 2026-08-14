//! What audio playback means in Lumen, without saying who performs it.
//!
//! This crate holds the transport model: the three playback states, the
//! playhead and track length, seeking, volume, end of track, and the
//! [`AudioBackend`] trait that a device implementation fills in. It links no
//! audio device and no decoder, so an app, a widget, or a test can talk about
//! playback without pulling one in. `lumen-audio-rodio` is the backend Lumen
//! ships; swapping in another one means implementing [`AudioBackend`] and
//! handing the runtime an [`AudioService`] that wraps it.
//!
//! The surface mirrors Qt's `QMediaPlayer` / `QAudioOutput`:
//!
//! - a three-state machine ([`PlaybackState::Stopped`] /
//!   [`PlaybackState::Playing`] / [`PlaybackState::Paused`]), Qt's
//!   `QMediaPlayer::PlaybackState`;
//! - `position` / `duration` in seconds, Qt's `positionChanged` /
//!   `durationChanged` signals (Lumen surfaces them as reactive signals the
//!   host writes, see the runtime's `poll_audio`);
//! - [`AudioBackend::seek`], Qt's `setPosition`;
//! - [`AudioBackend::set_volume`] over `0.0..=1.0`, Qt's
//!   `QAudioOutput::setVolume`.
//!
//! ## What a backend has to do, and what it gets for free
//!
//! A backend decodes bytes, feeds a device, and reports whether it has one.
//! Everything device-independent is already written: [`Transport`] runs the
//! state machine and the clock, and [`PositionTicker`] runs the wakeup pump
//! that lets a playing track drive a seek bar. A backend that uses both is
//! mostly glue, and one that needs neither can ignore them; the trait does not
//! require either.
//!
//! Control calls must stay cheap, because they run on the UI thread inside the
//! ECS tick. Decoding and mixing belong on a backend's own thread.
//!
//! ## Playing without a device
//!
//! [`NullAudio`] implements the whole transport and produces no sound. It is
//! what a build with no backend selected runs, and it is what a test uses when
//! the machine has no audio endpoint: state, seeking, and the playhead all
//! behave, so a pipeline can be verified without speakers. Track length is the
//! one thing it cannot know, since reading it means decoding.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod synth;
pub mod ticker;
pub mod transport;

pub use ticker::{PositionTicker, TICK_INTERVAL};
pub use transport::{Resume, Transport};

/// Re-export for backends that depend on `lumen-audio` but not directly on
/// `lumen-core`. The waker is what [`AudioBackend::set_waker`] takes.
pub use lumen_core::app::EventLoopWaker;

use std::path::Path;
use std::sync::Arc;

/// Playback state machine, mirroring `QMediaPlayer::PlaybackState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// No track loaded, or explicitly stopped. Position is 0.
    Stopped,
    /// A track is loaded and advancing.
    Playing,
    /// A track is loaded but held at its current position.
    Paused,
}

/// A read-only snapshot of the transport, produced by
/// [`AudioBackend::refresh`] on the UI/ECS thread. `ended` is an edge: true
/// only on the single refresh where a playing track reached its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSnapshot {
    /// Current playback position in seconds.
    pub position: f64,
    /// Total track duration in seconds (0.0 when unknown).
    pub duration: f64,
    /// Whether the transport is currently advancing.
    pub playing: bool,
    /// One-shot edge: the playing track just reached its end this refresh.
    pub ended: bool,
}

/// Errors from loading a track.
#[derive(Debug)]
pub enum AudioError {
    /// The file could not be opened.
    Io(std::io::Error),
    /// The bytes could not be decoded: an unknown or unsupported codec, or a
    /// codec the backend was built without.
    Decode(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::Io(e) => write!(f, "audio: open failed: {e}"),
            AudioError::Decode(e) => write!(f, "audio: decode failed: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        AudioError::Io(e)
    }
}

/// One loaded track at a time, driven from the UI thread.
///
/// Implementations decode encoded bytes, play them, and answer for the
/// transport. The control calls run inside the ECS tick, so they must not
/// block: decode and mix elsewhere.
pub trait AudioBackend {
    /// Load and start playing a track from its encoded bytes (a full container
    /// such as wav or ogg). Replaces any current track and resets the position
    /// to zero. Qt: `setSource` plus `play`.
    ///
    /// The runtime always arrives here with bytes the asset server already read
    /// off-thread, so nothing on the UI thread touches the filesystem.
    fn play_bytes(&mut self, bytes: Arc<[u8]>) -> Result<(), AudioError>;

    /// Hold the transport at the current position. No-op when not playing.
    fn pause(&mut self);

    /// Resume a paused track. No-op if nothing is loaded. A track sitting at
    /// its end restarts from the beginning.
    fn resume(&mut self);

    /// Stop playback and rewind to zero, keeping the track loaded.
    fn stop(&mut self);

    /// Seek to `secs`, clamped to `0..=duration`. Qt: `setPosition`.
    fn seek(&mut self, secs: f64);

    /// Set the output volume over `0.0..=1.0`. Qt:
    /// `QAudioOutput::setVolume`.
    fn set_volume(&mut self, volume: f32);

    /// Current volume, `0.0..=1.0`.
    fn volume(&self) -> f32;

    /// Current playback state.
    fn state(&self) -> PlaybackState;

    /// Current position in seconds.
    fn position_secs(&self) -> f64;

    /// Track duration in seconds (0.0 when unknown or nothing is loaded).
    fn duration_secs(&self) -> f64;

    /// Advance the clock, clamp it, and detect end of track. The embedder calls
    /// this on the UI/ECS thread each woken tick and pushes the returned
    /// snapshot into signals. `ended` is a one-shot edge.
    fn refresh(&mut self) -> AudioSnapshot;

    /// Wire the event-loop waker, so a backend that updates the position from
    /// its own thread can wake a parked loop. Idempotent; the embedder calls it
    /// once the waker exists, which is after the backend is built.
    fn set_waker(&self, waker: EventLoopWaker);

    /// Whether a real output device is open. False means the transport works
    /// and there is no sound.
    fn has_device(&self) -> bool;

    /// Read a file and play it. Convenience for tests and tools; the runtime
    /// uses [`Self::play_bytes`] so the UI thread never reads from disk.
    fn play(&mut self, path: &Path) -> Result<(), AudioError> {
        let bytes = std::fs::read(path)?;
        self.play_bytes(Arc::from(bytes))
    }
}

/// The audio backend an app runs, held by the runtime as a `NonSend` resource
/// (an output device is rarely `Send`).
///
/// It derefs to the backend, so `service.pause()` and the rest of
/// [`AudioBackend`] work directly on it. Build one from any backend with
/// `AudioService::from(backend)`, and replace the runtime's default by
/// inserting your own in an app hook.
pub struct AudioService(Box<dyn AudioBackend>);

impl AudioService {
    /// Wrap a backend.
    pub fn new<B: AudioBackend + 'static>(backend: B) -> Self {
        Self(Box::new(backend))
    }
}

impl<B: AudioBackend + 'static> From<B> for AudioService {
    fn from(backend: B) -> Self {
        Self::new(backend)
    }
}

impl std::ops::Deref for AudioService {
    type Target = dyn AudioBackend;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl std::ops::DerefMut for AudioService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}

impl Default for AudioService {
    fn default() -> Self {
        Self::new(NullAudio::default())
    }
}

/// The backend that plays nothing.
///
/// It runs the full transport, so state, seeking, volume, and the playhead all
/// behave; there is no device and no decoder, which is why the track duration
/// stays at zero and a track therefore never reaches an end. A build with no
/// audio backend selected runs this, and a test that must not touch an audio
/// endpoint can use it directly.
#[derive(Default)]
pub struct NullAudio {
    transport: Transport,
}

impl AudioBackend for NullAudio {
    fn play_bytes(&mut self, _bytes: Arc<[u8]>) -> Result<(), AudioError> {
        self.transport.start(std::time::Duration::ZERO);
        Ok(())
    }

    fn pause(&mut self) {
        self.transport.pause();
    }

    fn resume(&mut self) {
        self.transport.resume();
    }

    fn stop(&mut self) {
        self.transport.stop();
    }

    fn seek(&mut self, secs: f64) {
        self.transport.seek(secs);
    }

    fn set_volume(&mut self, volume: f32) {
        self.transport.set_volume(volume);
    }

    fn volume(&self) -> f32 {
        self.transport.volume()
    }

    fn state(&self) -> PlaybackState {
        self.transport.state()
    }

    fn position_secs(&self) -> f64 {
        self.transport.position_secs()
    }

    fn duration_secs(&self) -> f64 {
        self.transport.duration_secs()
    }

    fn refresh(&mut self) -> AudioSnapshot {
        self.transport.refresh()
    }

    fn set_waker(&self, _waker: EventLoopWaker) {}

    fn has_device(&self) -> bool {
        false
    }
}
