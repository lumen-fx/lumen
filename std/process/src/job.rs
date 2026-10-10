//! A Windows job object around one child, so a stop ends every program the
//! child started as well as the child itself.
//!
//! Windows has no process group a signal can reach. A job is the kernel's
//! equivalent: a process assigned to one stays in it, every process it starts
//! joins it too, and `TerminateJobObject` ends all of them at once. The job
//! sets no limits, so it does nothing until a stop asks it to; in particular
//! closing it ends nothing, and a child the app leaves running keeps running.
//!
//! This is the one module in the crate allowed `unsafe`: the three Win32 calls
//! below have no safe wrapper in std.

#![allow(unsafe_code)]

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};

/// The exit code every process in the job reports when a stop ends it: the
/// code `Child::kill` gives on Windows, so a stop reads the same with or
/// without a job.
const STOPPED: u32 = 1;

/// A job holding one child and everything it starts.
#[derive(Debug)]
pub(crate) struct Job(HANDLE);

// SAFETY: a job handle is a kernel object handle, which Windows lets any
// thread of the process use and close. `Job` owns it and closes it once, in
// `Drop`, so sharing it between the supervisor and a stop is sound.
unsafe impl Send for Job {}
// SAFETY: as above; `TerminateJobObject` may be called from several threads
// on one handle at once.
unsafe impl Sync for Job {}

impl Job {
    /// Put `child` in a job of its own. `None` when the system refuses, which
    /// leaves a stop ending the child alone, as it would without a job.
    ///
    /// The child is already running when this is called, so a program it
    /// starts before the assignment lands stays outside the job; std offers
    /// no way to start a child suspended and resume it after.
    pub(crate) fn assign(child: &Child) -> Option<Self> {
        // SAFETY: both arguments may be null: no security attributes (the
        // default descriptor, a handle the children do not inherit) and no
        // name (an unnamed job no other process can open).
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Self(handle);
        let process: HANDLE = child.as_raw_handle();
        // SAFETY: `job.0` is the job created above and `process` is the
        // child's process handle, which `child` keeps open for the length of
        // the call.
        let assigned = unsafe { AssignProcessToJobObject(job.0, process) } != 0;
        assigned.then_some(job)
    }

    /// End every process in the job. Answers false when the system refused.
    pub(crate) fn terminate(&self) -> bool {
        // SAFETY: `self.0` is a job handle this value owns and has not closed.
        unsafe { TerminateJobObject(self.0, STOPPED) != 0 }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` was opened by `CreateJobObjectW` and is closed
        // only here, once. The job sets no kill-on-close limit, so closing
        // it ends no process.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
