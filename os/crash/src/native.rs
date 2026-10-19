//! The native fault handler: signals on Linux, Mach exceptions on macOS,
//! structured exceptions on Windows, through `crash-handler`.
//!
//! On macOS a fault arrives as a Mach exception, which `crash-handler`
//! listens for; a signal that was sent (by `kill`, `raise` or `pthread_kill`)
//! raises none, so signal handlers sit beside it for those.
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
        #[cfg(target_os = "macos")]
        signals::install();
    }
}

fn on_crash(context: &CrashContext) -> CrashEventResult {
    let simulated = SIMULATING.load(Ordering::Acquire);
    // One crash, one report: a fault that went through the Mach path still
    // becomes a signal after it, and a process that dies of a signal raises
    // `EXC_CRASH` on its way out.
    #[cfg(target_os = "macos")]
    if !simulated && signals::REPORTED.swap(true, Ordering::AcqRel) {
        return CrashEventResult::Handled(false);
    }
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

/// The signal handlers macOS needs beside the Mach exception path.
///
/// A fault raises a Mach exception first, and `crash-handler` reports it
/// there; only then does the kernel turn it into a signal. A signal that was
/// sent raises no exception, so without these it would go to the handler
/// that was there before (the standard library's stack overflow handler,
/// usually), which ignores a signal that is not a fault, and the process
/// would carry on. The handlers here report such a signal through the same
/// path as a fault (the helper's dump, or the in-process report) and end the
/// process with it. For a signal that comes after a reported fault they do
/// nothing but hand it on, so the fault's own course, the standard library's
/// stack overflow message included, is as it was.
///
/// Everything here is async-signal-safe in the sense `crash-handler`'s own
/// `SIGABRT` handler is: atomics, `sigaction`, `raise`, and the Mach message
/// to the handler thread that `simulate_exception` sends.
#[cfg(target_os = "macos")]
mod signals {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crash_context::ExceptionInfo;

    /// Set once a crash has been reported, by whichever path came first.
    pub(super) static REPORTED: AtomicBool = AtomicBool::new(false);

    /// The signals a fault ends in, as on Linux. `SIGABRT` is
    /// `crash-handler`'s own.
    const SIGNALS: [libc::c_int; 5] = [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGTRAP,
    ];

    /// The action each signal had before ours, in [`SIGNALS`] order.
    static PREVIOUS: [OnceLock<libc::sigaction>; SIGNALS.len()] =
        [const { OnceLock::new() }; SIGNALS.len()];

    /// `EXC_SOFTWARE`, and its code for a Unix signal, from `<mach/exception_types.h>`
    /// and `<sys/ux_exception.h>`: how `crash-handler` reports `SIGABRT` too.
    const EXC_SOFTWARE: u32 = 5;
    const EXC_SOFT_SIGNAL: u64 = 0x10003;

    pub(super) fn install() {
        for (signal, previous) in SIGNALS.iter().zip(&PREVIOUS) {
            // SAFETY: plain `sigaction` calls on zeroed structs. The old
            // action is read and kept before ours replaces it, so a signal
            // never finds ours without the one to hand on to.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(*signal, std::ptr::null(), &mut old) != 0 {
                    continue;
                }
                let _ = previous.set(old);
                let mut action: libc::sigaction = std::mem::zeroed();
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                // On the alternate stack the standard library sets up, so a
                // stack overflow reaches its handler through ours.
                action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigaction(*signal, &action, std::ptr::null_mut());
            }
        }
    }

    extern "C" fn on_signal(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        if REPORTED.load(Ordering::Acquire) {
            hand_on(signal, info, context);
            return;
        }
        if let Some(handler) = super::HANDLER.get() {
            handler.simulate_exception(Some(ExceptionInfo {
                kind: EXC_SOFTWARE,
                code: EXC_SOFT_SIGNAL,
                subcode: Some(signal as u64),
            }));
        }
        // SAFETY: async-signal-safe calls. The signal is blocked while this
        // handler runs, so the raised one is delivered, with the default
        // action, as the handler returns.
        unsafe {
            default_action(signal);
            libc::raise(signal);
        }
    }

    /// Give `signal` to the action that was there before ours.
    fn hand_on(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        let previous = SIGNALS
            .iter()
            .position(|s| *s == signal)
            .and_then(|i| PREVIOUS[i].get());
        let Some(previous) = previous else {
            // SAFETY: async-signal-safe.
            unsafe { default_action(signal) };
            return;
        };
        let handler = previous.sa_sigaction;
        if handler == libc::SIG_DFL || handler == libc::SIG_IGN {
            // A fault comes back when the handler returns, and ends the
            // process with the default action; a sent signal is over.
            // SAFETY: async-signal-safe.
            unsafe { default_action(signal) };
        } else if previous.sa_flags & libc::SA_SIGINFO != 0 {
            // SAFETY: the previous action was installed with SA_SIGINFO, so
            // its handler takes these three arguments.
            unsafe {
                let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                    std::mem::transmute(handler);
                handler(signal, info, context);
            }
        } else {
            // SAFETY: an action without SA_SIGINFO takes the signal only.
            unsafe {
                let handler: extern "C" fn(libc::c_int) = std::mem::transmute(handler);
                handler(signal);
            }
        }
    }

    /// Put the default action back for `signal`.
    ///
    /// # Safety
    ///
    /// A `sigaction` call; safe in a signal handler.
    unsafe fn default_action(signal: libc::c_int) {
        // SAFETY: a zeroed action is SIG_DFL with an empty mask.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
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
