//! The C ABI a runtime plugin exports and the engine loads.
//!
//! Everything that crosses the boundary is bytes and C scalars: the init
//! context, the manifest a plugin answers with, and every call travel as
//! bincode buffers (see [`crate::codec`]), never as Rust types, so a plugin
//! built by any compiler works against any engine built from the same release
//! tag. Compatibility is enforced by the handshake fields on [`Desc`], not by
//! hoping both sides agree.
//!
//! [`Desc`] and [`HostVtable`] are what make this the *runtime's* ABI; the
//! buffer, the hook signature, and the status codes below come from
//! `lumen_plugin_abi::raw` and mean the same thing at every Lumen plugin
//! boundary.
//!
//! # Threading
//!
//! [`Desc::init`], [`Desc::call`], and [`Desc::shutdown`] run on the engine's
//! tick thread, one at a time; the engine never calls into a plugin from a
//! background thread and never re-enters it from inside a host callback. The
//! [`HostVtable`] entries go the other way and have the opposite contract:
//! they are callable from any thread at any time, including while a call is
//! running, which is what lets a plugin own a worker thread.

use std::ffi::c_void;
use std::os::raw::c_char;

pub use lumen_plugin_abi::raw::{
    Buf, ERR, FLAG_PANIC_ABORT, FreeFn, HookFn, OK, PANICKED, UNCHANGED,
};

/// Version of this descriptor layout and of the SDK-owned wire shapes.
/// Bump on any change to [`Desc`], [`HostVtable`], [`Buf`], the status codes,
/// or any serde type this crate sends across the boundary (`InitCx`,
/// `Manifest`, `Call`, `CallOut`); the `lumen_script` and `lumen_core::paint`
/// types - `PluginEvent` among them - are covered by
/// [`Desc::script_wire_version`] and [`Desc::paint_wire_version`] instead.
pub const ABI_VERSION: u32 = 1;

/// The one symbol a plugin cdylib exports:
/// `unsafe extern "C" fn lumen_plugin_v1() -> *const Desc`.
pub const ENTRY: &[u8] = b"lumen_plugin_v1\0";

/// Bring a plugin up: hand it the init context and the host callbacks, and
/// take back its manifest.
///
/// `ctx` is a bincode [`InitCx`](crate::InitCx) borrowed for the call. `host`
/// points at a [`HostVtable`] that lives for the process. On [`OK`], `out`
/// holds a bincode [`Manifest`](crate::Manifest); on [`ERR`] or [`PANICKED`]
/// it holds a UTF-8 message.
pub type InitFn = unsafe extern "C" fn(
    ctx: *const u8,
    ctx_len: usize,
    host: *const HostVtable,
    out: *mut Buf,
) -> i32;

/// The descriptor a plugin's entry function returns. Lives for the process;
/// the host reads it after the handshake below.
///
/// `abi_version` and `struct_size` sit first and their offsets are frozen
/// forever: the host reads them before it trusts anything else about the
/// struct, so a future layout can still be refused with a clear error.
#[repr(C)]
pub struct Desc {
    /// Must equal [`ABI_VERSION`].
    pub abi_version: u32,
    /// `size_of::<Desc>()` on the plugin side; a shorter value than the
    /// host's own means a truncated or foreign struct.
    pub struct_size: u32,
    /// `lumen_script::SCRIPT_WIRE_VERSION` the plugin was built against. The
    /// values, commands, and signatures it exchanges are only meaningful when
    /// it matches the host's.
    pub script_wire_version: u16,
    /// `lumen_core::paint::PAINT_WIRE_VERSION` the plugin was built against.
    pub paint_wire_version: u16,
    /// Bit set of [`FLAG_PANIC_ABORT`] and future flags; zero otherwise.
    pub flags: u16,
    /// Zero. Keeps the pointer fields below 8-aligned on every target.
    pub reserved: u16,
    /// NUL-terminated UTF-8, `'static`. Must match the name the app declared
    /// the module under.
    pub name: *const c_char,
    /// NUL-terminated UTF-8, `'static`.
    pub version: *const c_char,
    /// Required. Runs once, before any call.
    pub init: Option<InitFn>,
    /// Required when the manifest declares functions. `input` is a bincode
    /// [`Call`](crate::Call), `ctx` is empty (the plugin already has its
    /// context from init), and `out` is a bincode
    /// [`CallOut`](crate::CallOut).
    pub call: Option<HookFn>,
    /// Optional. Runs once, when the app is going down.
    pub shutdown: Option<unsafe extern "C" fn()>,
    /// Required. Frees every buffer the entries above hand out.
    pub free: Option<FreeFn>,
}

