//! Ending the helper when its app is gone.
//!
//! The socket closing is the usual sign, but an operating system can be slow
//! to tear down the connection of a process that died abruptly (Windows is,
//! at times), and an app that dies before it ever connects closes nothing. So
//! a thread also watches the app process itself, waiting on it rather than
//! polling where the platform allows, and stops the server loop when the
//! app has not connected in time or has exited.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long after the app's exit the loop keeps reading, so messages the app
/// sent just before it died are still in the report.
const DRAIN: Duration = Duration::from_millis(200);

/// Watch `pid`; set `shutdown` once it exits, or once `connect_timeout`
/// passes without `connected` being set.
pub fn spawn(
    pid: u32,
    shutdown: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    connect_timeout: Duration,
) {
    let _ = std::thread::Builder::new()
        .name("lumen-crash-watch".into())
        .spawn(move || {
            let start = Instant::now();
            while !connected.load(Ordering::Relaxed) {
                if start.elapsed() > connect_timeout || !imp::alive(pid) {
                    shutdown.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            imp::wait_for_exit(pid);
            std::thread::sleep(DRAIN);
            shutdown.store(true, Ordering::Relaxed);
        });
}

#[cfg(unix)]
mod imp {
    /// Whether a process with `pid` still exists.
    pub fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists and may be
        // signalled; nothing is delivered.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Block until `pid` exits.
    pub fn wait_for_exit(pid: u32) {
        if wait_native(pid) {
            return;
        }
        while alive(pid) {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    /// Linux: a pidfd turns readable when the process exits.
    #[cfg(target_os = "linux")]
    fn wait_native(pid: u32) -> bool {
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd or
        // -1; the fd is polled and then closed here.
        unsafe {
            let fd = libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) as libc::c_int;
            if fd < 0 {
                return false;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let mut ok = false;
            loop {
                let r = libc::poll(&mut pfd, 1, -1);
                if r >= 0 {
                    ok = r > 0;
                    break;
                }
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
            libc::close(fd);
            ok
        }
    }

    /// macOS: kqueue reports the exit of a watched process.
    #[cfg(target_os = "macos")]
    fn wait_native(pid: u32) -> bool {
        // SAFETY: one kqueue, one change registering NOTE_EXIT for the pid,
        // one blocking wait for it; the queue is closed after.
        unsafe {
            let kq = libc::kqueue();
            if kq < 0 {
                return false;
            }
            let mut change: libc::kevent = std::mem::zeroed();
            change.ident = pid as libc::uintptr_t;
            change.filter = libc::EVFILT_PROC;
            change.flags = libc::EV_ADD | libc::EV_ONESHOT;
            change.fflags = libc::NOTE_EXIT;
            let mut event: libc::kevent = std::mem::zeroed();
            let r = libc::kevent(kq, &change, 1, &mut event, 1, std::ptr::null());
            libc::close(kq);
            r > 0
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn wait_native(_pid: u32) -> bool {
        false
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    /// Whether a process with `pid` still runs.
    pub fn alive(pid: u32) -> bool {
        // SAFETY: a zero-timeout wait on a handle opened and closed here.
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return false;
            }
            let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            CloseHandle(handle);
            running
        }
    }

    /// Block until `pid` exits.
    pub fn wait_for_exit(pid: u32) {
        // SAFETY: as in `alive`, with an unbounded wait.
        unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return;
            }
            WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
        }
    }
}
