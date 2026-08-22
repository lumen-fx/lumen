//! The export side: [`lumenc_plugin!`] and the support code it calls.
//!
//! The macro generates the whole C-ABI surface for a plugin crate; authors
//! write no unsafe code and no FFI. All logic lives in [`dispatch`],
//! compiled into the plugin cdylib through this crate, so the generated code
//! is a handful of one-line thunks.

use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::abi::{Buf, ERR, OK, PANICKED, UNCHANGED};
use crate::{CompilerPlugin, Ctx, codec};

/// Which hook a thunk serves.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub enum HookKind {
    Markup,
    Css,
    Ir,
    Lint,
    Emit,
}

/// Move a byte vector across the boundary. The host returns it through
/// [`free_buf`].
fn fill(out: &mut Buf, bytes: Vec<u8>) {
    let mut v = ManuallyDrop::new(bytes);
    out.ptr = v.as_mut_ptr();
    out.len = v.len();
    out.cap = v.capacity();
}

/// The `free` entry every generated descriptor carries: rebuilds the vector
/// [`fill`] leaked and drops it.
///
/// # Safety
/// `ptr`/`len`/`cap` must be exactly the triple a hook of this plugin
/// returned, unfreed.
#[doc(hidden)]
pub unsafe extern "C" fn free_buf(ptr: *mut u8, len: usize, cap: usize) {
    if !ptr.is_null() {
        unsafe { drop(Vec::from_raw_parts(ptr, len, cap)) };
    }
}

fn dispatch(
    plugin: &dyn CompilerPlugin,
    kind: HookKind,
    input: &[u8],
    ctx: &[u8],
) -> (i32, Vec<u8>) {
    let ctx: Ctx = match codec::decode(ctx) {
        Ok(c) => c,
        Err(e) => return (ERR, format!("context decode: {e}").into_bytes()),
    };
    let text = |input: &[u8]| -> Result<String, (i32, Vec<u8>)> {
        String::from_utf8(input.to_vec())
            .map_err(|e| (ERR, format!("source decode: {e}").into_bytes()))
    };
    let ir = |input: &[u8]| -> Result<crate::LayoutIR, (i32, Vec<u8>)> {
        codec::decode(input).map_err(|e| (ERR, format!("IR decode: {e}").into_bytes()))
    };
    let result = match kind {
        HookKind::Markup | HookKind::Css => {
            let src = match text(input) {
                Ok(s) => s,
                Err(e) => return e,
            };
            let out = match kind {
                HookKind::Markup => plugin.transform_markup(&src, &ctx),
                _ => plugin.transform_css(&src, &ctx),
            };
            match out {
                Ok(None) => return (UNCHANGED, Vec::new()),
                Ok(Some(s)) => return (OK, s.into_bytes()),
                Err(e) => return (ERR, e.message.into_bytes()),
            }
        }
        HookKind::Ir => {
            let mut tree = match ir(input) {
                Ok(t) => t,
                Err(e) => return e,
            };
            plugin
                .transform_ir(&mut tree, &ctx)
                .map(|()| codec::encode(&tree))
        }
        HookKind::Lint => {
            let tree = match ir(input) {
                Ok(t) => t,
                Err(e) => return e,
            };
            plugin.lint(&tree, &ctx).map(|f| codec::encode(&f))
        }
        HookKind::Emit => {
            let tree = match ir(input) {
                Ok(t) => t,
                Err(e) => return e,
            };
            plugin.emit(&tree, &ctx).map(|o| codec::encode(&o))
        }
    };
    match result {
        // The payload types are this crate's own serde structs plus
        // `LayoutIR`; all of them always bincode-encode, same contract the
        // host side relies on.
        Ok(enc) => (OK, enc.expect("SDK payloads always encode")),
        Err(e) => (ERR, e.message.into_bytes()),
    }
}

