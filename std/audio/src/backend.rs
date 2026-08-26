//! The rodio playback backend: [`RodioAudio`] decodes wav and ogg, plays them
//! on the default output device, and leaves the transport model to
//! [`Transport`].
//!
//! ## Decoding and mixing run off the UI thread
//!
//! rodio's `Player` hands the decoded [`rodio::Source`] to cpal's audio
//! callback thread; decode, resample, and mix all happen there. The UI and
//! ECS thread only issues control-plane calls (play, pause, seek, volume),
//! which are cheap atomic and channel operations, so nothing here blocks the
//! tick.
//!
//! ## Degrading without a device
//!
//! Opening the default device can fail: no ALSA or PulseAudio server, CI,
//! SSH. [`RodioAudio::new`] logs once and carries on with no sink. Every
//! control call still drives the transport, so state, seeking, and duration
//! all behave and stay testable; there is simply no sound. That is what makes
//! the whole pipeline verifiable without speakers, and it is also the shape
//! [`RodioAudio::disabled`] takes on purpose.

use std::sync::Arc;
use std::time::Duration;

use lumen_module::lumen_core::app::EventLoopWaker;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

use crate::ticker::PositionTicker;
use crate::transport::{AudioSnapshot, PlaybackState, Resume, Transport};

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

/// The live rodio output. Absent when no device could be opened.
struct Output {
    // Field order matters for drop: the `Player` must drop before the
    // `MixerDeviceSink` that backs its mixer.
    player: Player,
    _device_sink: MixerDeviceSink,
}

/// rodio-backed playback: one loaded track at a time.
///
/// Held by the module as a `NonSend` world resource, because rodio's
/// `MixerDeviceSink` wraps a `!Send` cpal stream. Control calls run inside
/// the ECS tick, so they must not block: decode and mix happen elsewhere.
///
/// The playhead comes from [`Transport`]'s clock rather than
/// `Player::get_pos`, which keeps headless runs meaningful and sidesteps the
/// known "`get_pos` overshoots `duration`" rodio quirk.
pub struct RodioAudio {
    output: Option<Output>,
    transport: Transport,
    /// Encoded bytes of the loaded track, retained so a resume after a natural
    /// end or a stop (both of which `player.clear()` and so drop the decoded
    /// source) can re-append a fresh `Decoder` instead of playing a
    /// `try_seek`-on-empty-player silence.
    bytes: Option<Arc<[u8]>>,
    /// `Some` for a live backend; `None` for a [`RodioAudio::disabled`] one,
    /// which spawns no thread and opens no device.
    ticker: Option<PositionTicker>,
}

