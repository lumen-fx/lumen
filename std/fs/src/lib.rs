//! Filesystem access for Lumen apps, as a self-contained module.
//!
//! The engine has no file code; this crate is the whole capability. Install
//! [`FsPlugin`] and the app gains the `files` namespace, in every host:
//! `exists`, `is_dir`, `list`, `mkdir`, `remove`, `copy`, `read`, `write`,
//! `read_bytes`, `write_bytes`, and `data_dir`. Rhai and candela spell a call
//! `files::read(..)`; Lua spells it `files.read(..)`.
//!
//! The namespace is `files` rather than `fs` because candela reserves `fs`
//! for its own standard filesystem library, which resolves paths against the
//! process working directory. One name has to work in every host, and this is
//! the one that does.
//!
//! Without the module none of that exists: a script calling `files::read` gets
//! its host's ordinary unknown-function error.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-fs`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-fs = { bundled = true }
//!   ```
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`FsPlugin`] itself.
//!
//! A relative path names a file beside the app, wherever the app was started
//! from; saved state belongs under `files::data_dir()`. A call that cannot do
//! what it was asked answers false or empty and explains itself in one
//! `lumen-fs:` line on stderr, so a script branches on the value instead of
//! catching an error.
//!
//! The one setting is `read_bytes_cap`, the largest file `files::read_bytes`
//! hands back:
//!
//! ```toml
//! [dependencies]
//! lumen-fs = { bundled = true, config = { read_bytes_cap = 33554432 } }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ops;

mod plugin;

pub use plugin::{DEFAULT_READ_BYTES_CAP, FsPlugin, MAX_READ_BYTES_CAP, MIN_READ_BYTES_CAP};

// The bundled-module entry: the loader constructs the shipping plugin from
// the app's `config` table.
#[cfg(not(windows))]
lumen_module::lumen_module!(|config: lumen_module::ModuleConfig| FsPlugin::new(config));