/// The body every generated thunk calls.
///
/// A panic in the hook is caught here, on the plugin side of the boundary,
/// because unwinding out of an `extern "C"` function aborts the process.
/// This is also why a plugin must not be built with `panic = "abort"`: that
/// setting removes the unwind this catch depends on, and any hook panic
/// kills the compiler instead of failing the compile.
///
/// # Safety
/// `input`/`ctx` must be valid for reads of their lengths; `out` must be
/// valid for writes.
#[doc(hidden)]
pub unsafe fn hook_entry(
    plugin: fn() -> &'static dyn CompilerPlugin,
    kind: HookKind,
    input: *const u8,
    input_len: usize,
    ctx: *const u8,
    ctx_len: usize,
    out: *mut Buf,
) -> i32 {
    let (input, ctx) = unsafe {
        (
            std::slice::from_raw_parts(input, input_len),
            std::slice::from_raw_parts(ctx, ctx_len),
        )
    };
    // The getter runs inside the catch too: first-call construction is a
    // plugin-authored code path and a panicking constructor must fail the
    // compile, not abort the process.
    let (status, bytes) =
        match catch_unwind(AssertUnwindSafe(|| dispatch(plugin(), kind, input, ctx))) {
            Ok(r) => r,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                (PANICKED, msg.into_bytes())
            }
        };
    if !bytes.is_empty() {
        fill(unsafe { &mut *out }, bytes);
    }
    status
}

/// Export a [`CompilerPlugin`](crate::CompilerPlugin) from a cdylib crate.
///
/// Takes an expression that constructs the plugin; it runs once, on first
/// hook call:
///
/// ```ignore
/// lumenc_plugin::lumenc_plugin!(MyPlugin::default);
/// ```
///
/// The plugin crate must keep the default `panic = "unwind"`; see
/// [`hook_entry`] for why.
#[macro_export]
macro_rules! lumenc_plugin {
    ($ctor:expr) => {
        const _: () = {
            static INSTANCE: ::std::sync::OnceLock<::std::boxed::Box<dyn $crate::CompilerPlugin>> =
                ::std::sync::OnceLock::new();

            fn instance() -> &'static dyn $crate::CompilerPlugin {
                INSTANCE
                    .get_or_init(|| ::std::boxed::Box::new(($ctor)()))
                    .as_ref()
            }

            unsafe extern "C" fn markup_thunk(
                input: *const u8,
                input_len: usize,
                ctx: *const u8,
                ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe {
                    $crate::export::hook_entry(
                        instance,
                        $crate::export::HookKind::Markup,
                        input,
                        input_len,
                        ctx,
                        ctx_len,
                        out,
                    )
                }
            }
            unsafe extern "C" fn css_thunk(
                input: *const u8,
                input_len: usize,
                ctx: *const u8,
                ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe {
                    $crate::export::hook_entry(
                        instance,
                        $crate::export::HookKind::Css,
                        input,
                        input_len,
                        ctx,
                        ctx_len,
                        out,
                    )
                }
            }
            unsafe extern "C" fn ir_thunk(
                input: *const u8,
                input_len: usize,
                ctx: *const u8,
                ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe {
                    $crate::export::hook_entry(
                        instance,
                        $crate::export::HookKind::Ir,
                        input,
                        input_len,
                        ctx,
                        ctx_len,
                        out,
                    )
                }
            }
            unsafe extern "C" fn lint_thunk(
                input: *const u8,
                input_len: usize,
                ctx: *const u8,
                ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe {
                    $crate::export::hook_entry(
                        instance,
                        $crate::export::HookKind::Lint,
                        input,
                        input_len,
                        ctx,
                        ctx_len,
                        out,
                    )
                }
            }
            unsafe extern "C" fn emit_thunk(
                input: *const u8,
                input_len: usize,
                ctx: *const u8,
                ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe {
                    $crate::export::hook_entry(
                        instance,
                        $crate::export::HookKind::Emit,
                        input,
                        input_len,
                        ctx,
                        ctx_len,
                        out,
                    )
                }
            }

            static DESC: $crate::abi::Desc = $crate::abi::Desc {
                abi_version: $crate::abi::ABI_VERSION,
                struct_size: ::std::mem::size_of::<$crate::abi::Desc>() as u32,
                ir_format_version: $crate::lumen_ir::artifact::FORMAT_VERSION,
                flags: if ::std::cfg!(panic = "abort") {
                    $crate::abi::FLAG_PANIC_ABORT
                } else {
                    0
                },
                name: concat!(env!("CARGO_PKG_NAME"), "\0").as_ptr()
                    as *const ::std::os::raw::c_char,
                version: concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
                    as *const ::std::os::raw::c_char,
                transform_markup: Some(markup_thunk),
                transform_css: Some(css_thunk),
                transform_ir: Some(ir_thunk),
                lint: Some(lint_thunk),
                emit: Some(emit_thunk),
                free: Some($crate::export::free_buf),
            };

            #[unsafe(no_mangle)]
            pub extern "C" fn lumenc_plugin_v1() -> *const $crate::abi::Desc {
                &DESC
            }
        };
    };
}
