//! Integration coverage for the ABI 0.3 additions that need the full
//! `lumenc` runtime: eager `lumen_app_new` directory validation and the
//! `lumen_app_run_headless` tick driver. These can't live in the unit
//! test module because they build a real app (plugin stack, script host)
//! from an on-disk fixture.

use std::ffi::{CString, c_char};
use std::path::PathBuf;

use lumen::{
    LumenClickFn, LumenCloseFn, LumenFn, LumenKind, LumenStatus, LumenValue, LumenWatchFn,
    lumen_app_expose, lumen_app_free, lumen_app_new, lumen_app_on_click, lumen_app_on_close,
    lumen_app_run_headless, lumen_last_error, lumen_signal_get_str, lumen_signal_set_int64,
    lumen_signal_set_str, lumen_signal_watch,
};
use std::os::raw::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// Four of these tests build a whole app and tick it. An app is not a
/// process-local object: it publishes the DOM index, the node-handle
/// registry, the event-binding registry, the global property store and
/// the last-error slot, and its plugin stack constructs OS host
/// resources (hotkey manager, notifier, tray, clipboard) that several
/// platforms bind to one per process. libtest runs the tests in this
/// binary on parallel threads, so without this lock two or three apps
/// exist at once and take turns overwriting each other's globals; on
/// macOS the OS-side constructors trap rather than misbehave, which
/// kills the whole binary with no test output. One app at a time.
///
/// Same treatment the candela hot-reload suite and the FFI dom-query
/// pair already get.
static APP_ISOLATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn isolate() -> std::sync::MutexGuard<'static, ()> {
    APP_ISOLATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write a minimal, self-contained app fixture into a fresh temp dir.
/// MCP is disabled (`[mcp] port = 0`) so the headless run doesn't bind a
/// socket in CI.
fn write_fixture(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumen_headless_{name}_{}", std::process::id()));
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
    let _isolated = isolate();
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
    let _isolated = isolate();
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumen_empty_{}", std::process::id()));
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
    let _isolated = isolate();
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
    let _isolated = isolate();
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
    let _isolated = isolate();
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
    let _isolated = isolate();
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
        unsafe { lumen_signal_set_int64(name.as_ptr(), 42) },
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

// Records the last string value delivered to a watch callback.
static STR_WATCH_SEEN: Mutex<Option<String>> = Mutex::new(None);

extern "C" fn record_str_watch(_name: *const c_char, value: *const LumenValue, _ud: *mut c_void) {
    if value.is_null() {
        return;
    }
    let v = unsafe { &*value };
    if v.kind != LumenKind::String {
        return;
    }
    let p = unsafe { v.as_.string };
    if p.is_null() {
        return;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    *STR_WATCH_SEEN.lock().unwrap() = Some(s);
}

#[test]
fn string_signal_reaches_the_store_during_a_headless_run() {
    let dir = write_fixture("str");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(
        !handle.is_null(),
        "valid app dir must construct: {}",
        last_error()
    );

    // A signal the fixture markup does not bind, so the only writer is
    // this test. (`msg`, which the fixture's <label bind-text="msg">
    // reads, is seeded from the element's own text on the first tick.)
    let name = CString::new("embedder_note").unwrap();
    let value = CString::new("from the embedder").unwrap();
    *STR_WATCH_SEEN.lock().unwrap() = None;
    assert_eq!(
        unsafe { lumen_signal_set_str(name.as_ptr(), value.as_ptr()) },
        LumenStatus::Ok
    );

    // Read back before the run: the setter seeds the process-wide cache.
    let mut buf = [0i8; 64];
    let mut out_len: usize = 0;
    assert_eq!(
        unsafe { lumen_signal_get_str(name.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut out_len) },
        LumenStatus::Ok
    );
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_str()
            .unwrap(),
        "from the embedder"
    );

    let cb: LumenWatchFn = record_str_watch;
    assert_eq!(
        unsafe { lumen_signal_watch(name.as_ptr(), Some(cb), std::ptr::null_mut()) },
        LumenStatus::Ok
    );
    assert_eq!(
        unsafe { lumen_app_run_headless(handle, 4) },
        LumenStatus::Ok,
        "headless run should succeed: {}",
        last_error()
    );
    assert_eq!(
        STR_WATCH_SEEN.lock().unwrap().as_deref(),
        Some("from the embedder"),
        "the string setter must commit a string cell the watcher observes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn get_str_rejects_an_unset_signal() {
    let name = CString::new("never_set_str_signal").unwrap();
    let mut buf = [0i8; 8];
    let mut out_len: usize = 0;
    assert_eq!(
        unsafe { lumen_signal_get_str(name.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut out_len) },
        LumenStatus::ErrBadArg
    );
}

#[test]
fn signal_watch_rejects_null_args() {
    let _isolated = isolate();
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

// -- Exposed-callback parity across the three script hosts -----------------
//
// `lumen_app_expose` registers into every host, so the same C callback is
// callable from Rhai, Lua, and candela. Each test writes a one-file app in one
// language whose `on_start` calls the exposed function twice, feeding the first
// call's return value back into the second. The callback records the second
// call's first argument, which proves both directions of the marshaling:
// arguments reach C, and the returned `LumenValue` reaches the script.

/// Argument the exposed callback last received, one slot per host so the three
/// tests stay independent under the default parallel test runner.
static RHAI_ECHO: AtomicI64 = AtomicI64::new(i64::MIN);
static LUA_ECHO: AtomicI64 = AtomicI64::new(i64::MIN);
static CANDELA_ECHO: AtomicI64 = AtomicI64::new(i64::MIN);

/// Sum two int arguments, recording the first one into `slot`.
fn sum_into(slot: &AtomicI64, argc: std::os::raw::c_int, argv: *const LumenValue) -> LumenValue {
    let args: &[LumenValue] = if argv.is_null() || argc <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(argv, argc as usize) }
    };
    let int_at = |i: usize| -> i64 { args.get(i).map(|v| unsafe { v.as_.integer }).unwrap_or(0) };
    slot.store(int_at(0), Ordering::SeqCst);
    LumenValue {
        kind: lumen::LumenKind::Int,
        as_: lumen::LumenValueData {
            integer: int_at(0) + int_at(1),
        },
    }
}

unsafe extern "C" fn rhai_sum(
    argc: std::os::raw::c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) -> LumenValue {
    sum_into(&RHAI_ECHO, argc, argv)
}

unsafe extern "C" fn lua_sum(
    argc: std::os::raw::c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) -> LumenValue {
    sum_into(&LUA_ECHO, argc, argv)
}

unsafe extern "C" fn candela_sum(
    argc: std::os::raw::c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) -> LumenValue {
    sum_into(&CANDELA_ECHO, argc, argv)
}

/// Write an app whose markup loads `script_name`, and the script itself.
fn write_script_fixture(name: &str, script_name: &str, script: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumen_expose_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n",
    )
    .unwrap();
    let markup = format!(
        "<root>\n  <label id=\"lbl\" text=\"hi\" />\n  <script src=\"{script_name}\" />\n</root>\n"
    );
    std::fs::write(dir.join("main.lmn"), markup).unwrap();
    std::fs::write(dir.join(script_name), script).unwrap();
    dir
}

