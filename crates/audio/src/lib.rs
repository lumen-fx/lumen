//! Audio playback subsystem for Lumen.
//!
//! [`AudioService`] is a thin, UI-thread-friendly control surface over a
//! [`rodio`] device sink + `Player`. Its shape mirrors Qt's
//! `QMediaPlayer` / `QAudioOutput`:
//!
//! - a three-state machine ([`PlaybackState::Stopped`] /
//!   [`PlaybackState::Playing`] / [`PlaybackState::Paused`]) - Qt's
//!   `QMediaPlayer::PlaybackState`;
//! - `position` / `duration` in seconds - Qt's `positionChanged` /
//!   `durationChanged` signals (Lumen surfaces them as reactive signals
//!   the host writes, see `lumenc`'s `poll_audio`);
//! - [`AudioService::seek`] - Qt's `setPosition`;
//! - [`AudioService::set_volume`] `0.0..=1.0` - Qt's `QAudioOutput::setVolume`.
//!
//! ## Decoding + mixing runs off the UI thread
//!
//! rodio's `Player` hands the decoded [`rodio::Source`] to cpal's audio
//! callback thread; decode + resample + mix all happen there. The UI /
//! ECS thread only issues control-plane calls (play/pause/seek/volume),
//! which are cheap atomic + channel ops. **Nothing here blocks the tick.**
//!
//! ## Position marshalling (Slint `invoke_from_event_loop` discipline)
//!
//! Slint has no audio, but its threading rule is the reference: values
//! produced off the UI thread must be applied *on* it before touching a
//! reactive property. We follow it exactly. A tiny background **ticker
//! thread** ([`AudioService::new`]) does nothing but fire the wired
//! [`EventLoopWaker`] on a fixed cadence *while playing* - it never
//! touches a signal. The woken tick then calls [`AudioService::refresh`]
//! **on the UI/ECS thread**, reads the position, and writes it into a
//! signal. When paused/stopped the ticker goes quiet, so the event loop
//! parks and idle quiescence holds - no per-frame polling.
//!
//! ## Graceful headless degradation
//!
//! Opening the default device can fail (no ALSA/PulseAudio server, CI,
//! SSH). [`AudioService::new`] logs once and drops into a **null device**:
//! every control call still updates the state machine and the clock-based
//! position model, so seeks/pauses/duration all behave and are testable,
//! there is simply no sound. This lets the whole pipeline be verified
//! without speakers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod synth;

use lumen_core::app::EventLoopWaker;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

/// Cadence of the position-update wakeups while a track is playing.
///
/// ~6-7 Hz: smooth enough for a seek bar without a busy loop. Qt's
/// `QMediaPlayer` defaults to a coarser 1 s `notifyInterval`; a media UI
/// with a scrubber wants finer, so we sit between the two.
const TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Playback state machine - mirrors `QMediaPlayer::PlaybackState`.
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
/// [`AudioService::refresh`] on the UI/ECS thread. `ended` is an *edge*:
/// true only on the single refresh where a playing track reached its end.
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
    /// The bytes could not be decoded (unknown/unsupported codec, or a
    /// codec whose cargo feature is disabled - only wav+ogg are on).
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

/// The live rodio output. Absent in null (headless) mode.
struct Output {
    // Field order matters for drop: the `Player` must drop before the
    // `MixerDeviceSink` that backs its mixer.
    player: Player,
    _device_sink: MixerDeviceSink,
}

