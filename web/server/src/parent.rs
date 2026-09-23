//! Stopping when the process that started this one is gone.
//!
//! A worker goes with its supervisor, and a `--dev` server with whatever
//! started it, such as `lumenc web --serve`, so neither is left answering on
//! a port after that process has been killed.

#[cfg(unix)]
use std::time::Duration;

use crate::server::Shutdown;

/// Stop the server once this process's parent exits.
#[cfg(unix)]
pub(crate) fn watch(stop: Shutdown) {
    // SAFETY: getppid has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new()
        .name("lumen-server-parent".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // A process whose parent exits is handed to another, so a
                // different parent means the first one is gone.
                // SAFETY: as above.
                if unsafe { libc::getppid() } != parent {
                    stop.shutdown();
                    return;
                }
            }
        });
}

/// Stop the server once this process's parent exits.
#[cfg(windows)]
pub(crate) fn watch(stop: Shutdown) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let Some(parent) = parent_id() else {
        return;
    };
    // SAFETY: OpenProcess takes plain values and returns a handle or null,
    // and a null is checked before use.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent) };
    if handle.is_null() {
        // Gone already, or not ours to watch; nothing to wait on either way.
        return;
    }
    // A handle is a pointer, which does not cross threads; its value does.
    let handle = handle as usize;
    let _ = std::thread::Builder::new()
        .name("lumen-server-parent".to_string())
        .spawn(move || {
            let handle = handle as windows_sys::Win32::Foundation::HANDLE;
            // SAFETY: the handle came from OpenProcess above and is closed
            // only here, once the wait on it has returned.
            let waited = unsafe { WaitForSingleObject(handle, INFINITE) };
            // SAFETY: as above.
            unsafe { CloseHandle(handle) };
            if waited == WAIT_OBJECT_0 {
                stop.shutdown();
            }
        });
}

/// The id of the process that started this one.
#[cfg(windows)]
fn parent_id() -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let me = std::process::id();
    // SAFETY: the snapshot handle is checked before use and closed once, and
    // the entry is a zeroed plain-data struct whose size field is set before
    // it is passed, as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == me {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}
