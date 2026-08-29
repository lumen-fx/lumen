//! The Lumen app launcher: the executable a packaged app is built from.
//!
//! `lumenc package` copies this binary, appends the app's compiled artifact to
//! the copy, and puts liblumen and the app's files beside it. Started by an end
//! user, the result finds its own artifact, opens the shared library sitting
//! next to it, and runs the app. Nothing is compiled and no toolchain is
//! involved; see `docs/docs/guides/packaging.md`.
//!
//! The artifact reaches the launcher one of two ways:
//!
//! - appended to the executable itself, with a trailing footer marking where it
//!   starts. Windows and Linux program loaders ignore bytes past the end of the
//!   image, so this is a plain file append.
//! - as a `<name>.lmna` file beside the executable. This is what a macOS
//!   package cross-built from another platform ships, where appending is not
//!   available (a Mach-O signature has to cover the whole file).
//!
//! The `static-run` feature builds the other shape of the same launcher: the
//! engine and the first-party runtime modules compiled in, nothing opened at
//! run time, and nothing that has to sit beside the executable. A macOS
//! package built on macOS carries its artifact in a `__LUMEN,__lmna` section
//! instead, which that shape reads as a third artifact source.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

// Link-line anchors. A cargo dependency nobody names puts nothing on the link
// line, and these crates exist for what their constructors do before `main`:
// each one leaves its module on the registry the loader reads.
#[cfg(feature = "static-run")]
use lumen_archive as _;
#[cfg(feature = "static-run")]
use lumen_audio as _;
#[cfg(feature = "static-run")]
use lumen_canvas as _;
#[cfg(feature = "static-run")]
use lumen_download as _;
#[cfg(feature = "static-run")]
use lumen_fs as _;
#[cfg(feature = "static-run")]
use lumen_process as _;

/// Marks an appended artifact. The last bytes of a packaged executable are
/// this magic followed by the payload length, and the payload sits directly
/// before them.
const FOOTER_MAGIC: &[u8; 8] = b"LMNAPACK";

/// Magic plus a little-endian `u64` length.
const FOOTER_LEN: usize = FOOTER_MAGIC.len() + 8;

fn main() -> ExitCode {
    let mut headless = false;
    let mut ticks: u32 = 1;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--ticks" => match args.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(n) => ticks = n,
                None => {
                    eprintln!("--ticks needs a whole number of ticks");
                    return ExitCode::from(2);
                }
            },
            // Anything else belongs to the app, not to the launcher.
            _ => {}
        }
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot locate this executable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let base_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();

    let bytes = match load_artifact(&exe) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    #[cfg(not(feature = "static-run"))]
    let outcome = lumenc::loader::run_via_dlopen(&bytes, &base_dir, headless.then_some(ticks))
        .map_err(|e| e.to_string());
    #[cfg(feature = "static-run")]
    let outcome = run_via_static(&bytes, &base_dir, headless.then_some(ticks));

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Run the app in this process. The same sequence
/// `lumenc::loader::run_via_dlopen` drives across the dlopen seam, with the
/// entry points called as ordinary functions: this binary compiled the engine
/// in, so there is no library to find and no ABI version to agree on - the
/// two sides are one build.
#[cfg(feature = "static-run")]
fn run_via_static(bytes: &[u8], base_dir: &Path, headless: Option<u32>) -> Result<(), String> {
    use std::ffi::CString;

    // Base dir as a C string for relative asset resolution.
    let base = base_dir.to_string_lossy();
    let c_base = CString::new(base.as_ref()).unwrap_or_default();

    // SAFETY: the byte slice and the C string are both live across the call,
    // which is all the entry borrows them for - it copies what it keeps.
    let app =
        unsafe { lumen::lumen_app_new_from_lmna(bytes.as_ptr(), bytes.len(), c_base.as_ptr()) };
    if app.is_null() {
        return Err(format!(
            "failed to build the app from LMNA bytes: {}",
            last_error()
        ));
    }

    // SAFETY: the handle came from the call above and is passed exactly once;
    // either entry point consumes and frees it.
    let status = unsafe {
        match headless {
            Some(ticks) => lumen::lumen_app_run_headless(app, ticks),
            None => lumen::lumen_app_run(app),
        }
    };
    if status != lumen::LumenStatus::Ok {
        return Err(format!(
            "run failed (status {}): {}",
            status as u32,
            last_error()
        ));
    }
    Ok(())
}

/// The engine's last error message as an owned string (best effort).
#[cfg(feature = "static-run")]
fn last_error() -> String {
    // SAFETY: the export returns either null or a NUL-terminated string valid
    // until the next call on this thread that records an error.
    let p = unsafe { lumen::lumen_last_error() };
    if p.is_null() {
        return "(no error message)".to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// The app's artifact bytes: from the executable's own `__LUMEN,__lmna`
/// section where the platform has one, else appended to `exe` if it carries a
/// footer, else read from the sidecar file beside it.
///
/// Only the footer and the payload are read, never the program image around
/// them, so an app starts without pulling its own executable through memory.
fn load_artifact(exe: &Path) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};

    // First, because a macOS package built on macOS has no footer to find:
    // the artifact is linked into the image, not appended to it.
    #[cfg(all(target_os = "macos", feature = "static-run"))]
    if let Some(bytes) = section_artifact() {
        return Ok(bytes);
    }

    let read_error = |e: std::io::Error| format!("read {}: {e}", exe.display());
    let mut file = std::fs::File::open(exe).map_err(read_error)?;
    let total = file.metadata().map_err(read_error)?.len();
    if total >= FOOTER_LEN as u64 {
        file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
            .map_err(read_error)?;
        let mut footer = [0u8; FOOTER_LEN];
        file.read_exact(&mut footer).map_err(read_error)?;
        if let Some(range) = payload_range(total, &footer) {
            let mut bytes = vec![0u8; (range.end - range.start) as usize];
            file.seek(SeekFrom::Start(range.start))
                .map_err(read_error)?;
            file.read_exact(&mut bytes).map_err(read_error)?;
            return Ok(bytes);
        }
    }

    let sidecar = sidecar_path(exe);
    std::fs::read(&sidecar).map_err(|e| {
        format!(
            "this executable carries no app, and no app was found at {}: {e}",
            sidecar.display()
        )
    })
}

