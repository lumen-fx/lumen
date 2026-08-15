//! Integration coverage for the ABI 0.3 additions that need the full
//! `lumenc` runtime: eager `lumen_app_new` directory validation and the
//! `lumen_app_run_headless` tick driver. These can't live in the unit
//! test module because they build a real app (plugin stack, script host)
//! from an on-disk fixture.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;

use lumen::{
    LumenArrayView, LumenClickFn, LumenCloseFn, LumenFn, LumenKind, LumenMapEntry, LumenMapView,
    LumenStatus, LumenValue, LumenValueData, LumenWatchFn, lumen_app_expose, lumen_app_free,
    lumen_app_new, lumen_app_on_click, lumen_app_on_close, lumen_app_run_headless,
    lumen_last_error, lumen_signal_get_str, lumen_signal_set_int64, lumen_signal_set_str,
    lumen_signal_watch,
};
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

extern "C" fn allow_close(_ud: *mut c_void) -> c_int {
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

/// Borrow the argument vector a [`LumenFn`] was handed.
fn args_of<'a>(argc: c_int, argv: *const LumenValue) -> &'a [LumenValue] {
    if argv.is_null() || argc <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(argv, argc as usize) }
    }
}

/// Sum two int arguments through the out-parameter, recording the first
/// one into `slot`.
fn sum_into(slot: &AtomicI64, out: *mut LumenValue, argc: c_int, argv: *const LumenValue) {
    let args = args_of(argc, argv);
    let int_at = |i: usize| -> i64 { args.get(i).map(|v| unsafe { v.as_.integer }).unwrap_or(0) };
    slot.store(int_at(0), Ordering::SeqCst);
    unsafe {
        *out = LumenValue {
            kind: LumenKind::Int,
            as_: LumenValueData {
                integer: int_at(0) + int_at(1),
            },
        };
    }
}

unsafe extern "C" fn rhai_sum(
    out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) {
    sum_into(&RHAI_ECHO, out, argc, argv);
}

unsafe extern "C" fn lua_sum(
    out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) {
    sum_into(&LUA_ECHO, out, argc, argv);
}

unsafe extern "C" fn candela_sum(
    out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) {
    sum_into(&CANDELA_ECHO, out, argc, argv);
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

/// Register every `(name, arity, func)` on `dir`'s app and drive it
/// headlessly.
fn run_exposed_many(dir: &std::path::Path, funcs: &[(&str, u32, LumenFn)]) {
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let handle = unsafe { lumen_app_new(cdir.as_ptr()) };
    assert!(!handle.is_null(), "app must construct: {}", last_error());
    for (name, arity, func) in funcs {
        let cname = CString::new(*name).unwrap();
        assert_eq!(
            unsafe {
                lumen_app_expose(
                    handle,
                    cname.as_ptr(),
                    *arity,
                    Some(*func),
                    std::ptr::null_mut(),
                )
            },
            LumenStatus::Ok,
            "expose should succeed: {}",
            last_error()
        );
    }
    assert_eq!(
        unsafe { lumen_app_run_headless(handle, 3) },
        LumenStatus::Ok,
        "headless run should succeed: {}",
        last_error()
    );
}

/// Register `func` under `name` with arity 2 and drive the app headlessly.
fn run_exposed(dir: &std::path::Path, name: &str, func: LumenFn) {
    run_exposed_many(dir, &[(name, 2, func)]);
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

// -- Every value kind through the out-parameter ----------------------------
//
// One callback writes a value of a requested kind through `out`; a second
// callback receives what the script made of it and records a description.
// Between them the pair covers each `LumenKind` the marshaling handles,
// plus the "callback leaves `out` untouched" case, which must reach the
// script as unit.

/// Description of each value the receiving callback saw, in call order.
static OBSERVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Allocate a NUL-terminated copy of `s` for the rest of the process.
///
/// Lumen reads the value the callback wrote after the callback returns, so
/// a pointer into the callback's own frame would dangle. A test-lifetime
/// allocation is the smallest thing that honours the contract.
fn leak_cstr(s: &str) -> *const c_char {
    CString::new(s).unwrap().into_raw()
}

fn int_value(i: i64) -> LumenValue {
    LumenValue {
        kind: LumenKind::Int,
        as_: LumenValueData { integer: i },
    }
}

fn str_value(s: &str) -> LumenValue {
    LumenValue {
        kind: LumenKind::String,
        as_: LumenValueData {
            string: leak_cstr(s),
        },
    }
}

/// Write a value of the kind named by the first argument through `out`.
unsafe extern "C" fn make_kind(
    out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) {
    let code = args_of(argc, argv)
        .first()
        .map(|v| unsafe { v.as_.integer })
        .unwrap_or(-1);
    let value = match code {
        0 => LumenValue {
            kind: LumenKind::Nil,
            as_: LumenValueData { integer: 0 },
        },
        1 => LumenValue {
            kind: LumenKind::Bool,
            as_: LumenValueData { boolean: 1 },
        },
        2 => int_value(7),
        3 => LumenValue {
            kind: LumenKind::Float,
            as_: LumenValueData { float_: 1.5 },
        },
        4 => str_value("lumen"),
        5 => {
            let items: &'static [LumenValue; 2] =
                Box::leak(Box::new([int_value(7), str_value("lumen")]));
            LumenValue {
                kind: LumenKind::Array,
                as_: LumenValueData {
                    array: LumenArrayView {
                        items: items.as_ptr(),
                        len: items.len(),
                    },
                },
            }
        }
        _ => {
            let entries: &'static [LumenMapEntry; 1] = Box::leak(Box::new([LumenMapEntry {
                key: leak_cstr("n"),
                value: int_value(9),
            }]));
            LumenValue {
                kind: LumenKind::Map,
                as_: LumenValueData {
                    map: LumenMapView {
                        entries: entries.as_ptr(),
                        len: entries.len(),
                    },
                },
            }
        }
    };
    unsafe { *out = value };
}