/// Handle to the background ticker thread. Dropping it signals the thread
/// to exit at its next wake (<= [`TICK_INTERVAL`]).
struct Ticker {
    alive: Arc<AtomicBool>,
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

/// Playback controller: one loaded track at a time, Qt-`QMediaPlayer`
/// shaped. Held as a `NonSend` resource because rodio's `MixerDeviceSink`
/// wraps a `!Send` cpal stream.
///
/// The position model is **clock-based** and identical in device and
/// null mode: `position = base_pos + (now - started_at)` while playing,
/// clamped to `duration`. Using our own clock (rather than
/// `Player::get_pos`) keeps headless tests meaningful and sidesteps the
/// known "`get_pos` overshoots `duration`" rodio quirk.
pub struct AudioService {
    output: Option<Output>,
    duration: Duration,
    /// Position at the last state change (play/pause/seek boundary).
    base_pos: Duration,
    /// Wall-clock moment playback resumed from `base_pos`; `None` when paused/stopped.
    started_at: Option<Instant>,
    /// Shared with the ticker thread so it only wakes the loop while playing.
    playing: Arc<AtomicBool>,
    volume: f32,
    loaded: bool,
    /// Encoded bytes of the loaded track, retained so a `resume()` after a
    /// natural end or a `stop()` (both of which `player.clear()` and thus drop
    /// the decoded source) can re-append a fresh `Decoder` instead of
    /// playing a `try_seek`-on-empty-player silence.
    bytes: Option<Arc<[u8]>>,
    /// True when the player's decoded source has been dropped (`stop()` or a
    /// natural end) while `loaded` is still true. A `resume()` in this state
    /// must rebuild the source from `bytes` before playing.
    cleared: bool,
    waker: Arc<Mutex<Option<EventLoopWaker>>>,
    /// `Some` for a live service (a ticker thread is running); `None` for a
    /// [`AudioService::disabled`] service, which spawns no thread and opens
    /// no device.
    _ticker: Option<Ticker>,
}

impl Default for AudioService {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioService {
    /// Open the default output device and spawn the ticker thread.
    ///
    /// Never fails: on device-open error it logs once and runs in null
    /// mode (state machine + clock still work, no sound).
    pub fn new() -> Self {
        let output = match DeviceSinkBuilder::open_default_sink() {
            Ok(device_sink) => {
                let player = Player::connect_new(device_sink.mixer());
                Some(Output {
                    player,
                    _device_sink: device_sink,
                })
            }
            Err(e) => {
                eprintln!(
                    "lumen-audio: no output device ({e}); running silent (null sink). \
                     Playback state, seeking and duration still work; there is just no sound."
                );
                None
            }
        };

        let playing = Arc::new(AtomicBool::new(false));
        let waker: Arc<Mutex<Option<EventLoopWaker>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));

        // Ticker: wake the parked event loop ~6-7x/s *only while playing*
        // so `poll_audio` runs on the UI thread and pushes position into a
        // signal. Silent when paused -> the loop parks -> idle quiescence.
        {
            let playing = Arc::clone(&playing);
            let waker = Arc::clone(&waker);
            let alive = Arc::clone(&alive);
            std::thread::Builder::new()
                .name("lumen-audio-ticker".into())
                .spawn(move || {
                    while alive.load(Ordering::Acquire) {
                        std::thread::sleep(TICK_INTERVAL);
                        if playing.load(Ordering::Acquire)
                            && let Ok(guard) = waker.lock()
                            && let Some(w) = guard.as_ref()
                        {
                            w.wake();
                        }
                    }
                })
                .expect("spawn lumen-audio ticker");
        }

        Self {
            output,
            duration: Duration::ZERO,
            base_pos: Duration::ZERO,
            started_at: None,
            playing,
            volume: 1.0,
            loaded: false,
            bytes: None,
            cleared: false,
            waker,
            _ticker: Some(Ticker { alive }),
        }
    }

    /// Construct an inert audio service: **no** output device is opened and
    /// **no** ticker thread is spawned. Every control call still updates the
    /// clock-based state machine (so `state()` / `position_secs()` /
    /// `duration_secs()` behave), there is simply never any sound and the
    /// event loop is never woken for position updates.
    ///
    /// The startup subsystem-gating path installs this instead of
    /// [`Self::new`] when a static scan finds no audio usage in the app
    /// (no `audio_*` script builtins, no audio asset references), so a
    /// pure-UI app pays neither the device-open cost, the rodio
    /// `MixerDeviceSink`, nor a background thread. An app that *does* use audio
    /// (or forces it on via `lumen.toml`) still gets the full [`Self::new`]
    /// service. `has_device()` returns `false`, exactly like the null-sink
    /// fallback of [`Self::new`].
    pub fn disabled() -> Self {
        Self {
            output: None,
            duration: Duration::ZERO,
            base_pos: Duration::ZERO,
            started_at: None,
            playing: Arc::new(AtomicBool::new(false)),
            volume: 1.0,
            loaded: false,
            bytes: None,
            cleared: false,
            waker: Arc::new(Mutex::new(None)),
            _ticker: None,
        }
    }

    /// Wire the event-loop waker so the ticker can wake a parked loop.
    /// Idempotent; called lazily by the embedder once the resource exists.
    pub fn set_waker(&self, waker: EventLoopWaker) {
        if let Ok(mut guard) = self.waker.lock() {
            *guard = Some(waker);
        }
    }

    /// Whether a real output device was acquired. `false` = null mode.
    pub fn has_device(&self) -> bool {
        self.output.is_some()
    }

