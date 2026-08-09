//! Integration coverage for the ABI 0.3 additions that need the full
//! `lumenc` runtime: eager `lumen_app_new` directory validation and the
//! `lumen_app_run_headless` tick driver. These can't live in the unit
//! test module because they build a real app (plugin stack, script host)
//! from an on-disk fixture.

use std::ffi::{CString, c_char};
use std::path::PathBuf;

use lumen_ffi::{
    LumenClickFn, LumenCloseFn, LumenStatus, LumenValue, LumenWatchFn, lumen_app_free,
    lumen_app_new, lumen_app_on_click, lumen_app_on_close, lumen_app_run_headless,
    lumen_last_error, lumen_signal_set_int64, lumen_signal_watch,
};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicI64, Ordering};

/// Write a minimal, self-contained app fixture into a fresh temp dir.
/// MCP is disabled (`[mcp] port = 0`) so the headless run doesn't bind a
/// socket in CI.
fn write_fixture(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumen_ffi_headless_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.lmn"),
        "<root>\n  <label id=\"lbl\" bind-text=\"msg\" text=\"hi\" />\n</root>\n",
    )
    .unwrap();
    dir
}

fn last_error() -> String {
    let p = unsafe { lumen_last_error() };
    if p.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }
}

#[test]
fn app_new_rejects_missing_directory() {
    let bogus = CString::new("/definitely/not/a/lumen/app/dir/xyzzy").unwrap();
    let handle = unsafe { lumen_app_new(bogus.as_ptr()) };
    assert!(
        handle.is_null(),
        "lumen_app_new must reject a nonexistent directory eagerly (ABI 0.3)"
    );
    assert!(
        !last_error().is_empty(),
        "a rejected lumen_app_new should set a last-error message"
    );
}

#[test]
fn app_new_rejects_dir_without_manifest() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumen_ffi_empty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(
        handle.is_null(),
        "a directory with neither main.lmn nor lumen.toml must be rejected"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_new_accepts_valid_dir_and_runs_headless() {
    let dir = write_fixture("basic");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(
        !handle.is_null(),
        "valid app dir must construct: {}",
        last_error()
    );

    // Drive a handful of ticks headlessly. Should build the full app,
    // run the schedule, and return OK without opening a window.
    let status = unsafe { lumen_app_run_headless(handle, 3) };
    assert_eq!(
        status,
        LumenStatus::Ok,
        "headless run should succeed: {}",
        last_error()
    );
    // handle is consumed by lumen_app_run_headless; do not free it.
    let _ = std::fs::remove_dir_all(&dir);
}

extern "C" fn noop_click(_id: *const c_char, _ud: *mut c_void) {}

#[test]
fn on_click_registration_replaces_and_rejects_nulls() {
    let dir = write_fixture("onclick");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null());

    let id = CString::new("increment").unwrap();
    let cb: LumenClickFn = noop_click;
    // First registration.
    assert_eq!(
        unsafe { lumen_app_on_click(handle, id.as_ptr(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );
    // Second registration for the same id is accepted (replaces).
    assert_eq!(
        unsafe { lumen_app_on_click(handle, id.as_ptr(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );
    // Null callback rejected.
    assert_eq!(
        unsafe { lumen_app_on_click(handle, id.as_ptr(), None, std::ptr::null_mut()) },
        LumenStatus::ErrBadArg
    );
    // Null id rejected.
    assert_eq!(
        unsafe { lumen_app_on_click(handle, std::ptr::null(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::ErrBadArg
    );
    // Null handle rejected.
    assert_eq!(
        unsafe {
            lumen_app_on_click(
                std::ptr::null_mut(),
                id.as_ptr(),
                Some(cb),
                std::ptr::null_mut(),
            )
        },
        LumenStatus::ErrInvalidHandle
    );
    // Building headless with a registered (never-fired) handler must not
    // panic - the routing system installs but no input is injected.
    assert_eq!(
        unsafe { lumen_app_run_headless(handle, 2) },
        LumenStatus::Ok
    );
    let _ = std::fs::remove_dir_all(&dir);
}

extern "C" fn allow_close(_ud: *mut c_void) -> std::os::raw::c_int {
    1
}

#[test]
fn on_close_registration_replaces_and_rejects_nulls() {
    let dir = write_fixture("onclose");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null());

    let cb: LumenCloseFn = allow_close;
    // First registration.
    assert_eq!(
        unsafe { lumen_app_on_close(handle, Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );
    // Second registration is accepted (replaces the first).
    assert_eq!(
        unsafe { lumen_app_on_close(handle, Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );
    // Null callback rejected.
    assert_eq!(
        unsafe { lumen_app_on_close(handle, None, std::ptr::null_mut()) },
        LumenStatus::ErrBadArg
    );
    // Null handle rejected.
    assert_eq!(
        unsafe { lumen_app_on_close(std::ptr::null_mut(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::ErrInvalidHandle
    );
    // Building headless with a registered close hook must not panic -
    // the router system installs, but no OS close request ever fires
    // headlessly (documented: the hook never fires under
    // lumen_app_run_headless).
    assert_eq!(
        unsafe { lumen_app_run_headless(handle, 2) },
        LumenStatus::Ok
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Records the last int value delivered to a watch callback, so the test
// can assert the subscription fired during the headless run.
static WATCH_SEEN: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn record_int_watch(_name: *const c_char, value: *const LumenValue, _ud: *mut c_void) {
    if value.is_null() {
        return;
    }
    // The fixture watches an int64 signal, delivered as a LUMEN_INT payload.
    let v = unsafe { &*value };
    WATCH_SEEN.store(unsafe { v.as_.integer }, Ordering::SeqCst);
}

#[test]
fn signal_watch_fires_during_headless_run() {
    let dir = write_fixture("watch");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(
        !handle.is_null(),
        "valid app dir must construct: {}",
        last_error()
    );

    // Seed a typed signal before the run, register a watcher for it, then
    // tick headlessly. The watcher must fire on the tick the value lands
    // in the PropertyStore (real commit-fired subscription, no polling).
    let name = CString::new("watched_count").unwrap();
    WATCH_SEEN.store(-1, Ordering::SeqCst);
    assert_eq!(
        unsafe { lumen_signal_set_int64(std::ptr::null_mut(), name.as_ptr(), 42) },
        LumenStatus::Ok
    );
    let cb: LumenWatchFn = record_int_watch;
    assert_eq!(
        unsafe { lumen_signal_watch(name.as_ptr(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );

    let status = unsafe { lumen_app_run_headless(handle, 4) };
    assert_eq!(
        status,
        LumenStatus::Ok,
        "headless run should succeed: {}",
        last_error()
    );
    assert_eq!(
        WATCH_SEEN.load(Ordering::SeqCst),
        42,
        "the watcher must fire with the seeded value once it commits to the PropertyStore"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn signal_watch_rejects_null_args() {
    let name = CString::new("watch_null_test").unwrap();
    let cb: LumenWatchFn = record_int_watch;
    // Null callback rejected.
    assert_eq!(
        unsafe { lumen_signal_watch(name.as_ptr(), None, std::ptr::null_mut()) },
        LumenStatus::ErrBadArg
    );
    // Null name rejected.
    assert_eq!(
        unsafe { lumen_signal_watch(std::ptr::null(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::ErrBadArg
    );
}

#[test]
fn app_free_on_unrun_handle_is_safe() {
    let dir = write_fixture("freed");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null());
    unsafe { lumen_app_free(handle) };
    let _ = std::fs::remove_dir_all(&dir);
}
