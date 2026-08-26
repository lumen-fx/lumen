//! The C scalars every Lumen plugin boundary is built from.
//!
//! Everything that crosses a plugin boundary is bytes and C scalars: hook
//! payloads travel as bincode buffers (see [`crate::codec`]), never as Rust
//! types, so a plugin built by any compiler works against any host built from
//! the same release tag. What differs between the plugin systems is the
//! descriptor that names the hooks; the buffer, the hook signature, and the
//! status codes below are the same in all of them.

use std::mem::ManuallyDrop;

/// A byte buffer allocated by the plugin. The host reads it, then returns it
/// through the plugin's own free function; it never frees plugin memory with
/// its own allocator.
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

/// Set in a descriptor's `flags` by a plugin built with `panic = "abort"`.
/// The host refuses such a plugin at load: the panic-to-error contract
/// depends on unwinding, and an aborting plugin would kill the host on any
/// hook panic.
pub const FLAG_PANIC_ABORT: u16 = 1;

/// `out` holds the hook's payload.
pub const OK: i32 = 0;
/// `out` holds a UTF-8 error message; the call fails with it.
pub const ERR: i32 = 1;
/// The hook panicked; `out` holds the panic message.
pub const PANICKED: i32 = 2;
/// `out` is empty; the host keeps the input unchanged.
pub const UNCHANGED: i32 = 3;

/// Move a byte vector across the boundary. The host returns it through
/// [`free_buf`].
pub fn fill(out: &mut Buf, bytes: Vec<u8>) {
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
pub unsafe extern "C" fn free_buf(ptr: *mut u8, len: usize, cap: usize) {
    if !ptr.is_null() {
        unsafe { drop(Vec::from_raw_parts(ptr, len, cap)) };
    }
}

/// The message a caught panic crosses the boundary as. `panic!` payloads are
/// `&str` or `String`; anything else has no text to report.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filled_buffer_round_trips_through_free() {
        let mut buf = Buf::empty();
        assert!(buf.ptr.is_null());
        fill(&mut buf, b"payload".to_vec());
        assert_eq!(buf.len, 7);
        let copied = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
        assert_eq!(copied, b"payload");
        unsafe { free_buf(buf.ptr, buf.len, buf.cap) };
    }

    #[test]
    fn freeing_a_null_buffer_is_a_no_op() {
        unsafe { free_buf(std::ptr::null_mut(), 0, 0) };
    }

    #[test]
    fn panic_payloads_report_their_text_or_say_they_have_none() {
        let str_payload = std::panic::catch_unwind(|| panic!("literal")).unwrap_err();
        assert_eq!(panic_message(str_payload.as_ref()), "literal");

        let owned = std::panic::catch_unwind(|| panic!("{}", "formatted")).unwrap_err();
        assert_eq!(panic_message(owned.as_ref()), "formatted");

        let other = std::panic::catch_unwind(|| std::panic::panic_any(42usize)).unwrap_err();
        assert_eq!(panic_message(other.as_ref()), "non-string panic payload");
    }
}
