# Waveform - Music player

A desktop player shell: a selectable playlist sidebar, a scrolling library
table, and a now-playing bar whose transport drives audio through the
`lumen-audio` module the app declares in its `lumen.toml`; the module is
what gives the script its `audio_*` functions.

## Run

```
lumenc run apps/music
```

Click a library row (or press Play) to hear a track. On a machine with a
working audio device you hear the track; on a deviceless box playback runs
silent and the transport UI still works. The `audio_*` functions come from
the `lumen-audio` module, so a source-built (`cargo run -p lumenc`) binary,
which compiles the engine in and cannot load modules, runs the app without
its audio surface.

## Soundtracks

The default Featured playlist streams three royalty-free tracks by
Kevin MacLeod (incompetech.com), shipped as OGG Vorbis via Git LFS and
licensed under CC BY 4.0 (see [`CREDITS.md`](CREDITS.md)):

| file | track |
| --- | --- |
| `carefree.ogg` | Carefree - 3:25 |
| `wallpaper.ogg` | Wallpaper - 3:40 |
| `cipher.ogg` | Cipher - 3:51 |

The Pure Tones and Moving Tones playlists hold four short PCM WAV tracks
generated in-repo, which exercise the pipeline end to end without shipping
copyrighted music:

| file | playlist | sound |
| --- | --- | --- |
| `tone-a440.wav` | Pure Tones | 3 s 440 Hz reference sine |
| `low-pulse.wav` | Pure Tones | 2.5 s 110 Hz tremolo pulse |
| `rising-sweep.wav` | Moving Tones | 4 s 220->880 Hz sweep |
| `major-triad.wav` | Moving Tones | 3 s C-E-G major chord |

Regenerate them with:

```
cargo run -p lumen-audio --bin lumen-gen-test-tracks -- apps/music/assets
```

## What it demonstrates

- **Audio transport** - the play/pause button drives `audio_pause` and
  `audio_resume`, prev/next call `audio_play` on the adjacent track, and the
  seek slider drives `audio_seek`. The module writes `audio_position`,
  `audio_duration`, and `audio_playing` into signals on every woken tick;
  there is no per-frame poll, and a paused player goes idle. The script
  `derive()`s the seek percentage, elapsed and total time, and the play glyph
  from them. When a track ends the module calls `on_audio_end(path)`, which
  is what advances the playlist.
- **Library table built through the DOM API** - `src/main.lmn` ships an empty
  `#playlist` container and the script fills it element by element with
  `node_spawn` and `node_append`, one row per track, rebuilt when the playlist
  changes. The playing row gets a highlight class on each rebuild.
- **Slider seek and volume** - two `<slider>`s; the seek bar scrubs playback,
  the volume bar drives `audio_volume`.
- **Play / pause state** - a toggle button whose label (`Play` / `Pause`)
  comes from a `derive()` chain rooted at the host `audio_playing` signal.
- **Playlist selection** - sidebar buttons rebuild the library from the
  selected playlist and flip an active class with `set_class`.
- **Gradient album art** - the now-playing tile is a `conic-gradient`, no
  bitmap asset.

## Credits

The Featured playlist is music by Kevin MacLeod, licensed under CC BY 4.0.
Full attribution and source links are in [`CREDITS.md`](CREDITS.md).

## Design

Deep teal-charcoal with a coral now-playing accent. The library reads as a
quiet table; the transport bar carries the one bright color. Text-only
transport labels; no glyph fonts required.
