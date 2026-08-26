//! The loader side, minus the descriptor: open a plugin library, read the
//! frozen prefix every descriptor layout starts with, and drive one hook.
//!
//! A descriptor's hook slots differ between the plugin systems, so the layout
//! stays with each of them. Everything here works on raw pointers and the
//! scalars in [`crate::raw`], so both loaders share the same unsafe code and
//! the same wording in the errors a user sees.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;

use libloading::{Library, Symbol};

use crate::raw::{Buf, ERR, FreeFn, HookFn, OK, PANICKED, UNCHANGED};

/// Join an error with its source chain, most specific last.
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

/// dlopen a plugin library. On failure the message is the whole error chain:
/// libloading's Display is a bare "dlopen failed", and the OS reason (missing
/// dependency, wrong architecture, not a library) sits in the source chain,
/// where it is the only actionable part.
pub fn open_library(path: &Path) -> Result<Library, String> {
    unsafe { Library::new(path) }.map_err(|e| error_chain(&e))
}

/// Call a library's entry symbol and hand back the descriptor pointer it
/// returns. `None` means the library exports no such symbol, which is how a
/// library that is not a plugin at all shows up.
///
/// # Safety
/// `symbol`, when the library exports it, must be
/// `unsafe extern "C" fn() -> *const T` for the caller's descriptor type.
pub unsafe fn entry_descriptor(lib: &Library, symbol: &[u8]) -> Option<*const u8> {
    let entry: Symbol<unsafe extern "C" fn() -> *const u8> = unsafe { lib.get(symbol) }.ok()?;
    Some(unsafe { entry() })
}

/// What the frozen prefix of a descriptor can be wrong about.
#[derive(Debug, thiserror::Error)]
pub enum PrefixError {
    #[error("built for plugin ABI {got}, this build speaks {want}")]
    AbiMismatch { want: u32, got: u32 },
    #[error("descriptor is {got} bytes, expected at least {want}")]
    ShortStruct { got: u32, want: usize },
}

/// Check the two fields every descriptor layout starts with: a `u32`
/// `abi_version` then a `u32` `struct_size`. Their offsets are frozen
/// forever, and they are read through the raw pointer before a reference to
/// the whole struct exists, so a truncated or foreign descriptor is refused
/// without ever forming a reference past its end.
///
/// # Safety
/// `desc` is non-null and points at at least 8 readable bytes; any exporter
/// of an entry symbol provides at least the frozen prefix.
pub unsafe fn verify_prefix(
    desc: *const u8,
    want_abi: u32,
    want_size: usize,
) -> Result<(), PrefixError> {
    let abi_version = unsafe { (desc as *const u32).read() };
    let struct_size = unsafe { (desc as *const u32).add(1).read() };
    if abi_version != want_abi {
        return Err(PrefixError::AbiMismatch {
            want: want_abi,
            got: abi_version,
        });
    }
    if (struct_size as usize) < want_size {
        return Err(PrefixError::ShortStruct {
            got: struct_size,
            want: want_size,
        });
    }
    Ok(())
}

/// Read a NUL-terminated UTF-8 descriptor field. `what` names the field in
/// the error, which reads as a descriptor "reason" on either loader.
///
/// # Safety
/// `ptr`, when non-null, points at a NUL-terminated string that lives at
/// least as long as the call.
pub unsafe fn c_string(ptr: *const c_char, what: &str) -> Result<String, String> {
    if ptr.is_null() {
        return Err(format!("null {what}"));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| format!("{what} is not UTF-8"))
}

/// One hook's outcome at the byte level.
#[derive(Debug)]
pub enum HookOut {
    Unchanged,
    Bytes(Vec<u8>),
}

/// Why a hook call did not produce a payload.
#[derive(Debug)]
pub enum CallError {
    /// The hook reported a failure; the string is its message.
    Failed(String),
    /// The hook panicked; the string is the panic message.
    Panicked(String),
    /// A status code no version of this ABI defines.
    UnknownStatus(i32),
}