/// The artifact linked into this executable's `__LUMEN,__lmna` section, if it
/// carries one.
///
/// `lumenc package` links a macOS executable rather than appending to it, so
/// that the code signature covers the whole file: the artifact goes in with
/// `-sectcreate`, and the generated C wrapper reads it back through
/// `getsectiondata` against the image's own Mach-O header. This is the same
/// read, from the Rust side of the same shape.
#[cfg(all(target_os = "macos", feature = "static-run"))]
fn section_artifact() -> Option<Vec<u8>> {
    use std::os::raw::{c_char, c_ulong};

    // SAFETY of the block below rests on these two: `_mh_execute_header` is
    // the linker-provided Mach-O header of the running executable, present in
    // every macOS executable image and used here only for its address; and
    // `getsectiondata` is libSystem's section lookup, which returns either
    // null or a pointer to `size` bytes inside the mapped image.
    unsafe extern "C" {
        static _mh_execute_header: c_char;
        fn getsectiondata(
            mhp: *const c_char,
            segname: *const c_char,
            sectname: *const c_char,
            size: *mut c_ulong,
        ) -> *mut u8;
    }

    let mut size: c_ulong = 0;
    // SAFETY: the segment and section names are NUL-terminated literals, the
    // header address is this image's own, and `size` is written only on a
    // non-null return.
    let data = unsafe {
        getsectiondata(
            &raw const _mh_execute_header,
            c"__LUMEN".as_ptr(),
            c"__lmna".as_ptr(),
            &mut size,
        )
    };
    if data.is_null() || size == 0 {
        return None;
    }
    // SAFETY: the section is mapped for the life of the process, so the bytes
    // are readable here and copied out before anything else runs.
    Some(unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec())
}

/// Where the sidecar artifact lives: beside the executable, named after it.
fn sidecar_path(exe: &Path) -> PathBuf {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());
    exe.with_file_name(format!("{stem}.lmna"))
}

/// Where the appended artifact sits in a file of `total` bytes ending in
/// `footer`, or `None` when the footer is a foreign one or declares a length
/// that does not fit in the bytes before it.
fn payload_range(total: u64, footer: &[u8; FOOTER_LEN]) -> Option<std::ops::Range<u64>> {
    if &footer[..FOOTER_MAGIC.len()] != FOOTER_MAGIC {
        return None;
    }
    let mut len = [0u8; 8];
    len.copy_from_slice(&footer[FOOTER_MAGIC.len()..]);
    let len = u64::from_le_bytes(len);
    if len == 0 {
        return None;
    }
    let end = total.checked_sub(FOOTER_LEN as u64)?;
    let start = end.checked_sub(len)?;
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte range of the appended artifact in a whole image, or `None`
    /// when it carries no usable footer. The launcher itself seeks to the
    /// footer rather than holding the image, so this is how the tests reach
    /// the same decision over a slice.
    fn appended_artifact(image: &[u8]) -> Option<std::ops::Range<usize>> {
        if image.len() < FOOTER_LEN {
            return None;
        }
        let mut footer = [0u8; FOOTER_LEN];
        footer.copy_from_slice(&image[image.len() - FOOTER_LEN..]);
        let range = payload_range(image.len() as u64, &footer)?;
        Some(range.start as usize..range.end as usize)
    }

    fn with_footer(stub: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = stub.to_vec();
        out.extend_from_slice(payload);
        out.extend_from_slice(FOOTER_MAGIC);
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out
    }

    #[test]
    fn reads_an_appended_payload() {
        let image = with_footer(b"stub-image-bytes", b"LMNA-payload");
        let range = appended_artifact(&image).expect("footer found");
        assert_eq!(&image[range], b"LMNA-payload");
    }

    #[test]
    fn a_plain_executable_has_no_payload() {
        assert!(appended_artifact(b"an ordinary executable image").is_none());
        assert!(appended_artifact(b"").is_none());
    }

    /// A short read, a truncated payload, and an overlong declared length all
    /// have to come back empty rather than panic on the slice.
    #[test]
    fn a_truncated_image_has_no_payload() {
        let image = with_footer(b"stub", b"payload");
        assert!(appended_artifact(&image[..FOOTER_LEN - 1]).is_none());

        let mut overlong = image.clone();
        let len = overlong.len();
        overlong[len - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(appended_artifact(&overlong).is_none());

        let mut empty_payload = Vec::new();
        empty_payload.extend_from_slice(FOOTER_MAGIC);
        empty_payload.extend_from_slice(&0u64.to_le_bytes());
        assert!(appended_artifact(&empty_payload).is_none());
    }

    #[test]
    fn the_sidecar_is_named_after_the_executable() {
        assert_eq!(
            sidecar_path(Path::new("/apps/Notes/Notes")),
            PathBuf::from("/apps/Notes/Notes.lmna")
        );
        assert_eq!(
            sidecar_path(Path::new("/apps/Notes/Notes.exe")),
            PathBuf::from("/apps/Notes/Notes.lmna")
        );
    }
}
