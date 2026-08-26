//! Audio playback for Lumen apps, as a self-contained module.
//!
//! The engine has no audio code; this crate is the whole capability. Install
//! [`AudioPlugin`] and the app gains:
//!
//! - the `audio_play` / `audio_pause` / `audio_resume` / `audio_stop` /
//!   `audio_seek` / `audio_volume` script functions, in every host;
//! - the `audio_position` / `audio_duration` / `audio_playing` signals,
//!   written every woken tick while a track plays;
//! - the `on_audio_end(path)` script event when a track reaches its end,
//!   with a per-track `on("audio_end", path, fn)` registration winning over
//!   the fallback.
//!
//! Without the module none of that exists: a script calling `audio_play`
//! gets its host's ordinary unknown-function error.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-audio`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-audio = { bundled = true }
//!   ```
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`AudioPlugin`] itself.
//!
//! Playback is rodio over a cpal output device, decoding wav and ogg. A
//! machine with no output device (CI, SSH) degrades to the silent transport:
//! state, seeking, position, and duration all still work.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod synth;

mod backend;
mod plugin;
mod ticker;
mod transport;

pub use backend::{AudioError, RodioAudio};
pub use plugin::AudioPlugin;
pub use ticker::{PositionTicker, TICK_INTERVAL};
pub use transport::{AudioSnapshot, PlaybackState, Resume, Transport};

// The bundled-module entry: the loader constructs the shipping plugin. The
// deviceless shape is not reachable from module config on purpose - an app
// that declares the module wants sound, and a machine without a device
// already degrades to silent.
#[cfg(not(windows))]
lumen_module::lumen_module!(|_config: lumen_module::ModuleConfig| AudioPlugin::new());
