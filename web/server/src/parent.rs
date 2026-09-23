//! Stopping when the process that started this one is gone.
//!
//! A worker goes with its supervisor, and a server `lumenc web --serve`
//! started goes with lumenc, so neither is left answering on a port after
//! that process has been killed. Whatever starts the server asks for this by
//! putting its own process id in [`PARENT_VAR`]; a server started any other
//! way, from a shell or a service manager, watches nothing and outlives
//! whatever started it.
//!
//! The id comes from the parent rather than from asking the system who the
//! parent is, because by the time a process asks, the parent may already have
//! exited and handed it to another.

/// The variable whatever starts the server puts its own process id in, to
/// have the server stop once that process is gone. lumenc sets it by the
/// same name.
pub(crate) const PARENT_VAR: &str = "LUMEN_SERVER_PARENT";

/// What [`adopt`] found.
pub(crate) enum Adopted {
    /// Nobody asked to be watched.
    Unwatched,
    /// The parent named is running, and is watched from here on.
    Watching(Parent),
    /// The parent named has already exited.
    Gone,
}

/// The process whose exit stops this one.
pub(crate) struct Parent {
    #[cfg(all(unix, not(target_os = "linux")))]
    pid: rustix::process::Pid,
    /// A handle on the parent process, as its value: a handle is a pointer,
    /// which does not cross threads.
    #[cfg(windows)]
    handle: usize,
}

/// Read [`PARENT_VAR`] and check that the process it names is this one's
/// parent and still running.
///
/// On Linux the kernel is asked to send this process SIGTERM once the parent
/// exits, which drains it the way any SIGTERM does.
pub(crate) fn adopt() -> Adopted {
    let Some(expected) = std::env::var(PARENT_VAR)
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
    else {
        return Adopted::Unwatched;
    };
    adopt_pid(expected)
}

#[cfg(unix)]
fn adopt_pid(expected: u32) -> Adopted {
    use rustix::process::{Pid, getppid};

    let Some(expected) = i32::try_from(expected).ok().and_then(Pid::from_raw) else {
        return Adopted::Unwatched;
    };
    let alive = || getppid() == Some(expected);
    if !alive() {
        return Adopted::Gone;
    }
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Signal, set_parent_process_death_signal};
        if set_parent_process_death_signal(Some(Signal::TERM)).is_err() {
            return Adopted::Unwatched;
        }
        // Asked again after the request, so a parent that exits between the
        // first look and the request is seen here rather than missed by both.
        if alive() {
            Adopted::Watching(Parent {})
        } else {
            Adopted::Gone
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Adopted::Watching(Parent { pid: expected })
    }
}

impl Parent {
    /// Call `gone` once the parent has exited.
    ///
    /// On Linux the kernel's SIGTERM does this job, so nothing runs here.
    #[cfg(target_os = "linux")]
    pub(crate) fn watch(self, gone: impl FnOnce() + Send + 'static) {
        let _ = gone;
    }

    /// Call `gone` once the parent has exited.
    #[cfg(all(unix, not(target_os = "linux")))]
    pub(crate) fn watch(self, gone: impl FnOnce() + Send + 'static) {
        let parent = self.pid;
        let _ = std::thread::Builder::new()
            .name("lumen-server-parent".to_string())
            .spawn(move || {
                // A process whose parent exits is handed to another, so a
                // different parent means the first one is gone.
                while rustix::process::getppid() == Some(parent) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                gone();
            });
    }

    /// Call `gone` once the parent has exited.
    #[cfg(windows)]
    pub(crate) fn watch(self, gone: impl FnOnce() + Send + 'static) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

        let handle = self.handle;
        let _ = std::thread::Builder::new()
            .name("lumen-server-parent".to_string())
            .spawn(move || {
                let handle = handle as HANDLE;
                // SAFETY: the handle came from OpenProcess in `adopt_pid`, is
                // owned by this thread alone, and is closed only here, once
                // the wait on it has returned.
                let waited = unsafe { WaitForSingleObject(handle, INFINITE) };
                // SAFETY: as above.
                unsafe { CloseHandle(handle) };
                if waited == WAIT_OBJECT_0 {
                    gone();
                }
            });
    }
}

/// Open the process `expected` names and make sure it is the one that
/// started this process.
///
/// A process id is reused once its process has exited and nothing holds it
/// open, so the id alone could name an unrelated process started since. A
/// parent is created before its child, so a process created after this one
/// is not its parent, whatever its id. Once the handle is open the id cannot
/// be reused until it is closed, so the check holds from then on.
#[cfg(windows)]
fn adopt_pid(expected: u32) -> Adopted {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    /// When `process` was created, in 100 ns units since 1601.
    fn created(process: HANDLE) -> Option<u64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: `process` is a handle this process holds with query
        // rights, and each out-parameter is a FILETIME that lives across
        // the call.
        let ok =
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
        (ok != 0)
            .then(|| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }

    // SAFETY: OpenProcess takes plain values and returns a handle or null,
    // and a null is checked before use.
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            expected,
        )
    };
    if handle.is_null() {
        // No process by that id is left to open.
        return Adopted::Gone;
    }
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
    let own = created(unsafe { GetCurrentProcess() });
    // SAFETY: a zero wait on a handle this process holds.
    let exited = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
    match (created(handle), own) {
        (Some(parent), Some(own)) if parent < own && !exited => Adopted::Watching(Parent {
            handle: handle as usize,
        }),
        _ => {
            // SAFETY: the handle came from OpenProcess above and is not used
            // again.
            unsafe { CloseHandle(handle) };
            Adopted::Gone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_that_is_not_this_ones_parent_is_gone() {
        // This process's own id names a running process, but not the one
        // that started it.
        assert!(matches!(adopt_pid(std::process::id()), Adopted::Gone));
    }

    #[cfg(unix)]
    #[test]
    fn this_ones_parent_is_watched() {
        let parent = rustix::process::getppid().expect("a parent");
        let parent = u32::try_from(parent.as_raw_nonzero().get()).expect("a positive id");
        assert!(matches!(adopt_pid(parent), Adopted::Watching(_)));
        // The death signal asked for above is not one a test process keeps.
        #[cfg(target_os = "linux")]
        let _ = rustix::process::set_parent_process_death_signal(None);
    }
}
