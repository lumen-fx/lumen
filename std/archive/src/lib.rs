//! Unpacking archives in a Lumen app, as a self-contained module.
//!
//! The engine has no archive code; this crate is the whole capability.
//! Install [`ArchivePlugin`] and the app gains one function in the `archive`
//! namespace, in every host: `archive::extract(src, dest, tag)` in Rhai and
//! candela, `archive.extract(src, dest, tag)` in Lua. It reads zip, tar, and
//! gzip-compressed tar.
//!
//! Without the module none of that exists: a script calling
//! `archive::extract` gets its host's ordinary unknown-function error.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-archive`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-archive = { bundled = true }
//!   ```
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`ArchivePlugin`] itself.
//!
//! Extraction takes as long as the archive is big, so it never happens inside
//! the call. `extract` answers straight away with whether the job was taken,
//! and the result arrives later as an event: `on_archive_done(tag, dest,
//! count)` when the archive is out, `on_archive_error(tag, message)` when it
//! is not. The tag is the key, so a per-job `on("archive_done", tag, fn)`
//! registration wins over the fallback the way it does for every plugin
//! event.
//!
//! What an archive may write is decided before anything is written. An entry
//! naming an absolute path, climbing out with `..`, or resolving outside the
//! destination stops the whole extraction with an error naming it, rather
//! than being passed over: a partly written hostile archive is worse than a
//! failed one. Symbolic and hard links are skipped, because a link inside the
//! destination can point outside it once extraction is over; `count` is the
//! files written, so it does not include them.
//!
//! The one setting is `max_concurrent`, how many extractions may run at once:
//!
//! ```toml
//! [dependencies]
//! lumen-archive = { bundled = true, config = { max_concurrent = 2 } }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod unpack;

#[doc(hidden)]
pub mod testkit;

mod plugin;

pub use plugin::{ArchivePlugin, DEFAULT_MAX_CONCURRENT, MAX_MAX_CONCURRENT};

// The bundled-module entry: the loader constructs the shipping plugin from
// the app's `config` table.
#[cfg(not(windows))]
lumen_module::lumen_module!(|config: lumen_module::ModuleConfig| ArchivePlugin::new(config));