/// Register `func` under `name` on `dir`'s app and drive it headlessly.
fn run_exposed(dir: &std::path::Path, name: &str, func: LumenFn) {
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null(), "app must construct: {}", last_error());
    let cname = CString::new(name).unwrap();
    assert_eq!(
        unsafe { lumen_app_expose(handle, cname.as_ptr(), 2, Some(func), std::ptr::null_mut()) },
        LumenStatus::Ok,
        "expose should succeed: {}",
        last_error()
    );
    assert_eq!(
        unsafe { lumen_app_run_headless(handle, 3) },
        LumenStatus::Ok,
        "headless run should succeed: {}",
        last_error()
    );
}

#[test]
fn exposed_fn_is_callable_from_rhai() {
    RHAI_ECHO.store(i64::MIN, Ordering::SeqCst);
    let dir = write_script_fixture(
        "rhai",
        "main.rhai",
        "fn on_start() {\n    let a = native_sum(20, 22);\n    native_sum(a, 1);\n}\n",
    );
    run_exposed(&dir, "native_sum", rhai_sum);
    assert_eq!(
        RHAI_ECHO.load(Ordering::SeqCst),
        42,
        "the second call must receive the first call's return value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exposed_fn_is_callable_from_lua() {
    LUA_ECHO.store(i64::MIN, Ordering::SeqCst);
    let dir = write_script_fixture(
        "lua",
        "main.lua",
        "function on_start()\n    local a = native_sum(20, 22)\n    native_sum(a, 1)\nend\n",
    );
    run_exposed(&dir, "native_sum", lua_sum);
    assert_eq!(
        LUA_ECHO.load(Ordering::SeqCst),
        42,
        "the second call must receive the first call's return value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exposed_fn_is_callable_from_candela() {
    CANDELA_ECHO.store(i64::MIN, Ordering::SeqCst);
    // candela resolves host calls through a declared block, so the script
    // declares the exposed function and calls it under the `native` namespace.
    let dir = write_script_fixture(
        "candela",
        "main.cdl",
        "host \"native\" {\n    any native_sum(...);\n}\n\n\
         fn on_start() {\n    let a = native::native_sum(20, 22);\n    \
         native::native_sum(as_int(a), 1);\n}\n\nfn main() {}\n",
    );
    run_exposed(&dir, "native_sum", candela_sum);
    assert_eq!(
        CANDELA_ECHO.load(Ordering::SeqCst),
        42,
        "the second call must receive the first call's return value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn app_free_on_unrun_handle_is_safe() {
    let _isolated = isolate();
    let dir = write_fixture("freed");
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null());
    unsafe { lumen_app_free(handle) };
    let _ = std::fs::remove_dir_all(&dir);
}
