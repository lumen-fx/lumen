//! dlopen loader for the link-not-embed launcher (`dlopen-run`).
//!
//! Instead of static-linking `lumen-runtime`, the thin launcher discovers and
//! `dlopen`s the shared liblumen (`lumen-ffi` built as a cdylib), verifies its
//! ABI, and drives a prebuilt LMNA app across the C-ABI. See
//! `docs/design/link-not-embed.md`.
//!
//! Library discovery order:
//!   1. next to the `lumenc` executable,
//!   2. the `LUMEN_LIB_DIR` directory override,
//!   3. the platform default loader search (bare soname).

use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

/// ABI the launcher was built against. Must stay in lockstep with
/// `lumen_ffi::LUMEN_ABI_{MAJOR,MINOR}` (checked against the loaded library's
/// `lumen_abi_version()` at load time). The launcher does not link `lumen-ffi`,
/// so it carries its own copy of the expectation.
const EXPECTED_ABI_MAJOR: u32 = 0;
const EXPECTED_ABI_MINOR: u32 = 7;

/// Errors from discovering, loading, or driving liblumen.
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    /// No liblumen could be found on any of the search paths.
    #[error(
        "could not find liblumen ({soname}) -- looked next to the lumenc \
         executable, then $LUMEN_LIB_DIR, then the platform default search. \
         Set LUMEN_LIB_DIR to the directory containing it. (last error: {last})"
    )]
    NotFound {
        /// Platform soname probed for.
        soname: String,
        /// The final dlopen error encountered.
        last: String,
    },
    /// A required export was missing from the loaded library.
    #[error("liblumen is missing the '{0}' export (ABI too old?)")]
    MissingSymbol(String),
    /// The loaded library reports an incompatible ABI version.
    #[error(
        "liblumen ABI mismatch: launcher built against {want_major}.{want_minor}.x, \
         library reports {got_major}.{got_minor}.{got_patch}. \
         Rebuild lumen-ffi or use a matching liblumen."
    )]
    AbiMismatch {
        /// Major ABI the launcher was built against.
        want_major: u32,
        /// Minor ABI the launcher was built against.
        want_minor: u32,
        /// Major ABI the loaded library reports.
        got_major: u32,
        /// Minor ABI the loaded library reports.
        got_minor: u32,
        /// Patch ABI the loaded library reports.
        got_patch: u32,
    },
    /// `lumen_app_new_from_lmna` returned null (bad LMNA bytes / base dir).
    #[error("liblumen failed to build the app from LMNA bytes: {0}")]
    Build(String),
    /// A run entry point returned a non-OK status.
    #[error("liblumen run failed (status {status}): {message}")]
    Run {
        /// The non-OK `LumenStatus` code returned by the run entry point.
        status: u32,
        /// The library's `lumen_last_error()` message, if any.
        message: String,
    },
}

/// Platform-specific shared-library file name for the liblumen cdylib. The
/// crate is `lumen-ffi`, so the produced file is `liblumen_ffi.{so,dylib}` /
/// `lumen_ffi.dll`: the same name the C++ / Python SDKs load.
fn soname() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "lumen_ffi.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "liblumen_ffi.dylib"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "liblumen_ffi.so"
    }
}

/// Ordered candidate paths to probe for liblumen.
fn candidates() -> Vec<PathBuf> {
    let name = soname();
    let mut out: Vec<PathBuf> = Vec::new();
    // 1. Next to the lumenc executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join(name));
    }
    // 2. $LUMEN_LIB_DIR override (a directory).
    if let Some(dir) = std::env::var_os("LUMEN_LIB_DIR") {
        out.push(Path::new(&dir).join(name));
    }
    // 3. Bare soname: the platform default search (LD_LIBRARY_PATH / DYLD_* /
    //    the DLL search path) resolves it.
    out.push(PathBuf::from(name));
    out
}

