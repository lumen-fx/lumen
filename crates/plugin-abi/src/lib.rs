//! The plumbing under Lumen's plugin systems.
//!
//! A Lumen plugin is a Rust cdylib the compiler or the runtime loads and
//! talks to over a C ABI. The two systems hand each other different things,
//! so each owns its own descriptor and its own hooks; what they share is
//! everything underneath that: the buffer and status codes bytes cross on
//! ([`raw`]), the one bincode call site those bytes go through ([`codec`]),
//! the `[[plugins]]` declarations in `lumen.toml` ([`config`]), the plugin
//! cache and `lumen.lock` (`resolve`), and the loader helpers that open a
//! library and drive a hook (`dlopen`). The last two are the loader's half
//! and sit behind features of the same names.
//!
//! Nothing here knows about the engine or the IR, so both plugin systems can
//! depend on it without depending on each other.

pub mod codec;
pub mod config;
#[cfg(feature = "dlopen")]
pub mod dlopen;
pub mod raw;
#[cfg(feature = "resolve")]
pub mod resolve;
