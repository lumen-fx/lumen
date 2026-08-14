//! Audio loader: reads a track's encoded bytes and probes its container.

use std::sync::Arc;

use crate::{
    AssetKind, AssetLoader, AudioData, LoadContext, LoadErrorKind, LoadedAsset, LoadedAudio,
};

/// Extensions the audio loader claims.
///
/// Only wav and ogg are decodable by the playback layer's current feature
/// set, but the pipeline can carry any of these (an unplayable file surfaces
/// later, on play) so the classification here stays permissive.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "ogg", "oga", "mp3", "flac", "m4a", "aac"];

/// Loads an audio track's encoded bytes into [`AudioData`].
///
/// Audio deliberately does not decode to PCM here: songs are large and the
/// playback layer stream-decodes on the audio thread. What must stay off the
/// UI thread is the read (or bundle fetch) and a cheap container probe, so a
/// truncated or mislabelled file fails here, off-thread and cached, rather
/// than at play time.
pub struct AudioLoader;

impl AssetLoader for AudioLoader {
    fn extensions(&self) -> &[&str] {
        AUDIO_EXTENSIONS
    }

    fn kind(&self) -> AssetKind {
        AssetKind::Audio
    }

    fn load(&self, ctx: &LoadContext<'_>) -> Result<LoadedAsset, LoadErrorKind> {
        let bytes = ctx.read_bytes()?;
        if !audio_magic_ok(&bytes) {
            let path = ctx.path();
            return Err(LoadErrorKind::DecodeFailed(format!(
                "{path:?}: unrecognized audio container (expected RIFF/WAVE or OggS)"
            )));
        }
        let bytes: Arc<[u8]> = Arc::from(bytes.into_owned());
        Ok(LoadedAsset::Audio(LoadedAudio(AudioData { bytes }.into())))
    }
}

/// True when `bytes` starts with a WAV (`RIFF....WAVE`) or Ogg (`OggS`)
/// container header. MP3/FLAC/M4A magic is also accepted permissively so
/// those files can flow through if the decoder features are ever enabled.
fn audio_magic_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    let riff_wave = &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE";
    let ogg = &bytes[0..4] == b"OggS";
    let flac = &bytes[0..4] == b"fLaC";
    let id3 = &bytes[0..3] == b"ID3";
    let mp3_sync = bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0;
    let m4a = &bytes[4..8] == b"ftyp";
    riff_wave || ogg || flac || id3 || mp3_sync || m4a
}

#[cfg(test)]
mod tests {
    use super::audio_magic_ok;

    #[test]
    fn magic_accepts_containers_and_rejects_garbage() {
        assert!(audio_magic_ok(b"RIFF\0\0\0\0WAVEfmt "));
        assert!(audio_magic_ok(b"OggS\0\0\0\0\0\0\0\0"));
        assert!(!audio_magic_ok(b"not audio at all"));
        assert!(!audio_magic_ok(b"RIFFxxxxAVI ")); // RIFF but not WAVE
        assert!(!audio_magic_ok(b"short"));
    }
}