    /// Load and start playing a track from its **encoded bytes** (a full
    /// wav/ogg container). This is the production entry point: the bytes
    /// come from `lumen-assets`' `AudioServer` (read + cached off the UI
    /// thread), so nothing here opens a file. rodio decodes lazily on the
    /// cpal audio thread as it mixes. Replaces any current track and
    /// resets position to 0. Qt: `setSource` + `play`.
    pub fn play_bytes(&mut self, bytes: Arc<[u8]>) -> Result<(), AudioError> {
        // Probe the encoded bytes for a total duration before the decoder
        // consumes them. `Arc<[u8]>` is `AsRef<[u8]> + Send + Sync`, so the
        // clone here is a refcount bump, not a byte copy. `Arc::clone` for the
        // cursor keeps the original `bytes` alive so it can be retained below
        // for a later resume-from-end / resume-after-stop rebuild.
        let probed = probe_duration(&bytes);

        let cursor = std::io::Cursor::new(Arc::clone(&bytes));
        let source = Decoder::new(cursor).map_err(|e| AudioError::Decode(e.to_string()))?;
        // Capture duration before the source is moved into the player.
        // WAV reports it directly via symphonia's frame count; Ogg/Vorbis
        // returns `None` (no frame count without the final granule position),
        // so fall back to our explicit symphonia probe of the container.
        // If neither yields a value the bar can't scale, but transport works.
        self.duration = match source.total_duration() {
            Some(d) if d > Duration::ZERO => d,
            _ => probed.unwrap_or(Duration::ZERO),
        };

        if let Some(out) = &mut self.output {
            out.player.clear(); // drop any previous track, leaves player paused
            out.player.set_volume(self.volume);
            out.player.append(source);
            out.player.play();
        }

        // Retain the encoded bytes so a later resume-from-end / resume-after-
        // stop can rebuild a fresh source (see `resume`).
        self.bytes = Some(bytes);
        self.base_pos = Duration::ZERO;
        self.started_at = Some(Instant::now());
        self.playing.store(true, Ordering::Release);
        self.loaded = true;
        self.cleared = false;
        Ok(())
    }

    /// Rebuild the player's decoded source from the retained encoded bytes.
    /// Used by [`Self::resume`] when the previous source was dropped by a
    /// `stop()` or a natural end. Recomputes `duration` from the fresh
    /// decoder (identical value, but proves the load path re-ran) and, in
    /// device mode, re-appends the source so playback actually produces
    /// sound again. No-op in null mode beyond the duration refresh; a
    /// decode failure leaves the transport quietly loaded.
    fn reload_source(&mut self) {
        let Some(bytes) = self.bytes.clone() else {
            return;
        };
        let cursor = std::io::Cursor::new(bytes);
        match Decoder::new(cursor) {
            Ok(source) => {
                self.duration = source.total_duration().unwrap_or(Duration::ZERO);
                if let Some(out) = &mut self.output {
                    out.player.clear();
                    out.player.set_volume(self.volume);
                    out.player.append(source);
                }
            }
            Err(_) => {
                // Bytes that decoded once should decode again; if they
                // somehow don't, keep the transport in a sane state rather
                // than panicking off the UI thread.
            }
        }
    }

    /// Convenience for tests and the track-generator tool: read a file and
    /// play it. **Not used by the runtime** - production playback flows
    /// through [`Self::play_bytes`] with `AudioServer`-provided bytes so
    /// the UI thread never touches the filesystem.
    pub fn play(&mut self, path: &Path) -> Result<(), AudioError> {
        let bytes = std::fs::read(path).map_err(AudioError::Io)?;
        self.play_bytes(Arc::from(bytes))
    }

    /// Hold the transport at the current position. No-op when not playing.
    pub fn pause(&mut self) {
        if !self.playing.load(Ordering::Acquire) {
            return;
        }
        self.base_pos = self.raw_position();
        self.started_at = None;
        self.playing.store(false, Ordering::Release);
        if let Some(out) = &self.output {
            out.player.pause();
        }
    }

    /// Resume a paused track. No-op if nothing is loaded. A track sitting
    /// at its end restarts from 0.
    pub fn resume(&mut self) {
        if !self.loaded || self.playing.load(Ordering::Acquire) {
            return;
        }
        let at_end = self.duration > Duration::ZERO && self.base_pos >= self.duration;
        if self.cleared || at_end {
            // The decoded source was dropped (stop / natural end). A bare
            // `try_seek(0)` on the now-empty player plays silence while the
            // clock climbs (phantom position). Rebuild it from the retained
            // bytes so playback restarts from 0 for real.
            self.reload_source();
            self.base_pos = Duration::ZERO;
            self.cleared = false;
        }
        self.started_at = Some(Instant::now());
        self.playing.store(true, Ordering::Release);
        if let Some(out) = &self.output {
            out.player.play();
        }
    }