// The host stores a `&'static Desc` inside a Send + Sync set; the raw
// pointers inside are 'static C strings the macro generates.
unsafe impl Send for Desc {}
unsafe impl Sync for Desc {}

/// What the host offers a plugin: an opaque context pointer and the calls a
/// plugin makes back into the engine.
///
/// The table and everything it points at live for the process. `ctx` is
/// host-owned and opaque; a plugin passes it back verbatim and never frees
/// it.
///
/// Every entry is callable from any thread, at any time, including
/// concurrently with a call the plugin is already inside. None of them block
/// on the tick thread, and the host never calls back into the plugin from
/// inside one, so a plugin's worker thread can emit while the tick thread is
/// running one of its functions.
#[repr(C)]
pub struct HostVtable {
    /// `size_of::<HostVtable>()` on the host side. A plugin built against a
    /// later ABI reads only the fields this covers.
    pub struct_size: u32,
    /// Zero. Keeps `ctx` 8-aligned on every target.
    pub _pad: u32,
    /// Opaque host state, passed back to every entry below.
    pub ctx: *mut c_void,
    /// Deliver a bincode [`PluginEvent`](crate::PluginEvent) to the engine.
    /// Returns [`OK`] when the event was queued, [`ERR`] when the queue is
    /// gone (the app is shutting down) or the payload did not decode.
    pub emit_event: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32>,
    /// Write a UTF-8 line to the engine's diagnostic output at `level`, one
    /// of the [`LogLevel`] codes.
    pub log: Option<unsafe extern "C" fn(*mut c_void, level: i32, *const u8, usize)>,
    /// Ask the engine for another tick, so an event emitted from a worker
    /// thread is drained without waiting for input.
    pub wake: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// How loud a line handed to [`HostVtable::log`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Something failed and the plugin could not do what it was asked.
    Error,
    /// Something is wrong but the plugin carried on.
    Warn,
    /// Worth saying once.
    Info,
    /// Detail for someone debugging the plugin.
    Debug,
    /// Per-call detail.
    Trace,
}

impl From<LogLevel> for i32 {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => 0,
            LogLevel::Warn => 1,
            LogLevel::Info => 2,
            LogLevel::Debug => 3,
            LogLevel::Trace => 4,
        }
    }
}

/// A code no version of this ABI defines reads as [`LogLevel::Info`]: a line
/// from a plugin built against a later Lumen is still worth printing.
impl From<i32> for LogLevel {
    fn from(code: i32) -> Self {
        match code {
            0 => Self::Error,
            1 => Self::Warn,
            3 => Self::Debug,
            4 => Self::Trace,
            _ => Self::Info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_keeps_its_pointers_aligned() {
        assert_eq!(std::mem::offset_of!(Desc, abi_version), 0);
        assert_eq!(std::mem::offset_of!(Desc, struct_size), 4);
        assert_eq!(std::mem::offset_of!(Desc, name) % 8, 0);
        assert_eq!(std::mem::offset_of!(HostVtable, ctx) % 8, 0);
    }

    #[test]
    fn log_levels_round_trip_and_unknown_codes_read_as_info() {
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert_eq!(LogLevel::from(i32::from(level)), level);
        }
        assert_eq!(LogLevel::from(99), LogLevel::Info);
        assert_eq!(LogLevel::from(-1), LogLevel::Info);
    }
}