/// Discover and open liblumen from the ordered candidate paths.
fn open() -> Result<Library, LoaderError> {
    let mut last = String::from("(none tried)");
    for cand in candidates() {
        // SAFETY: opening a shared library runs its initializers; liblumen is a
        // first-party cdylib with no hostile init. The handle is leaked for the
        // process lifetime by the caller.
        match unsafe { Library::new(&cand) } {
            Ok(lib) => return Ok(lib),
            Err(e) => last = format!("{}: {e}", cand.display()),
        }
    }
    Err(LoaderError::NotFound {
        soname: soname().to_string(),
        last,
    })
}

/// Verify the loaded library's ABI against the launcher's expectation: major
/// must match exactly; the library minor must be >= the launcher's (the
/// launcher only calls symbols present in its own minor).
fn check_abi(lib: &Library) -> Result<(), LoaderError> {
    let version: Symbol<unsafe extern "C" fn() -> u32> = unsafe { lib.get(b"lumen_abi_version\0") }
        .map_err(|_| LoaderError::MissingSymbol("lumen_abi_version".into()))?;
    let packed = unsafe { version() };
    let got_major = packed >> 16;
    let got_minor = (packed >> 8) & 0xFF;
    let got_patch = packed & 0xFF;
    if got_major != EXPECTED_ABI_MAJOR || got_minor < EXPECTED_ABI_MINOR {
        return Err(LoaderError::AbiMismatch {
            want_major: EXPECTED_ABI_MAJOR,
            want_minor: EXPECTED_ABI_MINOR,
            got_major,
            got_minor,
            got_patch,
        });
    }
    Ok(())
}

/// Read `lumen_last_error()` from the library as an owned string (best effort).
unsafe fn last_error(lib: &Library) -> String {
    let sym: Result<Symbol<unsafe extern "C" fn() -> *const c_char>, _> =
        unsafe { lib.get(b"lumen_last_error\0") };
    let Ok(f) = sym else {
        return "(no lumen_last_error export)".to_string();
    };
    let p = unsafe { f() };
    if p.is_null() {
        return "(no error message)".to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// Compile-free run over the dlopen seam: hand prebuilt LMNA `bytes` to a
/// discovered liblumen and drive it. `headless` selects the run mode: `None`
/// opens a window and blocks until close; `Some(ticks)` drives `ticks`
/// window-free main-schedule ticks and returns.
pub fn run_via_dlopen(
    bytes: &[u8],
    base_dir: &Path,
    headless: Option<u32>,
) -> Result<(), LoaderError> {
    let lib = open()?;
    check_abi(&lib)?;

    // Resolve the entry points we drive.
    type NewFromLmna = unsafe extern "C" fn(*const u8, usize, *const c_char) -> *mut c_void;
    type RunFn = unsafe extern "C" fn(*mut c_void) -> u32;
    type RunHeadlessFn = unsafe extern "C" fn(*mut c_void, u32) -> u32;

    let new_from_lmna: Symbol<NewFromLmna> = unsafe { lib.get(b"lumen_app_new_from_lmna\0") }
        .map_err(|_| LoaderError::MissingSymbol("lumen_app_new_from_lmna".into()))?;

    // Base dir as a C string for relative asset resolution.
    let base = base_dir.to_string_lossy();
    let c_base = CString::new(base.as_ref()).unwrap_or_default();

    let app = unsafe { new_from_lmna(bytes.as_ptr(), bytes.len(), c_base.as_ptr()) };
    if app.is_null() {
        return Err(LoaderError::Build(unsafe { last_error(&lib) }));
    }

    let status = match headless {
        Some(ticks) => {
            let run: Symbol<RunHeadlessFn> = unsafe { lib.get(b"lumen_app_run_headless\0") }
                .map_err(|_| LoaderError::MissingSymbol("lumen_app_run_headless".into()))?;
            unsafe { run(app, ticks) }
        }
        None => {
            let run: Symbol<RunFn> = unsafe { lib.get(b"lumen_app_run\0") }
                .map_err(|_| LoaderError::MissingSymbol("lumen_app_run".into()))?;
            unsafe { run(app) }
        }
    };

    if status != 0 {
        let message = unsafe { last_error(&lib) };
        return Err(LoaderError::Run { status, message });
    }

    // Keep the library mapped for the whole process: symbols were resolved from
    // it and any lingering state lives in it. Unloading is unsound here.
    std::mem::forget(lib);
    Ok(())
}
