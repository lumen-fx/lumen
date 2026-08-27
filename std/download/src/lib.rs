//! File downloads for Lumen apps, as a self-contained module.
//!
//! The engine has no download code; this crate is the whole capability.
//! Install [`DownloadPlugin`] and the app gains the `download` namespace, in
//! every host. It holds one function:
//!
//! ```text
//! download::to_file(url, path, tag, checksum) -> bool
//! ```
//!
//! Rhai and candela spell the call `download::to_file(..)`; Lua spells it
//! `download.to_file(..)`. Without the module none of it exists: a script
//! calling `download::to_file` gets its host's ordinary unknown-function
//! error.
//!
//! The call answers as soon as the transfer starts, and the transfer itself
//! reports through three events keyed by the tag you passed:
//! `download_progress`, `download_done`, and `download_error`. A per-tag
//! `on("download_done", tag, fn)` registration wins over the
//! `on_download_done(tag, path)` fallback.
//!
//! Use this rather than `fetch()` when the answer is a file: a large or binary
//! body, a transfer you want progress for, a payload you can verify. `fetch()`
//! hands a script a string under a memory cap, and treats any reply that
//! arrived as a completed fetch; this streams to disk under no size assumption
//! and treats a non-2xx reply as a failed download, because you asked for a
//! file and did not get one.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-download`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-download = { bundled = true }
//!   ```
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`DownloadPlugin`] itself.
//!
//! A relative destination names a file beside the app, wherever the app was
//! started from; downloads an installed app keeps belong under the data
//! directory the `lumen-fs` module answers with.
//!
//! Three settings, all optional:
//!
//! ```toml
//! [dependencies]
//! lumen-download = { bundled = true, config = { timeout_ms = 15000, max_bytes = 1073741824, max_concurrent = 2 } }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod transfer;

#[doc(hidden)]
pub mod testkit;

mod plugin;

pub use plugin::{DEFAULT_MAX_CONCURRENT, DownloadPlugin, MAX_MAX_CONCURRENT, PROGRESS_INTERVAL};

// The bundled-module entry: the loader constructs the shipping plugin from
// the app's `config` table.
#[cfg(not(windows))]
lumen_module::lumen_module!(|config: lumen_module::ModuleConfig| DownloadPlugin::new(config));
