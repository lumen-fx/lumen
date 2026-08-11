//! Generate the committed test soundtracks as 16-bit PCM WAV files.
//!
//! Run: `cargo run -p lumen-audio --bin lumen-gen-test-tracks -- <out_dir>`
//! (defaults to `apps/music/assets`). The output files are checked into
//! the repo so `apps/music` plays real audio without a generation step;
//! this binary exists to reproduce them deterministically.

use lumen_audio::synth;
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/music/assets"));
    std::fs::create_dir_all(&out)?;

    for track in synth::all_tracks() {
        let path = out.join(format!("{}.wav", track.stem));
        synth::write_wav(&path, &track.samples)?;
        let secs = track.samples.len() as f32 / synth::SAMPLE_RATE as f32;
        println!(
            "wrote {} ({:.2}s, {} samples) - {}",
            path.display(),
            secs,
            track.samples.len(),
            track.title
        );
    }
    Ok(())
}