/// Record a description of the first argument, and deliberately leave
/// `out` alone: the caller must see unit.
unsafe extern "C" fn take_value(
    _out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    _ud: *mut c_void,
) {
    let described = match args_of(argc, argv).first() {
        None => "missing".to_owned(),
        Some(v) => unsafe {
            match v.kind {
                LumenKind::Nil => "nil".to_owned(),
                LumenKind::Bool => format!("bool:{}", v.as_.boolean != 0),
                LumenKind::Int => format!("int:{}", v.as_.integer),
                LumenKind::Float => format!("float:{}", v.as_.float_),
                LumenKind::String => {
                    format!("str:{}", CStr::from_ptr(v.as_.string).to_string_lossy())
                }
                LumenKind::Array => "array".to_owned(),
                LumenKind::Map => "map".to_owned(),
            }
        },
    };
    OBSERVED.lock().unwrap().push(described);
}

#[test]
fn exposed_callback_round_trips_every_value_kind() {
    OBSERVED.lock().unwrap().clear();
    let dir = write_script_fixture(
        "kinds",
        "main.rhai",
        "fn on_start() {\n\
        \x20   native_take(native_make(0));\n\
        \x20   native_take(native_make(1));\n\
        \x20   native_take(native_make(2));\n\
        \x20   native_take(native_make(3));\n\
        \x20   native_take(native_make(4));\n\
        \x20   let arr = native_make(5);\n\
        \x20   native_take(arr[0]);\n\
        \x20   native_take(arr[1]);\n\
        \x20   let m = native_make(6);\n\
        \x20   native_take(m.n);\n\
        \x20   native_take(type_of(native_take(2)));\n\
        }\n",
    );
    run_exposed_many(
        &dir,
        &[
            ("native_make", 1, make_kind),
            ("native_take", 1, take_value),
        ],
    );
    let observed = OBSERVED.lock().unwrap().clone();
    assert_eq!(
        observed,
        vec![
            "nil",
            "bool:true",
            "int:7",
            "float:1.5",
            "str:lumen",
            "int:7",
            "str:lumen",
            "int:9",
            // The inner `native_take` left `out` untouched, so the script
            // saw unit; the outer call reports what its type was.
            "int:2",
            "str:()",
        ],
        "each kind must survive the trip out through `out` and back as an argument"
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
