//! Headless pipeline tests - no speakers required.
//!
//! These prove the backend end-to-end without asserting audible output:
//! we synthesize a WAV, decode it back (asserting sample count / duration),
//! then drive the [`RodioAudio`] state machine (play/pause/seek/stop) and
//! assert the clock-based position advances and transitions are correct.
//! They pass whether or not a real output device exists.

use lumen_audio::synth;
use lumen_audio::{AudioBackend, PlaybackState};
use lumen_audio_rodio::RodioAudio;
use std::io::Read;
use std::time::Duration;

/// Device-less backend for the suite.
///
/// Nothing here needs an output device: the assertions cover decoding,
/// duration, transport state, and the clock, all of which run without one.
/// Opening one is what makes the suite unportable, because a machine with no
/// audio endpoint is not guaranteed to report that cleanly; on a Windows CI
/// runner the WASAPI backend faults the process instead. [`RodioAudio::disabled`]
/// is the same device-less shape [`RodioAudio::new`] falls back to.
fn null_audio() -> RodioAudio {
    RodioAudio::disabled()
}

/// Parse the 16-bit-PCM data-chunk length back out of a generated WAV and
/// confirm it matches the synthesized sample count.
#[test]
fn synth_wav_roundtrips_sample_count() {
    let secs = 1.0_f32;
    let samples = synth::sine(440.0, secs);
    let expected = (synth::SAMPLE_RATE as f32 * secs) as usize;
    assert_eq!(samples.len(), expected, "sample count matches duration");

    let dir = std::env::temp_dir().join(format!("lumen-audio-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    synth::write_wav(&path, &samples).unwrap();

    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    // RIFF header sanity.
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    // Total file = 44-byte header + 2 bytes/sample (mono, 16-bit).
    assert_eq!(bytes.len(), 44 + samples.len() * 2, "wav data length");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A freshly generated WAV decodes and its measured duration matches the
/// synthesized length - proving load/decode works via the real codec path.
#[test]
fn play_reports_expected_duration() {
    let dir = std::env::temp_dir().join(format!("lumen-audio-dur-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("triad.wav");
    synth::write_wav(&path, &synth::chord(&[261.63, 329.63, 392.0], 2.0)).unwrap();

    let mut audio = null_audio();
    audio.play(&path).expect("decode + load 2s wav");
    let dur = audio.duration_secs();
    assert!(
        (dur - 2.0).abs() < 0.05,
        "decoded duration ~2s, got {dur} (device present: {})",
        audio.has_device()
    );
    assert_eq!(audio.state(), PlaybackState::Playing);

    let _ = std::fs::remove_dir_all(&dir);
}

/// An Ogg/Vorbis track reports a real, non-zero total duration.
///
/// This is the regression guard for the "blank total time" bug: rodio's
/// `Source::total_duration()` returns `None` for Vorbis, so before the
/// symphonia probe the duration collapsed to 0 and the seek bar's total /
/// max went blank. The committed fixture is a self-generated 1.5 s 440 Hz
/// sine encoded as Vorbis (`tests/fixtures/tone-1500ms.ogg`).
#[test]
fn ogg_reports_nonzero_duration() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone-1500ms.ogg");

    let mut audio = null_audio();
    audio.play(&path).expect("decode + load ogg/vorbis");
    let dur = audio.duration_secs();
    assert!(
        (dur - 1.5).abs() < 0.05,
        "ogg duration probed ~1.5s, got {dur} (device present: {})",
        audio.has_device()
    );
    assert_eq!(audio.state(), PlaybackState::Playing);
}

/// With a correct non-zero Ogg duration, seeking to the app's "half-way"
/// target lands near the midpoint - not snapped back to 0.
///
/// This mirrors `apps/music/main.rhai`'s seek math: the slider computes the
/// target as `(fraction) * audio_duration`. When the duration was 0 (the
/// bug), every target was `0.5 * 0 = 0` and the thumb snapped to the start.
/// Proving the target lands mid-track covers both the duration probe and the
/// downstream seek-target correctness in one assertion.
#[test]
fn ogg_seek_to_midpoint_lands_mid_track() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone-1500ms.ogg");

    let mut audio = null_audio();
    audio.play(&path).expect("decode + load ogg/vorbis");

    // Pause before seeking so `position_secs()` reports the static seek
    // target rather than target-plus-elapsed: a *playing* track keeps the
    // wall clock running, so under load the position drifts past the
    // tolerance band (flaky). Pausing freezes `base_pos`, which is exactly
    // what a scrubbed slider reads. The seek-target correctness this test
    // guards is unaffected by play/pause state.
    audio.pause();

    // App-side math: fraction * duration. With a real duration this is a
    // real target; with the old duration-0 bug it would collapse to 0.
    let dur = audio.duration_secs();
    let target = 0.5 * dur;
    audio.seek(target);

    let pos = audio.position_secs();
    assert!(
        (pos - 0.75).abs() < 0.05,
        "seek to 50% of a 1.5s track lands ~0.75s, got {pos} (target {target})"
    );
    assert!(pos > 0.1, "seek did not snap back to the start: {pos}");
}

/// The state machine + clock-based position behave under play/pause/seek/
/// stop regardless of whether an output device exists.
#[test]
fn transport_state_machine_and_position() {
    let dir = std::env::temp_dir().join(format!("lumen-audio-sm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sweep.wav");
    synth::write_wav(&path, &synth::sweep(220.0, 880.0, 5.0)).unwrap();

    let mut audio = null_audio();
    assert_eq!(audio.state(), PlaybackState::Stopped, "nothing loaded yet");

    audio.play(&path).unwrap();
    assert_eq!(audio.state(), PlaybackState::Playing);

    // Position advances while playing.
    std::thread::sleep(Duration::from_millis(120));
    let p1 = audio.position_secs();
    assert!(p1 > 0.0, "position advanced while playing: {p1}");

    // Pause holds the position.
    audio.pause();
    assert_eq!(audio.state(), PlaybackState::Paused);
    let held = audio.position_secs();
    std::thread::sleep(Duration::from_millis(120));
    let still = audio.position_secs();
    assert!(
        (held - still).abs() < 1e-3,
        "paused position frozen: {held} vs {still}"
    );

    // Resume advances again.
    audio.resume();
    assert_eq!(audio.state(), PlaybackState::Playing);
    std::thread::sleep(Duration::from_millis(120));
    assert!(audio.position_secs() > still, "resume advanced position");

    // Seek jumps and clamps.
    audio.seek(3.0);
    let seeked = audio.position_secs();
    assert!(
        (seeked - 3.0).abs() < 0.05,
        "seek landed near 3s, got {seeked}"
    );
    audio.seek(9999.0);
    assert!(
        audio.position_secs() <= audio.duration_secs() + 1e-6,
        "seek clamps to duration"
    );

    // Stop rewinds and unloads-to-stopped semantics (position 0).
    audio.stop();
    assert_eq!(audio.state(), PlaybackState::Paused, "stopped keeps loaded");
    assert!(audio.position_secs().abs() < 1e-6, "stop rewinds to 0");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Volume is clamped to `0.0..=1.0`.
#[test]
fn volume_clamps() {
    let mut audio = null_audio();
    audio.set_volume(0.5);
    assert!((audio.volume() - 0.5).abs() < 1e-6);
    audio.set_volume(2.0);
    assert!((audio.volume() - 1.0).abs() < 1e-6);
    audio.set_volume(-1.0);
    assert!(audio.volume().abs() < 1e-6);
}

/// A short track played and left running past its end reports the `ended`
/// edge exactly once via `refresh`, and the transport returns to a
/// non-playing state.
#[test]
fn natural_end_edge() {
    let dir = std::env::temp_dir().join(format!("lumen-audio-end-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("blip.wav");
    // 100 ms track.
    synth::write_wav(&path, &synth::sine(440.0, 0.1)).unwrap();

    let mut audio = null_audio();
    audio.play(&path).unwrap();
    std::thread::sleep(Duration::from_millis(180));

    let snap = audio.refresh();
    assert!(snap.ended, "ended edge fired after track finished");
    assert!(!snap.playing, "not playing after end");
    // Edge is one-shot.
    let snap2 = audio.refresh();
    assert!(!snap2.ended, "ended edge does not repeat");

    let _ = std::fs::remove_dir_all(&dir);
}

/// After a track ends naturally (`refresh` cleared the sink), `resume()`
/// must re-arm a real, playing transport from position 0 - not leave a
/// phantom clock climbing over a silent/empty sink. Observable without a device:
/// the load path re-runs (duration re-decoded, position resets, state
/// Playing), and the restarted track ends again.
#[test]
fn resume_after_natural_end_replays() {
    let dir = std::env::temp_dir().join(format!("lumen-audio-reend-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("blip.wav");
    // 100 ms track.
    synth::write_wav(&path, &synth::sine(440.0, 0.1)).unwrap();

    let mut audio = null_audio();
    audio.play(&path).unwrap();
    let dur0 = audio.duration_secs();
    std::thread::sleep(Duration::from_millis(180));
    let snap = audio.refresh();
    assert!(snap.ended, "track ended");
    assert!(
        (audio.position_secs() - dur0).abs() < 0.05,
        "position parked at end: {} vs {dur0}",
        audio.position_secs()
    );

    // Resume from the ended state.
    audio.resume();
    assert_eq!(
        audio.state(),
        PlaybackState::Playing,
        "resume from end re-enters Playing"
    );
    // Load path re-ran: duration recomputed to the same value from the
    // retained bytes (a phantom-clock resume would have left it stale, but
    // more importantly the source is now real again).
    assert!(
        (audio.duration_secs() - dur0).abs() < 0.05,
        "duration re-decoded on resume: {} vs {dur0}",
        audio.duration_secs()
    );
    // Position restarted from ~0, not stuck at the end.
    assert!(
        audio.position_secs() < dur0,
        "resume restarts near 0, got {}",
        audio.position_secs()
    );

    // The restarted track reaches its end again (proves the transport is
    // genuinely advancing a loaded track, not idling).
    std::thread::sleep(Duration::from_millis(180));
    assert!(audio.refresh().ended, "restarted track ends again");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `stop()` clears the sink but keeps the track loaded (Paused semantics).
/// A subsequent `resume()` must rebuild the source and play from 0 rather
/// than seek an empty sink. Observable without a device: state returns to Playing,
/// position advances from 0, duration is preserved via the retained bytes.
#[test]
fn resume_after_stop_replays() {
    let dir = std::env::temp_dir().join(format!("lumen-audio-restop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    synth::write_wav(&path, &synth::sine(440.0, 1.0)).unwrap();

    let mut audio = null_audio();
    audio.play(&path).unwrap();
    let dur0 = audio.duration_secs();

    audio.stop();
    // Preserved contract: stop keeps the track loaded -> Paused, position 0.
    assert_eq!(audio.state(), PlaybackState::Paused, "stop keeps loaded");
    assert!(audio.position_secs().abs() < 1e-6, "stop rewinds to 0");

    audio.resume();
    assert_eq!(
        audio.state(),
        PlaybackState::Playing,
        "resume after stop re-enters Playing"
    );
    assert!(
        (audio.duration_secs() - dur0).abs() < 0.05,
        "duration preserved across stop+resume: {} vs {dur0}",
        audio.duration_secs()
    );
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        audio.position_secs() > 0.0,
        "resume after stop advances position: {}",
        audio.position_secs()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
