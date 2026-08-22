//! The C ABI a compiler plugin exports and the host loads.
//!
//! Everything that crosses the boundary is bytes and C scalars: hook payloads
//! travel as bincode buffers (see [`crate::codec`]), never as Rust types, so
//! a plugin built by any compiler works against any lumenc built from the
//! same release tag. Compatibility is enforced by the handshake fields on
//! [`Desc`], not by hoping both sides agree.

use std::os::raw::c_char;

/// Version of this descriptor layout and of the SDK-owned wire shapes.
/// Bump on any change to [`Desc`], [`Buf`], the status codes, or any serde
/// type this crate sends across the boundary (`Ctx`, `Finding`, `Output`);
/// `lumen_ir` types are covered by [`Desc::ir_format_version`] instead.
pub const ABI_VERSION: u32 = 1;

/// The one symbol a plugin cdylib exports:
/// `unsafe extern "C" fn lumenc_plugin_v1() -> *const Desc`.
pub const ENTRY: &[u8] = b"lumenc_plugin_v1\0";

/// A byte buffer allocated by the plugin. The host reads it, then returns it
/// through [`Desc::free`]; it never frees plugin memory with its own
/// allocator.
#[repr(C)]
pub struct Buf {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl Buf {
    /// An empty buffer, the state a hook receives `out` in.
    pub const fn empty() -> Self {
        Buf {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }
}

/// One hook entry point. `input`/`ctx` are borrowed for the call; on return
/// the status code says what `out` holds.
pub type HookFn = unsafe extern "C" fn(
    input: *const u8,
    input_len: usize,
    ctx: *const u8,
    ctx_len: usize,
    out: *mut Buf,
) -> i32;

/// Frees a buffer previously returned by any hook of the same plugin.
pub type FreeFn = unsafe extern "C" fn(ptr: *mut u8, len: usize, cap: usize);

/// Set in [`Desc::flags`] by a plugin built with `panic = "abort"`. The host
/// refuses such a plugin at load: the panic-to-error contract depends on
/// unwinding, and an aborting plugin would kill the compiler on any hook
/// panic.
pub const FLAG_PANIC_ABORT: u16 = 1;

/// `out` holds the hook's payload.
pub const OK: i32 = 0;
/// `out` holds a UTF-8 error message; the compile fails with it.
pub const ERR: i32 = 1;
/// The hook panicked; `out` holds the panic message.
pub const PANICKED: i32 = 2;
/// `out` is empty; the host keeps the input unchanged.
pub const UNCHANGED: i32 = 3;

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