impl Default for RodioAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl RodioAudio {
    /// Open the default output device and start the position ticker.
    ///
    /// Never fails: on a device-open error it logs once and runs silent (the
    /// transport still works, there is no sound).
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
                    "lumen-audio: no output device ({e}); running silent. \
                     Playback state, seeking and duration still work; there is just no sound."
                );
                None
            }
        };

        let transport = Transport::default();
        let ticker = PositionTicker::spawn(transport.playing_flag());

        Self {
            output,
            transport,
            bytes: None,
            ticker: Some(ticker),
        }
    }

    /// Build an inert backend: no output device is opened and no ticker thread
    /// is started. Every control call still drives the transport, so state,
    /// position, and duration behave; there is never any sound, and the event
    /// loop is never woken for position updates.
    ///
    /// Tests use it to stay off the machine's audio endpoint while still
    /// exercising the real decoder; a deviceless embedder can install
    /// [`crate::AudioPlugin::inert`] for the same effect.
    pub fn disabled() -> Self {
        Self {
            output: None,
            transport: Transport::default(),
            bytes: None,
            ticker: None,
        }
    }

    /// Rebuild the player's decoded source from the retained encoded bytes,
    /// after a stop or a natural end dropped it. Refreshes the duration from
    /// the fresh decoder and, with a device, re-appends the source so playback
    /// makes sound again. A decode failure leaves the transport quietly loaded:
    /// bytes that decoded once should decode again, and panicking under the UI
    /// thread would be worse than silence.
    fn reload_source(&mut self) {
        let Some(bytes) = self.bytes.clone() else {
            return;
        };
        let cursor = std::io::Cursor::new(bytes);
        if let Ok(source) = Decoder::new(cursor) {
            self.transport
                .set_duration(source.total_duration().unwrap_or(Duration::ZERO));
            if let Some(out) = &mut self.output {
                out.player.clear();
                out.player.set_volume(self.transport.volume());
                out.player.append(source);
            }
        }
    }

    /// Load and start playing a track from its encoded bytes (a full container
    /// such as wav or ogg). Replaces any current track and resets the position
    /// to zero. Qt: `setSource` plus `play`.
    ///
    /// The module always arrives here with bytes its loader thread already
    /// read, so nothing on the UI thread touches the filesystem.
    pub fn play_bytes(&mut self, bytes: Arc<[u8]>) -> Result<(), AudioError> {
        // Probe the encoded bytes for a total duration before the decoder
        // consumes them. `Arc<[u8]>` is `AsRef<[u8]> + Send + Sync`, so the
        // clone here is a refcount bump, not a byte copy. The clone for the
        // cursor keeps the original `bytes` alive so it can be retained below
        // for a later resume-from-end / resume-after-stop rebuild.
        let probed = probe_duration(&bytes);

        let cursor = std::io::Cursor::new(Arc::clone(&bytes));
        let source = Decoder::new(cursor).map_err(|e| AudioError::Decode(e.to_string()))?;
        // Capture duration before the source is moved into the player.
        // WAV reports it directly via symphonia's frame count; Ogg/Vorbis
        // returns `None` (no frame count without the final granule position),
        // so fall back to the explicit symphonia probe of the container.
        // If neither yields a value the bar cannot scale, but transport works.
        let duration = match source.total_duration() {
            Some(d) if d > Duration::ZERO => d,
            _ => probed.unwrap_or(Duration::ZERO),
        };

        if let Some(out) = &mut self.output {
            out.player.clear(); // drop any previous track, leaves player paused
            out.player.set_volume(self.transport.volume());
            out.player.append(source);
            out.player.play();
        }

        // Retain the encoded bytes so a later resume-from-end or
        // resume-after-stop can rebuild a fresh source (see `resume`).
        self.bytes = Some(bytes);
        self.transport.start(duration);
        Ok(())
    }

    /// Read a file and play it. Convenience for tests and tools; the module's
    /// systems use [`Self::play_bytes`] so the UI thread never reads from
    /// disk.
    pub fn play(&mut self, path: &std::path::Path) -> Result<(), AudioError> {
        let bytes = std::fs::read(path)?;
        self.play_bytes(Arc::from(bytes))
    }

    /// Hold the transport at the current position. No-op when not playing.
    /// Qt: `pause`.
    pub fn pause(&mut self) {
        if self.transport.pause()
            && let Some(out) = &self.output
        {
            out.player.pause();
        }
    }

    /// Resume a paused track. No-op if nothing is loaded. A track sitting at
    /// its end restarts from the beginning. Qt: `play` from `PausedState`.
    pub fn resume(&mut self) {
        match self.transport.resume() {
            Resume::Ignored => return,
            // The decoded source was dropped (stop or natural end). A bare
            // `try_seek(0)` on the now-empty player plays silence while the
            // clock climbs (a phantom position). Rebuild it from the retained
            // bytes so playback restarts from zero for real.
            Resume::Restart => self.reload_source(),
            Resume::Continue => {}
        }
        if let Some(out) = &self.output {
            out.player.play();
        }
    }

    /// Stop playback and rewind to zero, keeping the track loaded. Qt: `stop`.
    pub fn stop(&mut self) {
        self.transport.stop();
        if let Some(out) = &self.output {
            out.player.clear();
        }
    }

    /// Seek to `secs`, clamped to `0..=duration`. Qt: `setPosition`.
    pub fn seek(&mut self, secs: f64) {
        let target = self.transport.seek(secs);
        if let Some(out) = &self.output {
            let _ = out.player.try_seek(target);
        }
    }

    /// Set the output volume over `0.0..=1.0`. Qt: `QAudioOutput::setVolume`.
    pub fn set_volume(&mut self, volume: f32) {
        self.transport.set_volume(volume);
        if let Some(out) = &self.output {
            out.player.set_volume(self.transport.volume());
        }
    }

    /// Current volume, `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        self.transport.volume()
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        self.transport.state()
    }

    /// Current position in seconds.
    pub fn position_secs(&self) -> f64 {
        self.transport.position_secs()
    }

    /// Track duration in seconds (0.0 when unknown or nothing is loaded).
    pub fn duration_secs(&self) -> f64 {
        self.transport.duration_secs()
    }

    /// Advance the clock, clamp it, and detect end of track. The module's
    /// tick system calls this on the UI/ECS thread each woken tick and pushes
    /// the returned snapshot into signals. `ended` is a one-shot edge.
    pub fn refresh(&mut self) -> AudioSnapshot {
        let snap = self.transport.refresh();
        if snap.ended
            && let Some(out) = &self.output
        {
            // The track finished: drop the decoded source. The transport has
            // already flagged the next resume as a rebuild.
            out.player.clear();
        }
        snap
    }

    /// Wire the event-loop waker, so the ticker thread can wake a parked loop
    /// while a track plays. Idempotent; the module's tick system calls it
    /// once the waker resource exists.
    pub fn set_waker(&self, waker: EventLoopWaker) {
        if let Some(ticker) = &self.ticker {
            ticker.set_waker(waker);
        }
    }

    /// Whether a real output device is open. False means the transport works
    /// and there is no sound.
    pub fn has_device(&self) -> bool {
        self.output.is_some()
    }
}

/// Probe the total duration of an encoded audio container (Ogg/Vorbis, WAV)
/// without decoding it.
///
/// rodio's `Source::total_duration()` returns `None` for Ogg/Vorbis because a
/// Vorbis stream carries no up-front sample count: the total is only
/// recoverable from the granule position on the final Ogg page. symphonia's
/// format reader reads that page during probing and populates
/// `codec_params.n_frames`, from which `n_frames / sample_rate` gives the real
/// length. This runs once at load and touches metadata only (it does not pull
/// packets or decode audio).
///
/// Returns `None` when the format is unrecognised or the frame count and sample
/// rate cannot be determined; the caller then keeps `Duration::ZERO`.
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

    // Prefer the codec's time base if present (frames to seconds exactly);
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
