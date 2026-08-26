//! The export side: [`lumen_plugin!`] and the support code it calls.
//!
//! The macro generates the whole C-ABI surface for a plugin crate; authors
//! write no unsafe code and no FFI. All logic lives in the dispatch functions
//! below, compiled into the plugin cdylib through this crate, so the
//! generated code is a handful of one-line thunks.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use lumen_plugin_abi::raw::{fill, panic_message};

use crate::abi::{Buf, ERR, HostVtable, OK, PANICKED};
use crate::wire::{Call, CallOut, InitCx};
use crate::{Cx, Host, HostInner, PluginFn, Registrar, RuntimePlugin, codec};

/// The `free` entry every generated descriptor carries.
#[doc(hidden)]
pub use lumen_plugin_abi::raw::free_buf;

/// What init left behind for the calls that follow it. One per loaded
/// library: a second library, even a copy of the same file, links its own
/// copy of this crate and gets its own.
static REGISTERED: OnceLock<Registered> = OnceLock::new();

struct Registered {
    fns: Vec<PluginFn>,
    host: &'static HostInner,
}

/// The init dispatch: read the context, let the plugin register, and answer
/// with the manifest.
fn init(plugin: &dyn RuntimePlugin, ctx: &[u8], host: *const HostVtable) -> (i32, Vec<u8>) {
    if REGISTERED.get().is_some() {
        return (
            ERR,
            b"already initialized; a runtime plugin holds one instance per process, and this \
              library was loaded more than once"
                .to_vec(),
        );
    }
    let cx: InitCx = match codec::decode(ctx) {
        Ok(c) => c,
        Err(e) => return (ERR, format!("context decode: {e}").into_bytes()),
    };
    // Leaked rather than stored in a `OnceLock` beside the registration: a
    // `Host` handed to a plugin's worker thread outlives every borrow the
    // engine could hand back, and the table it wraps is process-lifetime on
    // the host side too.
    let host: &'static HostInner = Box::leak(Box::new(unsafe { HostInner::from_vtable(host) }));
    let mut registrar = Registrar::new(Host::new(host));
    if let Err(e) = plugin.register(&mut registrar, &cx) {
        return (ERR, e.message.into_bytes());
    }
    let manifest = registrar.manifest();
    let _ = REGISTERED.set(Registered {
        fns: registrar.take_fns(),
        host,
    });
    match codec::encode(&manifest) {
        Ok(bytes) => (OK, bytes),
        Err(e) => (ERR, format!("manifest encode: {e}").into_bytes()),
    }
}

/// The call dispatch: run one registered function and answer with what it
/// returned and emitted.
fn call(input: &[u8]) -> (i32, Vec<u8>) {
    let Some(registered) = REGISTERED.get() else {
        return (ERR, b"called before init".to_vec());
    };
    let call: Call = match codec::decode(input) {
        Ok(c) => c,
        Err(e) => return (ERR, format!("call decode: {e}").into_bytes()),
    };
    let Some(f) = registered.fns.get(call.index as usize) else {
        return (
            ERR,
            format!(
                "no function at index {}; this plugin registered {}",
                call.index,
                registered.fns.len()
            )
            .into_bytes(),
        );
    };
    let mut commands = Vec::new();
    let ret = {
        let mut cx = Cx::new(&call.args, &mut commands, Host::new(registered.host));
        f.invoke(&mut cx)
    };
    match codec::encode(&CallOut { ret, commands }) {
        Ok(bytes) => (OK, bytes),
        // The one payload a body can carry that has no encoding is a
        // `ScriptCommand::SetProperty` holding a `PropertyValue::Custom`,
        // which cannot be built from data that crossed the boundary.
        Err(e) => (
            ERR,
            format!("{}: result encode: {e}", f.name()).into_bytes(),
        ),
    }
}

