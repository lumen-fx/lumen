//! Clock types for the tick loop, animation primitives, and script timers.
//!
//! `std::time::Instant::now` panics on `wasm32-unknown-unknown` and [`crate::tick::Tick::default`]
//! calls it at boot, so this module resolves to `web_time` there and to `std::time` everywhere else.
//! Use `lumen_core::time::{Duration, Instant}` instead of `std::time` in any crate that has to reach
//! the browser.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{
    Duration, Instant, SystemTime, SystemTimeError, TryFromFloatSecsError, UNIX_EPOCH,
};
#[cfg(target_arch = "wasm32")]
pub use web_time::{
    Duration, Instant, SystemTime, SystemTimeError, TryFromFloatSecsError, UNIX_EPOCH,
};