    /// Stop playback and rewind to 0, keeping the track loaded.
    pub fn stop(&mut self) {
        self.base_pos = Duration::ZERO;
        self.started_at = None;
        self.playing.store(false, Ordering::Release);
        // `clear()` drops the decoded source; flag a reload so the next
        // `resume()` rebuilds it instead of playing an empty player.
        self.cleared = true;
        if let Some(out) = &self.output {
            out.player.clear();
        }
    }

    /// Seek to `secs` (clamped to `0..=duration`). Qt: `setPosition`.
    pub fn seek(&mut self, secs: f64) {
        let mut target = Duration::from_secs_f64(secs.max(0.0));
        if self.duration > Duration::ZERO && target > self.duration {
            target = self.duration;
        }
        self.base_pos = target;
        self.started_at = if self.playing.load(Ordering::Acquire) {
            Some(Instant::now())
        } else {
            None
        };
        if let Some(out) = &self.output {
            let _ = out.player.try_seek(target);
        }
    }

    /// Set output volume, `0.0..=1.0`. Qt: `QAudioOutput::setVolume`.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(out) = &self.output {
            out.player.set_volume(self.volume);
        }
    }

    /// Current volume, `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        if !self.loaded {
            PlaybackState::Stopped
        } else if self.playing.load(Ordering::Acquire) {
            PlaybackState::Playing
        } else {
            PlaybackState::Paused
        }
    }

    /// Current position in seconds.
    pub fn position_secs(&self) -> f64 {
        self.raw_position().as_secs_f64()
    }

    /// Track duration in seconds (0.0 when unknown / nothing loaded).
    pub fn duration_secs(&self) -> f64 {
        self.duration.as_secs_f64()
    }

    /// Advance the clock, clamp, and detect end-of-track. Call this on the
    /// UI/ECS thread each woken tick; the returned [`AudioSnapshot`] is
    /// what the embedder pushes into signals. `ended` is a one-shot edge.
    pub fn refresh(&mut self) -> AudioSnapshot {
        let mut ended = false;
        if self.playing.load(Ordering::Acquire)
            && self.duration > Duration::ZERO
            && self.raw_position() >= self.duration
        {
            self.base_pos = self.duration;
            self.started_at = None;
            self.playing.store(false, Ordering::Release);
            // `clear()` drops the decoded source; flag a reload so a
            // resume-from-end rebuilds it instead of playing an empty player.
            self.cleared = true;
            if let Some(out) = &self.output {
                out.player.clear();
            }
            ended = true;
        }
        AudioSnapshot {
            position: self.raw_position().as_secs_f64(),
            duration: self.duration.as_secs_f64(),
            playing: self.playing.load(Ordering::Acquire),
            ended,
        }
    }

    /// Clock-based current position, clamped to `duration`.
    fn raw_position(&self) -> Duration {
        match self.started_at {
            Some(t) if self.playing.load(Ordering::Acquire) => {
                let p = self.base_pos + t.elapsed();
                if self.duration > Duration::ZERO && p > self.duration {
                    self.duration
                } else {
                    p
                }
            }
            _ => self.base_pos,
        }
    }
}

/// Probe the total duration of an encoded audio container (Ogg/Vorbis, WAV)
/// without decoding it.
///
/// rodio's `Source::total_duration()` returns `None` for Ogg/Vorbis because
/// a Vorbis stream carries no up-front sample count - the total is only
/// recoverable from the granule position on the final Ogg page. symphonia's
/// format reader reads that page during probing and populates
/// `codec_params.n_frames`, from which `n_frames / sample_rate` gives the
/// real length. This runs once at load and touches metadata only (it does
/// not pull packets / decode audio).
///
/// Returns `None` when the format is unrecognised or the frame count /
/// sample rate can't be determined; the caller then keeps `Duration::ZERO`.
fn probe_duration(bytes: &Arc<[u8]>) -> Option<Duration> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    // `Arc<[u8]>` is `AsRef<[u8]> + Send + Sync`, satisfying symphonia's
    // blanket `MediaSource for Cursor<T>` impl; the clone is a refcount bump.
    let cursor = std::io::Cursor::new(Arc::clone(bytes));
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    let track = probed.format.default_track()?;
    let params = &track.codec_params;
    let n_frames = params.n_frames?;

    // Prefer the codec's time base if present (frames -> seconds exactly);
    // otherwise derive from the sample rate.
    if let Some(tb) = params.time_base {
        let t = tb.calc_time(n_frames);
        return Some(Duration::from_secs(t.seconds) + Duration::from_secs_f64(t.frac));
    }
    let sample_rate = params.sample_rate?;
    if sample_rate == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        n_frames as f64 / sample_rate as f64,
    ))
}
