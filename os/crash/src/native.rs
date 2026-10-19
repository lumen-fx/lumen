//! The native fault handler: signals on Linux and macOS, structured
//! exceptions on Windows, through `crash-handler`.
//!
//! On a fault the handler asks the helper for a minidump and waits until it
//! is written; the helper reads the crashed process from outside. With no
//! helper connected, it writes the minimal in-process report instead. Then
//! the fault takes its course and the process ends.
//!
//! The panic hook uses the same handler to have the helper dump the
//! panicking moment: a simulated fault, which returns instead of ending the
//! process.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crash_handler::{CrashContext, CrashEventResult, CrashHandler, make_crash_event};

static HANDLER: OnceLock<CrashHandler> = OnceLock::new();
/// Set while the panic hook simulates a fault, so the handler does not
/// write a fault report for it.
static SIMULATING: AtomicBool = AtomicBool::new(false);

/// Attach the handler. A process where another crash handler from the same
/// crate is attached keeps that one.
pub(crate) fn install() {
    // SAFETY: `on_crash` runs in a compromised context. It reads two atomics,
    // makes the blocking IPC call `minidumper` provides for exactly this
    // place, and otherwise writes the fallback report with system calls into
    // buffers prepared at install time; it allocates nothing.
    let event = unsafe { make_crash_event(on_crash) };
    if let Ok(handler) = CrashHandler::attach(event) {
        let _ = HANDLER.set(handler);
    }
}

fn on_crash(context: &CrashContext) -> CrashEventResult {
    let simulated = SIMULATING.load(Ordering::Acquire);
    if let Some(client) = crate::client::current() {
        // On macOS the dump request travels a different channel from the
        // messages; a ping makes sure the helper has read them first.
        #[cfg(target_os = "macos")]
        let _ = client.ping();
        if client.request_dump(context).is_ok() {
            return CrashEventResult::Handled(true);
        }
    }
    if !simulated {
        crate::fallback::write_fault(context);
        if sent_by_kill(context) {
            return CrashEventResult::Handled(true);
        }
    }
    CrashEventResult::Handled(false)
}

/// Whether the signal came from `kill` or `raise` rather than a fault. The
/// handler that was there before ours (the standard library's, usually)
/// would swallow such a signal, where a fault repeats itself on return;
/// claiming it has the default action end the process instead.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn sent_by_kill(context: &CrashContext) -> bool {
    context.siginfo.ssi_code <= 0
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn sent_by_kill(_: &CrashContext) -> bool {
    false
}

/// Have the helper dump this moment, as if the current thread had faulted.
/// Returns whether a dump was written.
pub(crate) fn dump_now() -> bool {
    let Some(handler) = HANDLER.get() else {
        return false;
    };
    SIMULATING.store(true, Ordering::Release);
    let done = simulate(handler);
    SIMULATING.store(false, Ordering::Release);
    done
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn simulate(handler: &CrashHandler) -> bool {
    matches!(
        handler.simulate_signal(libc::SIGABRT as u32),
        CrashEventResult::Handled(true)
    )
}

#[cfg(windows)]
fn simulate(handler: &CrashHandler) -> bool {
    matches!(
        handler.simulate_exception(None),
        CrashEventResult::Handled(true)
    )
}

#[cfg(target_os = "macos")]
fn simulate(handler: &CrashHandler) -> bool {
    handler.simulate_exception(None)
}

/// Let the helper read this process when it dumps it. Linux restricts
/// `ptrace` to a process's ancestors by default, and the helper is a child.
pub(crate) fn allow_dumps_from(pid: u32) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(handler) = HANDLER.get()
        && pid != 0
    {
        handler.set_ptracer(Some(pid));
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = pid;
}

/// The directory of the binary this code is linked into: the executable,
/// or the shared engine a host program loaded.
pub(crate) fn module_dir() -> Option<PathBuf> {
    module_path().and_then(|p| p.parent().map(PathBuf::from))
}

#[cfg(unix)]
fn module_path() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: dladdr reads the loader's records for an address inside this
    // binary and fills `info`; the file name it points at is owned by the
    // loader and copied out before return.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        let here = module_path as *const () as *const libc::c_void;
        if libc::dladdr(here, &mut info) == 0 || info.dli_fname.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(info.dli_fname);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())))
    }
}

#[cfg(windows)]
fn module_path() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    // SAFETY: the module lookup takes an address inside this binary and does
    // not change its reference count; the name is written into a buffer of
    // the size passed.
    unsafe {
        let mut module = std::ptr::null_mut();
        let here = module_path as *const () as *const u16;
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            here,
            &mut module,
        ) == 0
        {
            return None;
        }
        let mut buf = vec![0u16; 32_768];
        let len = GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        if len == 0 || len >= buf.len() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..len])))
    }
}

#[cfg(not(any(unix, windows)))]
fn module_path() -> Option<PathBuf> {
    None
}