/// The body every generated init thunk calls.
///
/// A panic in the plugin is caught here, on the plugin side of the boundary,
/// because unwinding out of an `extern "C"` function aborts the process.
/// This is also why a plugin must not be built with `panic = "abort"`: that
/// setting removes the unwind this catch depends on, and any panic kills the
/// app instead of failing the plugin.
///
/// # Safety
/// `ctx` must be valid for reads of `ctx_len`; `host`, when non-null, must
/// point at a live [`HostVtable`]; `out` must be valid for writes.
#[doc(hidden)]
pub unsafe fn init_entry(
    plugin: fn() -> &'static dyn RuntimePlugin,
    ctx: *const u8,
    ctx_len: usize,
    host: *const HostVtable,
    out: *mut Buf,
) -> i32 {
    let ctx = unsafe { std::slice::from_raw_parts(ctx, ctx_len) };
    // The getter runs inside the catch too: first-call construction is a
    // plugin-authored code path and a panicking constructor must fail the
    // module's load, not abort the app.
    let (status, bytes) = match catch_unwind(AssertUnwindSafe(|| init(plugin(), ctx, host))) {
        Ok(r) => r,
        Err(payload) => (PANICKED, panic_message(payload.as_ref()).into_bytes()),
    };
    if !bytes.is_empty() {
        fill(unsafe { &mut *out }, bytes);
    }
    status
}

/// The body every generated call thunk calls. See [`init_entry`] for why the
/// panic is caught here.
///
/// # Safety
/// `input` must be valid for reads of `input_len`; `out` must be valid for
/// writes.
#[doc(hidden)]
pub unsafe fn call_entry(input: *const u8, input_len: usize, out: *mut Buf) -> i32 {
    let input = unsafe { std::slice::from_raw_parts(input, input_len) };
    let (status, bytes) = match catch_unwind(AssertUnwindSafe(|| call(input))) {
        Ok(r) => r,
        Err(payload) => (PANICKED, panic_message(payload.as_ref()).into_bytes()),
    };
    if !bytes.is_empty() {
        fill(unsafe { &mut *out }, bytes);
    }
    status
}

/// The body every generated shutdown thunk calls. A plugin whose init never
/// succeeded is never constructed here, and a panic on the way down is
/// swallowed: there is no call left to fail.
#[doc(hidden)]
pub fn shutdown_entry(plugin: fn() -> &'static dyn RuntimePlugin) {
    if REGISTERED.get().is_none() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| plugin().shutdown()));
}

/// Export a [`RuntimePlugin`](crate::RuntimePlugin) from a cdylib crate.
///
/// Takes an expression that constructs the plugin; it runs once, when the
/// engine initializes the module:
///
/// ```ignore
/// lumen_plugin::lumen_plugin!(MyPlugin::default);
/// ```
///
/// The plugin crate must keep the default `panic = "unwind"`; see
/// [`init_entry`] for why.
#[macro_export]
macro_rules! lumen_plugin {
    ($ctor:expr) => {
        const _: () = {
            static INSTANCE: ::std::sync::OnceLock<::std::boxed::Box<dyn $crate::RuntimePlugin>> =
                ::std::sync::OnceLock::new();

            fn instance() -> &'static dyn $crate::RuntimePlugin {
                INSTANCE
                    .get_or_init(|| ::std::boxed::Box::new(($ctor)()))
                    .as_ref()
            }

            unsafe extern "C" fn init_thunk(
                ctx: *const u8,
                ctx_len: usize,
                host: *const $crate::abi::HostVtable,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe { $crate::export::init_entry(instance, ctx, ctx_len, host, out) }
            }

            unsafe extern "C" fn call_thunk(
                input: *const u8,
                input_len: usize,
                _ctx: *const u8,
                _ctx_len: usize,
                out: *mut $crate::abi::Buf,
            ) -> i32 {
                unsafe { $crate::export::call_entry(input, input_len, out) }
            }

            unsafe extern "C" fn shutdown_thunk() {
                $crate::export::shutdown_entry(instance);
            }

            static DESC: $crate::abi::Desc = $crate::abi::Desc {
                abi_version: $crate::abi::ABI_VERSION,
                struct_size: ::std::mem::size_of::<$crate::abi::Desc>() as u32,
                script_wire_version: $crate::SCRIPT_WIRE_VERSION,
                paint_wire_version: $crate::PAINT_WIRE_VERSION,
                flags: if ::std::cfg!(panic = "abort") {
                    $crate::abi::FLAG_PANIC_ABORT
                } else {
                    0
                },
                reserved: 0,
                name: concat!(env!("CARGO_PKG_NAME"), "\0").as_ptr()
                    as *const ::std::os::raw::c_char,
                version: concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
                    as *const ::std::os::raw::c_char,
                init: Some(init_thunk),
                call: Some(call_thunk),
                shutdown: Some(shutdown_thunk),
                free: Some($crate::export::free_buf),
            };

            #[unsafe(no_mangle)]
            pub extern "C" fn lumen_plugin_v1() -> *const $crate::abi::Desc {
                &DESC
            }
        };
    };
}
