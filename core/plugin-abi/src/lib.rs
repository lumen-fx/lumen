//! The plumbing under Lumen's plugin systems.
//!
//! A Lumen plugin is a Rust cdylib the compiler or the runtime loads and
//! talks to over a C ABI. The two systems hand each other different things,
//! so each owns its own descriptor and its own hooks; what they share is
//! everything underneath that: the buffer and status codes bytes cross on
//! ([`raw`]), the one bincode call site those bytes go through ([`codec`]),
//! the `[[plugins]]` declarations in `lumen.toml` ([`config`]), and the
//! loader helpers that open a library and drive a hook (`dlopen`). The last
//! one is the loader's half and sits behind a feature of the same name.
//!
//! Nothing here resolves a `version` source. `lpm` does that, and `lumenc`
//! hands the answer to whichever loader needs it.
//!
//! Nothing here knows about the engine or the IR, so both plugin systems can
//! depend on it without depending on each other.

pub mod codec;
pub mod config;
#[cfg(feature = "dlopen")]
pub mod dlopen;
pub mod raw;