/// One FFI hook call: borrowed input in, plugin-owned buffer out, freed here
/// through the plugin's own `free` after copying.
///
/// # Safety
/// `hook` and `free` come from the same verified descriptor, so the buffer
/// the hook allocates is the one `free` knows how to release.
pub unsafe fn call_hook(
    hook: HookFn,
    free: FreeFn,
    input: &[u8],
    ctx: &[u8],
) -> Result<HookOut, CallError> {
    let mut out = Buf::empty();
    let status = unsafe {
        hook(
            input.as_ptr(),
            input.len(),
            ctx.as_ptr(),
            ctx.len(),
            &mut out,
        )
    };
    let bytes = if out.ptr.is_null() {
        Vec::new()
    } else {
        let copied = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        unsafe { free(out.ptr, out.len, out.cap) };
        copied
    };
    match status {
        OK => Ok(HookOut::Bytes(bytes)),
        UNCHANGED => Ok(HookOut::Unchanged),
        ERR => Err(CallError::Failed(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        PANICKED => Err(CallError::Panicked(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        other => Err(CallError::UnknownStatus(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{fill, free_buf};

    #[test]
    fn error_chains_join_every_source() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let joined = error_chain(&Outer(std::io::Error::other("inner reason")));
        assert_eq!(joined, "outer: inner reason");
    }

    #[test]
    fn a_text_file_fails_to_open_with_the_os_reason() {
        let dir =
            std::env::temp_dir().join(format!("lumen-plugin-abi-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("libfake.so");
        std::fs::write(&fake, b"this is not an elf").unwrap();
        let err = open_library(&fake).unwrap_err();
        // The libloading Display alone is "dlopen failed"; the chain carries
        // the loader's reason, which is the actionable part.
        assert!(err.len() > "dlopen failed".len() + 10, "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stand-in descriptor: the frozen prefix and nothing else. Every
    /// layout starts this way, which is the whole point of the check.
    #[repr(C)]
    struct Prefix {
        abi_version: u32,
        struct_size: u32,
        rest: [u8; 16],
    }

    fn prefix(abi_version: u32, struct_size: u32) -> Prefix {
        Prefix {
            abi_version,
            struct_size,
            rest: [0; 16],
        }
    }

    #[test]
    fn a_matching_prefix_verifies() {
        let d = prefix(3, 24);
        unsafe { verify_prefix((&d as *const Prefix).cast(), 3, 24) }.unwrap();
    }

    #[test]
    fn a_foreign_abi_version_is_refused() {
        let d = prefix(2, 24);
        let err = unsafe { verify_prefix((&d as *const Prefix).cast(), 3, 24) }.unwrap_err();
        assert!(matches!(err, PrefixError::AbiMismatch { want: 3, got: 2 }));
    }

    #[test]
    fn a_truncated_struct_is_refused() {
        let d = prefix(3, 8);
        let err = unsafe { verify_prefix((&d as *const Prefix).cast(), 3, 24) }
            .unwrap_err()
            .to_string();
        assert_eq!(err, "descriptor is 8 bytes, expected at least 24");
    }

    #[test]
    fn a_longer_struct_from_a_newer_build_is_accepted() {
        let d = prefix(3, 64);
        unsafe { verify_prefix((&d as *const Prefix).cast(), 3, 24) }.unwrap();
    }

    #[test]
    fn c_string_fields_report_what_is_wrong_with_them() {
        assert_eq!(
            unsafe { c_string(c"demo".as_ptr(), "name") }.unwrap(),
            "demo"
        );
        assert_eq!(
            unsafe { c_string(std::ptr::null(), "name") }.unwrap_err(),
            "null name"
        );
        assert_eq!(
            unsafe { c_string(c"\xff\xfe".as_ptr(), "version") }.unwrap_err(),
            "version is not UTF-8"
        );
    }

    unsafe extern "C" fn echo_hook(
        input: *const u8,
        input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let bytes = unsafe { std::slice::from_raw_parts(input, input_len) }.to_vec();
        fill(unsafe { &mut *out }, bytes);
        OK
    }

    unsafe extern "C" fn status_only_hook(
        input: *const u8,
        input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let status = unsafe { *input };
        if input_len > 1 {
            fill(unsafe { &mut *out }, b"the reason".to_vec());
        }
        status as i32
    }

    #[test]
    fn an_ok_hook_hands_back_its_payload() {
        let out = unsafe { call_hook(echo_hook, free_buf, b"payload", b"") }.unwrap();
        assert!(matches!(out, HookOut::Bytes(b) if b == b"payload"));
    }

    #[test]
    fn the_status_codes_map_to_their_outcomes() {
        let call = |bytes: &[u8]| unsafe { call_hook(status_only_hook, free_buf, bytes, b"") };
        assert!(matches!(
            call(&[UNCHANGED as u8]).unwrap(),
            HookOut::Unchanged
        ));
        assert!(matches!(
            call(&[ERR as u8, 1]).unwrap_err(),
            CallError::Failed(m) if m == "the reason"
        ));
        assert!(matches!(
            call(&[PANICKED as u8, 1]).unwrap_err(),
            CallError::Panicked(m) if m == "the reason"
        ));
        assert!(matches!(
            call(&[99]).unwrap_err(),
            CallError::UnknownStatus(99)
        ));
    }
}
