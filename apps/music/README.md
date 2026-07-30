# Waveform - Music player

A desktop player shell: a selectable playlist sidebar, a scrolling library
table, and a now-playing bar with a **real** audio transport backed by
`lumen-audio` (rodio).

## Run

```
cargo run -p lumenc -- run apps/music
```

Click a library row (or press Play) to hear a track. Playback is real: on a
machine with a working audio device you will hear the tone; on a headless /
deviceless box `lumen-audio` degrades to a silent null sink and the transport
UI still works.

## Soundtracks

The default **Featured** playlist streams three real royalty-free tracks by
Kevin MacLeod (incompetech.com), shipped as OGG Vorbis via Git LFS and
licensed under CC BY 4.0 (see [`CREDITS.md`](CREDITS.md)):

| file | track |
| --- | --- |
| `carefree.ogg` | Carefree - 3:25 |
| `wallpaper.ogg` | Wallpaper - 3:40 |
| `cipher.ogg` | Cipher - 3:51 |

The **Pure** and **Moving** playlists keep four short, self-generated PCM WAV
tracks - honest, decodable audio used to prove the pipeline end to end without
shipping copyrighted music:

| file | sound |
| --- | --- |
| `tone-a440.wav` | 3 s 440 Hz reference sine |
| `major-triad.wav` | 3 s C-E-G major chord |
| `rising-sweep.wav` | 4 s 220->880 Hz sweep |
| `low-pulse.wav` | 2.5 s 110 Hz tremolo pulse |

Regenerate them with:

```
cargo run -p lumen-audio --bin lumen-gen-test-tracks -- apps/music/assets
```

## What it demonstrates

- **Real audio transport** - the play/pause button drives
  `audio_pause`/`audio_resume`, prev/next call `audio_play` on the adjacent
  track, and the seek slider drives `audio_seek`. The host writes
  `audio_position` / `audio_duration` / `audio_playing` into signals every
  woken tick (reactively - no per-frame poll; paused = idle), and the script
  `derive()`s the seek percentage, elapsed / total time, and the play glyph
  from them.
- **Asset-pipeline audio loading** - tracks load through the same async
  `AssetServer` as images: `audio_play("assets/x.wav")` sets an `AudioSource`
  on a player entity, the worker pool reads + caches the bytes off-thread, and
  playback starts when the `LoadedAudio` handle resolves.
- **Library table via `<for>`** - one row per track, rebuilt when the playlist
  changes. The playing row gets a highlight class recomputed on each rebuild.
- **Slider seek + volume** - two `<slider>`s; the seek bar scrubs real audio,
  the volume bar drives `audio_volume`.
- **Play / pause state** - a toggle button whose label (`Play` / `Pause`)
  comes from a `derive()` chain rooted at the host `audio_playing` signal.
- **Playlist selection** - sidebar buttons swap the library ArraySignal and
  flip an active class with `set_class`.
- **Gradient album art** - the now-playing tile is a `conic-gradient`, no
  bitmap asset.

## Credits

The Featured playlist is music by Kevin MacLeod, licensed under CC BY 4.0.
Full attribution and source links are in [`CREDITS.md`](CREDITS.md).

## Design

Deep teal-charcoal with a coral now-playing accent. The library reads as a
quiet table; the transport bar carries the one bright color. Text-only
transport labels - no glyph fonts required.
