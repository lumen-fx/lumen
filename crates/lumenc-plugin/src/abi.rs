//! The C ABI a compiler plugin exports and the host loads.
//!
//! Everything that crosses the boundary is bytes and C scalars: hook payloads
//! travel as bincode buffers (see [`crate::codec`]), never as Rust types, so
//! a plugin built by any compiler works against any lumenc built from the
//! same release tag. Compatibility is enforced by the handshake fields on
//! [`Desc`], not by hoping both sides agree.
//!
//! [`Desc`] is what makes this the *compiler's* ABI; the buffer, the hook
//! signature, and the status codes below come from `lumen_plugin_abi::raw`
//! and mean the same thing at every Lumen plugin boundary.

use std::os::raw::c_char;

pub use lumen_plugin_abi::raw::{
    Buf, ERR, FLAG_PANIC_ABORT, FreeFn, HookFn, OK, PANICKED, UNCHANGED,
};

/// Version of this descriptor layout and of the SDK-owned wire shapes.
/// Bump on any change to [`Desc`], [`Buf`], the status codes, or any serde
/// type this crate sends across the boundary (`Ctx`, `Finding`, `Output`);
/// `lumen_ir` types are covered by [`Desc::ir_format_version`] instead.
pub const ABI_VERSION: u32 = 1;

/// The one symbol a plugin cdylib exports:
/// `unsafe extern "C" fn lumenc_plugin_v1() -> *const Desc`.
pub const ENTRY: &[u8] = b"lumenc_plugin_v1\0";

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
    /// `lumen_ir::artifact::FORMAT_VERSION` the plugin was built against.
    /// The IR payloads are only meaningful when it matches the host's.
    pub ir_format_version: u16,
    /// Bit set of [`FLAG_PANIC_ABORT`] and future flags; zero otherwise.
    pub flags: u16,
    /// NUL-terminated UTF-8, `'static`. Must match the `[[plugins]] name`
    /// declared in `lumen.toml`.
    pub name: *const c_char,
    /// NUL-terminated UTF-8, `'static`.
    pub version: *const c_char,
    pub transform_markup: Option<HookFn>,
    pub transform_css: Option<HookFn>,
    pub transform_ir: Option<HookFn>,
    pub lint: Option<HookFn>,
    pub emit: Option<HookFn>,
    /// Required. Frees every buffer the hooks hand out.
    pub free: Option<FreeFn>,
}

// The host stores a `&'static Desc` inside a Send + Sync set; the raw
// pointers inside are 'static C strings the macro generates.
unsafe impl Send for Desc {}
unsafe impl Sync for Desc {}
