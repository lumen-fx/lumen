//! The device-independent half of playback: the transport state machine and
//! its clock.
//!
//! Every backend has to answer the same questions - is a track loaded, is it
//! advancing, where is the playhead, how long is the track - and every backend
//! answers them the same way. [`Transport`] holds that answer once so a backend
//! only has to carry the parts that talk to a device: decoding bytes, feeding a
//! sink, and following the transport's decisions.
//!
//! The surface mirrors Qt's `QMediaPlayer` / `QAudioOutput`: the three-state
//! machine, `position` / `duration` in seconds, seek, and a `0.0..=1.0`
//! volume.
//!
//! The position model is clock-based: `position = base_pos + (now - started_at)`
//! while playing, clamped to the duration. A backend that has its own playhead
//! (a platform player, say) can report that instead; [`crate::RodioAudio`]
//! uses the clock, because it behaves the same with or without an output
//! device and so stays meaningful in a headless test.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// A read-only snapshot of the transport, produced by [`Transport::refresh`]
/// on the UI/ECS thread. `ended` is an edge: true only on the single refresh
/// where a playing track reached its end.
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

/// What [`Transport::resume`] asks the backend to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Nothing to do: no track is loaded, or the transport is already playing.
    Ignored,
    /// Play the existing source from where it stands.
    Continue,
    /// The decoded source is gone (a stop, or a track that reached its end).
    /// Rebuild it from the retained bytes and play from the start.
    Restart,
}

/// Playback state machine plus the clock that drives the playhead.
///
/// A backend owns one of these, forwards the control calls into it, and acts on
/// what it returns. The `playing` flag is shared ([`Transport::playing_flag`])
/// so a position ticker can watch it without locking.
pub struct Transport {
    duration: Duration,
    /// Position at the last state change (play/pause/seek boundary).
    base_pos: Duration,
    /// Wall-clock moment playback resumed from `base_pos`; `None` when
    /// paused or stopped.
    started_at: Option<Instant>,
    playing: Arc<AtomicBool>,
    volume: f32,
    loaded: bool,
    /// True when the backend's decoded source has been dropped (a stop, or a
    /// natural end) while a track is still loaded. The next resume rebuilds it.
    cleared: bool,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            duration: Duration::ZERO,
            base_pos: Duration::ZERO,
            started_at: None,
            playing: Arc::new(AtomicBool::new(false)),
            volume: 1.0,
            loaded: false,
            cleared: false,
        }
    }
}

impl Transport {
    /// The shared "is advancing" flag, for a position ticker to poll.
    pub fn playing_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.playing)
    }

    /// Begin a fresh track of `duration`: loaded, playing, playhead at zero.
    pub fn start(&mut self, duration: Duration) {
        self.duration = duration;
        self.base_pos = Duration::ZERO;
        self.started_at = Some(Instant::now());
        self.playing.store(true, Ordering::Release);
        self.loaded = true;
        self.cleared = false;
    }

    /// Hold the playhead where it is. Returns false when nothing was playing,
    /// in which case the backend has nothing to forward to its device.
    pub fn pause(&mut self) -> bool {
        if !self.playing.load(Ordering::Acquire) {
            return false;
        }
        self.base_pos = self.position();
        self.started_at = None;
        self.playing.store(false, Ordering::Release);
        true
    }

    /// Resume a paused track and report what the backend must do about its
    /// source. A track sitting at its end restarts from zero.
    pub fn resume(&mut self) -> Resume {
        if !self.loaded || self.playing.load(Ordering::Acquire) {
            return Resume::Ignored;
        }
        let at_end = self.duration > Duration::ZERO && self.base_pos >= self.duration;
        let action = if self.cleared || at_end {
            self.base_pos = Duration::ZERO;
            self.cleared = false;
            Resume::Restart
        } else {
            Resume::Continue
        };
        self.started_at = Some(Instant::now());
        self.playing.store(true, Ordering::Release);
        action
    }

    /// Stop and rewind to zero, keeping the track loaded. The backend drops its
    /// decoded source; the next resume comes back as [`Resume::Restart`].
    pub fn stop(&mut self) {
        self.base_pos = Duration::ZERO;
        self.started_at = None;
        self.playing.store(false, Ordering::Release);
        self.cleared = true;
    }

    /// Move the playhead to `secs`, clamped to `0..=duration`. Returns the
    /// clamped target so the backend can seek its source to the same place.
    /// Qt: `setPosition`.
    pub fn seek(&mut self, secs: f64) -> Duration {
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
        target
    }

    /// Set the output volume, clamped to `0.0..=1.0`. Read the stored value
    /// back with [`Self::volume`] to apply it to a device. Qt:
    /// `QAudioOutput::setVolume`.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Current volume, `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Replace the known track length. A backend that rebuilds its source calls
    /// this with the length the fresh decoder reports.
    pub fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
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

    /// Whether the transport is advancing.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    /// Clock-based playhead, clamped to the duration.
    pub fn position(&self) -> Duration {
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

    /// Playhead in seconds.
    pub fn position_secs(&self) -> f64 {
        self.position().as_secs_f64()
    }

    /// Track length in seconds (0.0 when unknown or nothing is loaded).
    pub fn duration_secs(&self) -> f64 {
        self.duration.as_secs_f64()
    }

    /// Advance the clock, clamp it, and detect end of track. When the returned
    /// snapshot has `ended` set, the backend drops its decoded source; the
    /// transport has already flagged the next resume as a restart.
    pub fn refresh(&mut self) -> AudioSnapshot {
        let mut ended = false;
        if self.playing.load(Ordering::Acquire)
            && self.duration > Duration::ZERO
            && self.position() >= self.duration
        {
            self.base_pos = self.duration;
            self.started_at = None;
            self.playing.store(false, Ordering::Release);
            self.cleared = true;
            ended = true;
        }
        AudioSnapshot {
            position: self.position().as_secs_f64(),
            duration: self.duration.as_secs_f64(),
            playing: self.playing.load(Ordering::Acquire),
            ended,
        }
    }
}
