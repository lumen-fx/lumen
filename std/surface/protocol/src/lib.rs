//! The Lumen surface protocol: how a renderer in another process shows its
//! frames in a Lumen `<surface>` element with no copy.
//!
//! The producer (a game, a video decoder, any renderer) allocates a small
//! ring of GPU images, exports them as OS handles, and hands them over once;
//! from then on it renders into a free slot, waits for its own GPU, and
//! announces the frame. The consumer (Lumen) imports the images into its own
//! device and shows the newest frame in place. Input travels back the other
//! way. The specification, `SPEC.md` beside this crate, is the contract;
//! this crate implements it for both roles:
//!
//! - [`wire`]: every message and its byte layout;
//! - [`endpoint`]: the one-line string that tells a producer where to
//!   connect;
//! - [`keys`]: the key-code table key inputs are numbered from;
//! - [`transport`]: the platform's transport: [`unix`] sockets (with
//!   descriptors on Linux, Mach ports beside them on macOS) or Windows named
//!   pipes;
//! - [`producer`]: the producer's bookkeeping (pacing, slot reuse, events);
//! - [`consumer`]: the consumer's handshake checks;
//! - [`ffi`]: the producer role as a C ABI (`lumen_surface_protocol.h`), for
//!   an engine written in C or C++.
//!
//! Nothing here touches a graphics API: the engine exports and renders with
//! its own code.

#![warn(missing_docs)]

pub mod consumer;
pub mod endpoint;
#[cfg(any(unix, windows))]
pub mod ffi;
pub mod keys;
#[cfg(target_os = "macos")]
pub mod mach;
#[cfg(windows)]
pub mod pipe;
#[cfg(any(unix, windows))]
pub mod producer;
pub mod transport;
#[cfg(unix)]
pub mod unix;
pub mod wire;

/// Now on the machine's monotonic clock, in nanoseconds: the clock every
/// timestamp in the protocol is on, the same for both processes
/// (`CLOCK_MONOTONIC` on Linux and macOS).
#[cfg(unix)]
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

/// Now on the machine's monotonic clock, in nanoseconds: the clock every
/// timestamp in the protocol is on, the same for both processes
/// (`QueryPerformanceCounter` on Windows).
#[cfg(windows)]
pub fn monotonic_ns() -> u64 {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let (mut count, mut frequency) = (0i64, 0i64);
    // SAFETY: each call writes one i64; neither fails on Windows XP or later.
    unsafe {
        QueryPerformanceCounter(&mut count);
        QueryPerformanceFrequency(&mut frequency);
    }
    let ns = (count.max(0) as u128) * 1_000_000_000 / (frequency.max(1) as u128);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// Now on a clock that only moves forward, in nanoseconds. A platform with no
/// transport has no clock shared with another process, so this one counts
/// from the first call.
#[cfg(not(any(unix, windows)))]
pub fn monotonic_ns() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
