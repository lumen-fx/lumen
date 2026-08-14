//! The transport model, exercised with no device and no decoder.
//!
//! These cover what every backend inherits: the three-state machine, the
//! clock-based playhead, seek clamping, volume clamping, and the one-shot
//! end-of-track edge. Duration is set directly here, because knowing a track's
//! length is a decoder's job and this crate has none.

use lumen_audio::{AudioBackend, NullAudio, PlaybackState, Resume, Transport};
use std::sync::Arc;
use std::time::Duration;

/// A loaded track advances, freezes while paused, and advances again on resume.
#[test]
fn clock_follows_play_pause_resume() {
    let mut t = Transport::default();
    assert_eq!(t.state(), PlaybackState::Stopped, "nothing loaded yet");

    t.start(Duration::from_secs(5));
    assert_eq!(t.state(), PlaybackState::Playing);
    std::thread::sleep(Duration::from_millis(120));
    let advanced = t.position_secs();
    assert!(
        advanced > 0.0,
        "position advanced while playing: {advanced}"
    );

    assert!(t.pause(), "pause acted on a playing transport");
    assert_eq!(t.state(), PlaybackState::Paused);
    let held = t.position_secs();
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        (held - t.position_secs()).abs() < 1e-3,
        "paused position frozen at {held}"
    );
    assert!(!t.pause(), "pause on a paused transport does nothing");

    assert_eq!(
        t.resume(),
        Resume::Continue,
        "a paused source is still live"
    );
    assert_eq!(t.state(), PlaybackState::Playing);
    std::thread::sleep(Duration::from_millis(120));
    assert!(t.position_secs() > held, "resume advanced the position");
}

/// Seeking lands on the requested second and clamps to both ends.
#[test]
fn seek_clamps_to_the_track() {
    let mut t = Transport::default();
    t.start(Duration::from_secs(4));
    t.pause();

    assert_eq!(t.seek(2.5), Duration::from_secs_f64(2.5));
    assert!((t.position_secs() - 2.5).abs() < 1e-6);

    assert_eq!(t.seek(9999.0), Duration::from_secs(4), "clamped to the end");
    assert_eq!(t.seek(-3.0), Duration::ZERO, "clamped to the start");
}

/// Volume is clamped to `0.0..=1.0`.
#[test]
fn volume_clamps() {
    let mut t = Transport::default();
    t.set_volume(0.5);
    assert!((t.volume() - 0.5).abs() < 1e-6);
    t.set_volume(2.0);
    assert!((t.volume() - 1.0).abs() < 1e-6);
    t.set_volume(-1.0);
    assert!(t.volume().abs() < 1e-6);
}

/// A track that runs past its end reports the `ended` edge once, parks the
/// playhead at the end, and asks the next resume to rebuild the source.
#[test]
fn end_of_track_is_a_single_edge() {
    let mut t = Transport::default();
    t.start(Duration::from_millis(80));
    std::thread::sleep(Duration::from_millis(140));

    let snap = t.refresh();
    assert!(snap.ended, "ended edge fired after the track finished");
    assert!(!snap.playing, "not playing after the end");
    assert!(!t.refresh().ended, "the edge does not repeat");
    assert_eq!(t.state(), PlaybackState::Paused, "the track stays loaded");

    assert_eq!(
        t.resume(),
        Resume::Restart,
        "resuming from the end rebuilds the source"
    );
    assert!(t.position_secs() < 0.08, "restarted near zero");
}

/// A stop rewinds, keeps the track loaded, and makes the next resume a rebuild.
#[test]
fn stop_rewinds_and_keeps_the_track() {
    let mut t = Transport::default();
    t.start(Duration::from_secs(3));
    t.stop();

    assert_eq!(
        t.state(),
        PlaybackState::Paused,
        "stop keeps the track loaded"
    );
    assert!(t.position_secs().abs() < 1e-6, "stop rewinds to zero");
    assert_eq!(t.resume(), Resume::Restart);
}

/// Resuming with nothing loaded, or while already playing, is ignored.
#[test]
fn resume_without_a_track_is_ignored() {
    let mut t = Transport::default();
    assert_eq!(t.resume(), Resume::Ignored, "nothing loaded");
    t.start(Duration::from_secs(1));
    assert_eq!(t.resume(), Resume::Ignored, "already playing");
}

/// The silent backend answers the whole trait: it plays, seeks, and holds a
/// volume, and reports that it has no device.
#[test]
fn null_backend_drives_the_transport() {
    let mut audio = NullAudio::default();
    assert!(!audio.has_device());
    assert_eq!(audio.state(), PlaybackState::Stopped);

    let bytes: Arc<[u8]> = Arc::from(vec![0u8; 16]);
    audio
        .play_bytes(bytes)
        .expect("the silent backend accepts any bytes");
    assert_eq!(audio.state(), PlaybackState::Playing);

    audio.pause();
    assert_eq!(audio.state(), PlaybackState::Paused);
    audio.seek(1.0);
    assert!((audio.position_secs() - 1.0).abs() < 1e-6);

    audio.set_volume(0.25);
    assert!((audio.volume() - 0.25).abs() < 1e-6);

    // With no decoder there is no track length, so no track ever ends.
    assert_eq!(audio.duration_secs(), 0.0);
    assert!(!audio.refresh().ended);
}
