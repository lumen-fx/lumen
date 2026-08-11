//! Tiny offline synthesizer used to generate the committed test tracks.
//!
//! These are honest, self-generated, fully decodable PCM WAV files - the
//! correct way to prove the audio pipeline end-to-end without shipping
//! copyrighted music. Each generator returns mono `f32` samples in
//! `-1.0..=1.0`; [`write_wav`] encodes them as 16-bit PCM WAV.

use std::f32::consts::TAU;
use std::io::{self, Write};
use std::path::Path;

/// Sample rate of the generated tracks.
pub const SAMPLE_RATE: u32 = 44_100;

/// Peak amplitude - comfortably below clipping and easy on the ears.
const AMP: f32 = 0.28;

/// One generated track: a stable id/filename stem plus its samples.
pub struct Track {
    /// Filename stem (no extension), also the library row id.
    pub stem: &'static str,
    /// Human title for the library table.
    pub title: &'static str,
    /// Mono samples in `-1.0..=1.0`.
    pub samples: Vec<f32>,
}

/// The full set of committed test tracks. Distinct timbres so an author
/// can tell by ear which one is playing.
pub fn all_tracks() -> Vec<Track> {
    vec![
        Track {
            stem: "tone-a440",
            title: "Reference Tone A (440 Hz)",
            samples: sine(440.0, 3.0),
        },
        Track {
            stem: "major-triad",
            title: "Major Triad Chord",
            samples: chord(&[261.63, 329.63, 392.00], 3.0),
        },
        Track {
            stem: "rising-sweep",
            title: "Rising Sweep 220 to 880",
            samples: sweep(220.0, 880.0, 4.0),
        },
        Track {
            stem: "low-pulse",
            title: "Low Pulse 110 Hz",
            samples: pulse(110.0, 2.5),
        },
    ]
}

/// Generates `secs` seconds of samples at [`SAMPLE_RATE`]. `sample` receives
/// the sample index `i`, the total count `n`, and the time `t = i / rate`, and
/// returns the final sample value (envelope included). Centralises the
/// `n` / `t` / `collect` scaffolding shared by every generator while leaving
/// each timbre's arithmetic expression untouched.
fn generate(secs: f32, mut sample: impl FnMut(usize, usize, f32) -> f32) -> Vec<f32> {
    let n = (SAMPLE_RATE as f32 * secs) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            sample(i, n, t)
        })
        .collect()
}

/// A steady sine tone.
pub fn sine(freq: f32, secs: f32) -> Vec<f32> {
    generate(secs, move |i, n, t| {
        envelope(i, n) * AMP * (TAU * freq * t).sin()
    })
}

/// A sum of sine partials (a chord), normalized so the peak stays at `AMP`.
pub fn chord(freqs: &[f32], secs: f32) -> Vec<f32> {
    let scale = AMP / freqs.len() as f32;
    generate(secs, move |i, n, t| {
        let s: f32 = freqs.iter().map(|f| (TAU * f * t).sin()).sum();
        envelope(i, n) * scale * s
    })
}

/// A linear frequency sweep from `start` to `end` Hz.
pub fn sweep(start: f32, end: f32, secs: f32) -> Vec<f32> {
    // Instantaneous freq f(t) = start + k t; phase is its integral.
    let k = (end - start) / secs;
    generate(secs, move |i, n, t| {
        let phase = TAU * (start * t + 0.5 * k * t * t);
        envelope(i, n) * AMP * phase.sin()
    })
}

/// A soft amplitude-pulsed tone (2 Hz tremolo) - a distinct rhythmic timbre.
pub fn pulse(freq: f32, secs: f32) -> Vec<f32> {
    generate(secs, move |i, n, t| {
        let trem = 0.5 + 0.5 * (TAU * 2.0 * t).sin();
        envelope(i, n) * AMP * trem * (TAU * freq * t).sin()
    })
}

/// 10 ms linear attack + release to keep the start/end free of clicks.
fn envelope(i: usize, n: usize) -> f32 {
    let ramp = (SAMPLE_RATE as f32 * 0.010) as usize;
    if ramp == 0 || n == 0 {
        return 1.0;
    }
    if i < ramp {
        i as f32 / ramp as f32
    } else if i >= n.saturating_sub(ramp) {
        (n - i) as f32 / ramp as f32
    } else {
        1.0
    }
}

/// Encode mono `f32` samples as a 16-bit PCM WAV file at `path`.
pub fn write_wav(path: &Path, samples: &[f32]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    let sr = SAMPLE_RATE;
    let byte_rate = sr * 2; // mono, 2 bytes/sample
    let block_align: u16 = 2;
    let bits: u16 = 16;

    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    buf.extend_from_slice(&sr.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let v = (clamped * i16::MAX as f32) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }

    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}
