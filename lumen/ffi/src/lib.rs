//! Lumen C-ABI surface.
//!
//! Opaque `LumenApp` plus a tagged `LumenValue` union let any language
//! with C interop embed Lumen. The Rhai script side reaches across the
//! ABI through callbacks the embedder registers via `lumen_app_expose`.
//! No Rust panic escapes any `lumen_*` fn - every entry point wraps its
//! body in `catch_unwind` and stashes a UTF-8 message that
//! C callers read through `lumen_last_error`.
//!
//! ## W6.12 hardening
//!
//! - `user_data` no longer uses the `usize` stash trick. It now lives
//!   in [`UserData`], a `NonNull<c_void>` newtype with an explicit
//!   `unsafe impl Send + Sync` whose SAFETY comment names the
//!   embedder's contract.
//! - [`LumenStatus`] split from 5 variants into a richer error
//!   surface so C callers can branch on `ErrParse` vs `ErrCss` vs
//!   `ErrWindow` instead of one opaque `ErrRuntime`.
//! - [`LUMEN_ABI_VERSION`] + [`lumen_abi_version`] export a runtime
//!   ABI version `(major << 16) | (minor << 8) | patch`.
//! - [`lumen_last_error`] keeps its thread-local primary store but
//!   now falls back to a global `Mutex<Option<CString>>` when the
//!   thread has no error recorded. This trades a cheap lock for the
//!   common case where embedders call `lumen_app_run` on thread A
//!   and check the error on thread B.

#![allow(clippy::missing_safety_doc)]

use std::any::TypeId;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::ptr::{self, NonNull};
use std::sync::Mutex;

use lumen_core::components::Color;
use lumen_core::property_store::{
    PropertyKey, PropertyValue, external_property_snapshot, push_external_property,
};
use lumen_core::signals::{push_external_array, push_external_clear, push_external_signal};
use lumen_runtime::RunOptions;
use rhai::{Dynamic, ImmutableString};
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================
// ABI version
// ============================================================

/// Major ABI version. Bump on breaking layout/signature changes.
pub const LUMEN_ABI_MAJOR: u32 = 0;
/// Minor ABI version. Bump on additive changes (new exports, new variants at the end of enums).
///
/// 0.4 added the change-subscription surface: [`LumenWatchFn`] +
/// [`lumen_signal_watch`], a commit-fired signal-change callback wired
/// through the running app's [`PropertyStore`] dirty machinery (no
/// polling thread).
///
/// 0.5 added the graceful-shutdown hook: [`LumenCloseFn`] +
/// [`lumen_app_on_close`], a close callback that fires on the OS close
/// request (window button; Unix SIGINT/SIGTERM) *before* teardown and
/// can veto the close by returning 0.
///
/// 0.6 added file-based-pages navigation: [`lumen_navigate`],
/// [`lumen_navigate_back`], [`lumen_navigate_forward`], and
/// [`lumen_current_page`], writing the reserved `route.request` cell through
/// the shared `lumen_core::nav` bus (the same surface the script `page()`
/// builtin and the Rust SDK use).
///
/// 0.7 added the link-not-embed launcher seam: [`lumen_app_new_from_lmna`],
/// which builds a [`LumenApp`] from prebuilt LMNA artifact bytes with NO
/// parser. The thin `lumenc` launcher compiles source to LMNA bytes in-process
/// and hands them across this ABI to a dlopen'd liblumen, instead of
/// static-linking the runtime. `lumen_app_new(dir)` is unchanged (still parses
/// via the bundled parser). See `docs/design/link-not-embed.md`.
///
/// 0.8 added the dynamic DOM read side: [`LumenNode`], [`LumenNodeList`] +
/// [`lumen_nodelist_free`] / [`lumen_nodelist_get`], and the query +
/// traversal getters ([`lumen_query`], [`lumen_query_len`],
/// [`lumen_query_single`], [`lumen_get_by_id`], [`lumen_document`],
/// [`lumen_node_parent`] and siblings, [`lumen_node_children`],
/// [`lumen_node_closest`], [`lumen_node_valid`]). All read the process-shared
/// per-tick DOM snapshot; additive, so a minor bump.
///
/// 0.9 added the dynamic DOM write side + `window` / `document` / `history`:
/// [`lumen_node_set_attr`] / [`lumen_node_remove_attr`] /
/// [`lumen_node_set_text`], class-list edits
/// ([`lumen_node_class_add`] / `_remove` / `_toggle`), inline style
/// ([`lumen_node_set_style`] / [`lumen_node_remove_style`]), structure
/// ([`lumen_node_spawn`], [`lumen_node_clone`], [`lumen_node_append`],
/// [`lumen_node_insert_before`], [`lumen_node_set_parent`],
/// [`lumen_node_replace_with`], [`lumen_node_remove`]), and the window /
/// history / document entry points ([`lumen_window_set_href`],
/// [`lumen_window_reload`], [`lumen_window_set_title`],
/// [`lumen_window_set_size`], [`lumen_window_dpr`], [`lumen_history_go`],
/// [`lumen_document_spawn`]). Mutations queue on the external DOM bus the
/// runtime drains each tick; additive, so a minor bump.
///
/// 0.10 added the dynamic DOM event side (phase 4): register a C callback +
/// user data against a node and event type with [`lumen_on`]
/// (capture-phase opt-in), unbind with [`lumen_off`]. The callback receives a
/// [`LumenEvent`] (scalar fields) plus accessor functions for the string
/// fields ([`lumen_event_type`], [`lumen_event_key`], [`lumen_event_value`])
/// and the propagation controls ([`lumen_event_prevent_default`],
/// [`lumen_event_stop_propagation`], [`lumen_event_stop_immediate_propagation`]).
/// The runtime invokes registered callbacks during capture -> target ->
/// bubble propagation; additive, so a minor bump.
///
/// 0.11 added the low-level introspection read side (phase 5): post-layout
/// geometry ([`lumen_node_rect`], [`lumen_node_content_rect`],
/// [`lumen_node_scroll`], [`lumen_node_is_visible`], [`lumen_node_z_index`]),
/// full computed style / attributes / inline style / component reads as
/// key-value buffers ([`lumen_node_computed_style`], [`lumen_node_attrs`],
/// [`lumen_node_inline_style`], [`lumen_node_component`]), class / component
/// name lists ([`lumen_node_classes`], [`lumen_node_components`]), tree
/// serialization ([`lumen_node_outer_markup`], [`lumen_dump_tree`]), entity
/// id ([`lumen_node_entity_id`]), and global state ([`lumen_pointer_state`],
/// [`lumen_frame_info`], [`lumen_signals_all`]), with the
/// [`lumen_kvlist_free`] / [`lumen_strlist_free`] / [`lumen_string_free`]
/// releasers. Additive, so a minor bump.
///
/// 0.12 added guarded markup injection (phase 6): read a node's children as
/// `.lmn`-ish text with [`lumen_node_inner_markup`], and replace them from a
/// markup fragment with [`lumen_node_set_inner_markup`]. The setter parses
/// through the injected front-end (present on the from-source run path, a
/// no-op on the precompiled-artifact path) and must not be fed untrusted
/// content. Additive, so a minor bump.
pub const LUMEN_ABI_MINOR: u32 = 12;
/// Patch ABI version. Bump on non-API metadata changes (docs, code, etc.).
pub const LUMEN_ABI_PATCH: u32 = 0;

/// Packed runtime ABI version `(major << 16) | (minor << 8) | patch`.
/// Mirrored in `lumen.h` as `LUMEN_API_VERSION`. Embedders compare at
/// runtime to refuse a header / shared-library mismatch.
pub const LUMEN_ABI_VERSION: u32 =
    (LUMEN_ABI_MAJOR << 16) | (LUMEN_ABI_MINOR << 8) | LUMEN_ABI_PATCH;

/// Returns the packed ABI version this library was compiled with.
/// Compare against `LUMEN_API_VERSION` from `lumen.h` at startup.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_abi_version() -> u32 {
    LUMEN_ABI_VERSION
}

// ============================================================
// Value model
// ============================================================

/// Discriminant for [`LumenValue`].
#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LumenKind {
    /// Unit / null.
    Nil = 0,
    /// `int` 0/1 in [`LumenValueData::boolean`].
    Bool = 1,
    /// Signed 64-bit integer in [`LumenValueData::integer`].
    Int = 2,
    /// IEEE-754 double in [`LumenValueData::float_`].
    Float = 3,
    /// UTF-8, NUL-terminated, in [`LumenValueData::string`].
    String = 4,
    /// Heterogeneous array, see [`LumenArrayView`].
    Array = 5,
    /// Key->value map, see [`LumenMapView`].
    Map = 6,
}

/// Borrowed view of an array of [`LumenValue`]. Pointer must stay
/// valid for the duration of the call returning it.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LumenArrayView {
    /// Pointer to the first item; null when `len == 0`.
    pub items: *const LumenValue,
    /// Item count.
    pub len: usize,
}

/// Borrowed view of a map of [`LumenMapEntry`]. Pointer must stay
/// valid for the duration of the call returning it.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LumenMapView {
    /// Pointer to the first entry; null when `len == 0`.
    pub entries: *const LumenMapEntry,
    /// Entry count.
    pub len: usize,
}

/// Payload union for [`LumenValue`]. Read the field matching
/// [`LumenValue::kind`].
#[repr(C)]
#[derive(Copy, Clone)]
pub union LumenValueData {
    /// Boolean payload (0 = false, non-zero = true).
    pub boolean: c_int,
    /// 64-bit signed integer payload.
    pub integer: i64,
    /// 64-bit float payload.
    pub float_: f64,
    /// UTF-8, NUL-terminated string payload.
    pub string: *const c_char,
    /// Array payload.
    pub array: LumenArrayView,
    /// Map payload.
    pub map: LumenMapView,
}

/// One scalar / container value crossing the C ABI in either
/// direction. Always pass `kind` consistently with the populated
/// union field. Pointers are borrowed for the duration of the call;
/// Lumen copies before returning to Rhai.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LumenValue {
    /// Discriminant - which union field is valid.
    pub kind: LumenKind,
    /// Payload union.
    pub as_: LumenValueData,
}

/// One entry in a [`LumenMapView`]. `key` is UTF-8, NUL-terminated.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct LumenMapEntry {
    /// UTF-8, NUL-terminated key.
    pub key: *const c_char,
    /// Value.
    pub value: LumenValue,
}

/// Return code for every `lumen_*` C function.
///
/// W6.12 split the legacy 5-variant enum into a richer surface so C
/// callers can distinguish "parse failure" from "asset failure" from
/// "window backend failure" without inspecting the [`lumen_last_error`]
/// string. Numeric values are stable; new variants append.
#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LumenStatus {
    /// Operation succeeded.
    Ok = 0,
    /// A path argument was missing or could not be resolved.
    ErrBadPath = 1,
    /// A non-path argument was missing or malformed.
    ErrBadArg = 2,
    /// Generic runtime error (legacy catch-all; new code should pick a
    /// more specific variant when possible).
    ErrRuntime = 3,
    /// Internal error (Rust panic caught at the boundary).
    ErrInternal = 4,
    /// HTML / template parse failure.
    ErrParse = 5,
    /// CSS parse / cascade failure.
    ErrCss = 6,
    /// Asset load / decode failure.
    ErrAsset = 7,
    /// Window backend (winit / wgpu surface) failure.
    ErrWindow = 8,
    /// Script (Rhai) compile / runtime failure.
    ErrScript = 9,
    /// Generic I/O error (filesystem, network).
    ErrIo = 10,
    /// `lumen_*` was called with a handle that does not belong to a
    /// live `LumenApp` (use-after-free / null after move).
    ErrInvalidHandle = 11,
    /// A passed value was syntactically valid but semantically wrong
    /// (e.g. a `kind`/payload mismatch on `LumenValue`).
    ErrInvalidValue = 12,
    /// A Rust panic occurred at the boundary. Same numeric code as the
    /// legacy `ErrInternal`; kept as an alias for the rename.
    ErrPanic = 13,
    /// A caller-provided output buffer was too small. The associated
    /// `out_len` out-parameter (where the export takes one) is set to the
    /// number of bytes required, including the trailing NUL. Introduced in
    /// ABI 0.3 for the string / array read-back accessors.
    ErrBufferTooSmall = 14,
}

/// Signature of an exposed callback. `argv` is borrowed for the
/// duration of the call; the returned `LumenValue` (and any pointers
/// it carries) must stay valid until this function returns - Lumen
/// copies into a `Dynamic` before unwinding.
pub type LumenFn = unsafe extern "C" fn(
    argc: c_int,
    argv: *const LumenValue,
    user_data: *mut c_void,
) -> LumenValue;

/// Out-parameter callback variant of [`LumenFn`] (ABI 0.3).
///
/// Instead of returning a [`LumenValue`] by value - which forces every
/// non-Rust binding to hand-encode the platform's aggregate-return
/// (SysV `sret`) convention because `LumenValue` is larger than the
/// 16-byte register-pair threshold - the callback writes its result
/// through `out`. Lumen copies `*out` into a `Dynamic` before this
/// function returns, exactly as it does for the value `LumenFn` returns.
///
/// `out` is never null when Lumen invokes the callback, and points to a
/// single writable, uninitialised [`LumenValue`]. A callback that wants
/// to return nil may leave it untouched (Lumen zero-initialises the slot
/// to `LumenKind::Nil` first) or set `kind = LUMEN_NIL` explicitly. Any
/// pointers the written value carries must stay valid until the callback
/// returns. Register with [`lumen_app_expose_v2`].
pub type LumenFnV2 = unsafe extern "C" fn(
    out: *mut LumenValue,
    argc: c_int,
    argv: *const LumenValue,
    user_data: *mut c_void,
);

/// Id-scoped native click callback (ABI 0.3). Registered with
/// [`lumen_app_on_click`]; invoked once per [`ClickEvent`] whose target
/// element carries the matching `LumenId`. `id` is the element id
/// (UTF-8, NUL-terminated), borrowed for the duration of the call.
///
/// Fires on the Lumen tick thread (which may be a `bevy_ecs` worker,
/// not the thread that built the app); `user_data` carries the same
/// Send/Sync contract as [`lumen_app_expose`]'s.
pub type LumenClickFn = unsafe extern "C" fn(id: *const c_char, user_data: *mut c_void);

/// App-level close callback (ABI 0.5). Registered with
/// [`lumen_app_on_close`]; invoked once per OS close request - the
/// window close button, or (Unix) the first SIGINT/SIGTERM - *before*
/// the runtime tears anything down, so embedders get a last chance to
/// persist state.
///
/// Return nonzero to allow the close (the loop exits and `lumen_app_run`
/// returns), or 0 to veto it and keep the window open - mirroring the
/// script-side `on_close()` returning `false`. On Unix a second
/// SIGINT/SIGTERM bypasses the hook and exits immediately, so a vetoing
/// embedder cannot wedge shutdown.
///
/// Fires on the Lumen tick thread; `user_data` carries the same
/// Send/Sync contract as [`lumen_app_expose`]'s.
pub type LumenCloseFn = unsafe extern "C" fn(user_data: *mut c_void) -> c_int;

/// Signal-change subscription callback (ABI 0.4). Registered with
/// [`lumen_signal_watch`]; fires once per tick in which the watched
/// global signal's committed value changed (plus once on the first tick
/// the value is observed after the watch is registered).
///
/// `name` is the watched signal name (UTF-8, NUL-terminated). `value`
/// points to the new committed value, borrowed for the duration of the
/// call - Lumen frees it afterwards, so copy anything you keep. The
/// [`LumenValue`] mirrors the stored [`PropertyValue`]:
/// `Bool`->`LUMEN_BOOL`, `I64`->`LUMEN_INT`, `F64`->`LUMEN_FLOAT`,
/// `Str`->`LUMEN_STRING`, and `Color`->`LUMEN_INT` packed big-endian
/// `0xRRGGBBAA` (unpack channels with `(v>>24)&0xff` ... `v&0xff`). Other
/// variants (`Vec2`, `Custom`) arrive as `LUMEN_NIL`.
///
/// Fires on the Lumen tick thread (same Send/Sync `user_data` contract as
/// [`lumen_app_expose`]). Delivery is per-tick coalesced: several mid-tick
/// writes collapse into a single callback carrying the final value, mirroring
/// [`PropertyStore`]'s own GObject-style notify semantics.
pub type LumenWatchFn =
    unsafe extern "C" fn(name: *const c_char, value: *const LumenValue, user_data: *mut c_void);

// ============================================================
// Opaque app handle
// ============================================================

/// Builder + handle for one embedded Lumen application. Construct
/// with `lumen_app_new`, populate with `lumen_app_expose` /
/// `lumen_app_set_*`, run with `lumen_app_run` (which consumes the
/// handle). If you decide not to run, `lumen_app_free` drops it.
pub struct LumenApp {
    dir: PathBuf,
    /// Prebuilt LMNA artifact bytes (link-not-embed launcher path). When
    /// `Some`, the app runs from these bytes with NO parser and `dir` is used
    /// only as the base directory for relative asset resolution. `None` is the
    /// classic from-source path where `dir` names an app directory the bundled
    /// parser reads. Set by [`lumen_app_new_from_lmna`]; `None` for
    /// [`lumen_app_new`].
    artifact_bytes: Option<Vec<u8>>,
    title: Option<String>,
    size: Option<(u32, u32)>,
    exposed: Vec<ExposedFn>,
    /// Id-scoped native click handlers registered via
    /// [`lumen_app_on_click`]. Keyed on element id; a second
    /// registration for the same id replaces the first.
    click_handlers: HashMap<String, (LumenClickFn, UserData)>,
    /// App-level close hook registered via [`lumen_app_on_close`]. A
    /// second registration replaces the first.
    close_handler: Option<(LumenCloseFn, UserData)>,
}

/// Embedder-supplied opaque pointer carried across the FFI to native
/// callbacks. The Rhai engine moves the wrapping closure across
/// threads, which requires `Send + Sync` - that bound is impossible
/// to satisfy generically for `*mut c_void`, so this newtype carries
/// an explicit unsafe impl with the contract spelled out in SAFETY.
///
/// `None` represents the documented "no user data" case (the embedder
/// passes `nullptr`); we never dereference it. `NonNull` makes the
/// non-null invariant a type-system rule rather than a runtime check.
#[derive(Copy, Clone)]
pub struct UserData(Option<NonNull<c_void>>);

impl UserData {
    /// Wrap a raw embedder pointer. `None` if `p` is null.
    pub fn from_raw(p: *mut c_void) -> Self {
        Self(NonNull::new(p))
    }

    /// Recover the raw pointer for the dispatcher call. Caller must
    /// uphold all of `NonNull::as_ptr`'s usual contracts.
    pub fn as_ptr(self) -> *mut c_void {
        match self.0 {
            Some(p) => p.as_ptr(),
            None => ptr::null_mut(),
        }
    }
}

// SAFETY:
//
// `UserData` wraps an `Option<NonNull<c_void>>`. The pointer itself
// is opaque to lumen and is only ever passed back to the embedder's
// own `LumenFn` callback. The embedder is contractually responsible
// for ensuring that whatever object lives behind the pointer is safe
// to read from the Lumen script thread (where the callback fires)
// AND from any thread the script may move the closure to.
//
// This is the same contract Qt's `QObject*` user-data fields and
// GLib's `gpointer user_data` carry. Documented in `lumen.h` and the
// LumenApp expose docstring.
//
// Concretely, `Send + Sync` here means:
//   - **Send**: it is sound to move a `UserData` between threads.
//     Moving the pointer doesn't dereference it; the embedder must
//     also ensure that the *referent* tolerates being read from
//     whichever thread the callback eventually runs on.
//   - **Sync**: shared references can cross thread boundaries. Same
//     rationale: lumen never dereferences the pointer.
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

/// The native callback backing one [`ExposedFn`] - either the classic
/// by-value [`LumenFn`] (v1) or the out-parameter [`LumenFnV2`] (v2).
#[derive(Copy, Clone)]
enum ExposedPtr {
    V1(LumenFn),
    V2(LumenFnV2),
}

struct ExposedFn {
    name: String,
    fn_ptr: ExposedPtr,
    /// Embedder-supplied opaque pointer. See [`UserData`] for the
    /// Send/Sync rationale and embedder contract.
    user_data: UserData,
    arg_count: usize,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Global last-error fallback. Populated alongside the thread-local
/// store so callers that probe `lumen_last_error` on a thread that
/// did not raise the error still see *a* useful message. Documented
/// caveat: multi-threaded embedders can race on this slot; whoever
/// wrote last wins.
static GLOBAL_LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

fn set_last_error(s: impl Into<String>) {
    let s = s.into();
    if let Ok(c) = CString::new(s.clone()) {
        LAST_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(c.clone());
        });
        // Best-effort: a poisoned global mutex is recovered via
        // `into_inner_or` semantics (clone the inner Option out and
        // overwrite). The mutex contents are just a `CString`; a
        // panic in a previous critical section doesn't invalidate it.
        match GLOBAL_LAST_ERROR.lock() {
            Ok(mut g) => *g = Some(c),
            Err(poisoned) => {
                let mut g = poisoned.into_inner();
                *g = Some(c);
            }
        }
    }
}

fn catch<F>(f: F) -> LumenStatus
where
    F: FnOnce() -> LumenStatus,
{
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(s) => s,
        Err(_) => {
            set_last_error("rust panic across FFI");
            LumenStatus::ErrPanic
        }
    }
}

/// Value-returning panic guard for C entry points that hand back a plain
/// scalar (a handle, a token) rather than a [`LumenStatus`]. On a caught
/// panic it records the error and returns `fallback`, so no unwind crosses
/// the ABI.
fn catch_val<T, F>(fallback: T, f: F) -> T
where
    F: FnOnce() -> T,
{
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            set_last_error("rust panic across FFI");
            fallback
        }
    }
}

// ============================================================
// C entry points
// ============================================================

/// Allocate a new app rooted at `dir` (UTF-8, NUL-terminated). The
/// directory must exist and contain `main.lmn` and/or `lumen.toml`.
/// Returns null on error; call `lumen_last_error` for details.
///
/// ABI 0.3 made this validation eager: prior versions accepted any
/// path and only surfaced a bad directory later, inside
/// `lumen_app_run` (i.e. only after opening a window). The directory is
/// now `stat`-ed up front and its contents checked, so a bad app
/// directory fails at construction time with a null return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_new(dir: *const c_char) -> *mut LumenApp {
    let r: Result<*mut LumenApp, String> = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if dir.is_null() {
            return Err("null dir".to_string());
        }
        let s = match unsafe { CStr::from_ptr(dir) }.to_str() {
            Ok(s) => s,
            Err(_) => return Err("dir not utf-8".to_string()),
        };
        let path = PathBuf::from(s);
        // Eager directory validation (ABI 0.3). The doc promised this;
        // the implementation now delivers it.
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("lumen_app_new: cannot access app directory {s:?}: {e}"))?;
        if !meta.is_dir() {
            return Err(format!("lumen_app_new: {s:?} is not a directory"));
        }
        if !path.join("main.lmn").is_file() && !path.join("lumen.toml").is_file() {
            return Err(format!(
                "lumen_app_new: app directory {s:?} contains neither main.lmn nor lumen.toml"
            ));
        }
        Ok(Box::into_raw(Box::new(LumenApp {
            dir: path,
            artifact_bytes: None,
            title: None,
            size: None,
            exposed: Vec::new(),
            click_handlers: HashMap::new(),
            close_handler: None,
        })))
    }))
    .unwrap_or_else(|_| Err("panic in lumen_app_new".to_string()));
    match r {
        Ok(p) => p,
        Err(msg) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Allocate a new app from prebuilt LMNA artifact bytes (ABI 0.7). `data`
/// points to `len` bytes of a `lumenc`-compiled artifact (magic `LMNA`);
/// Lumen copies them in immediately, so the caller may free `data` as soon as
/// this returns. `base_dir` (UTF-8, NUL-terminated, or null) is the directory
/// relative asset paths in the artifact resolve against; null means the
/// current directory.
///
/// This is the link-not-embed launcher seam: the thin `lumenc` launcher
/// compiles source to LMNA bytes in-process (it has the parser) and hands them
/// here, so the runtime runs with NO parser and never touches a source file.
/// Contrast [`lumen_app_new`], which takes a source directory and parses via
/// the bundled parser.
///
/// Returns null on error (null/empty `data`, or `base_dir` not UTF-8); call
/// [`lumen_last_error`] for details. The artifact bytes themselves are
/// validated lazily at run time (`lumen_app_run` / `lumen_app_run_headless`),
/// where a bad magic / version surfaces as an error status.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_new_from_lmna(
    data: *const u8,
    len: usize,
    base_dir: *const c_char,
) -> *mut LumenApp {
    let r: Result<*mut LumenApp, String> = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if data.is_null() || len == 0 {
            return Err("lumen_app_new_from_lmna: null or empty LMNA data".to_string());
        }
        // Copy the caller's bytes in immediately (the pointer is borrowed only
        // for this call).
        let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        // Resolve the base dir for relative asset paths. Null -> ".".
        let dir = if base_dir.is_null() {
            PathBuf::from(".")
        } else {
            match unsafe { CStr::from_ptr(base_dir) }.to_str() {
                Ok(s) => PathBuf::from(s),
                Err(_) => return Err("lumen_app_new_from_lmna: base_dir not utf-8".to_string()),
            }
        };
        Ok(Box::into_raw(Box::new(LumenApp {
            dir,
            artifact_bytes: Some(bytes),
            title: None,
            size: None,
            exposed: Vec::new(),
            click_handlers: HashMap::new(),
            close_handler: None,
        })))
    }))
    .unwrap_or_else(|_| Err("panic in lumen_app_new_from_lmna".to_string()));
    match r {
        Ok(p) => p,
        Err(msg) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Expose a native callback to Rhai under `name`. `arg_count` is the
/// arity Rhai will dispatch against (0..=8 sensible). Pointers are
/// stored by value; the embedder owns `user_data` and must keep it
/// valid until `lumen_app_run` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_expose(
    app: *mut LumenApp,
    name: *const c_char,
    arg_count: u32,
    func: Option<LumenFn>,
    user_data: *mut c_void,
) -> LumenStatus {
    catch(|| {
        let Some(func) = func else {
            set_last_error("null fn");
            return LumenStatus::ErrBadArg;
        };
        if app.is_null() {
            set_last_error("null app");
            return LumenStatus::ErrInvalidHandle;
        }
        if name.is_null() {
            set_last_error("null name");
            return LumenStatus::ErrBadArg;
        }
        let app = unsafe { &mut *app };
        let name_str = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                set_last_error("name not utf-8");
                return LumenStatus::ErrBadArg;
            }
        };
        app.exposed.push(ExposedFn {
            name: name_str,
            fn_ptr: ExposedPtr::V1(func),
            user_data: UserData::from_raw(user_data),
            arg_count: arg_count as usize,
        });
        LumenStatus::Ok
    })
}

/// Expose a native callback to Rhai under `name`, using the
/// out-parameter callback convention (ABI 0.3).
///
/// Identical to [`lumen_app_expose`] except `func` is a [`LumenFnV2`]:
/// it receives a `*mut LumenValue` out-pointer as its first argument and
/// writes its result there instead of returning a `LumenValue` by value.
/// This lets `ctypes` / `libffi`-only bindings register callbacks
/// without hand-encoding the platform's aggregate-return (`sret`)
/// convention. Prefer this over [`lumen_app_expose`] for non-Rust
/// embedders; v1 is retained for source compatibility.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_expose_v2(
    app: *mut LumenApp,
    name: *const c_char,
    arg_count: u32,
    func: Option<LumenFnV2>,
    user_data: *mut c_void,
) -> LumenStatus {
    catch(|| {
        let Some(func) = func else {
            set_last_error("null fn");
            return LumenStatus::ErrBadArg;
        };
        if app.is_null() {
            set_last_error("null app");
            return LumenStatus::ErrInvalidHandle;
        }
        if name.is_null() {
            set_last_error("null name");
            return LumenStatus::ErrBadArg;
        }
        let app = unsafe { &mut *app };
        let name_str = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                set_last_error("name not utf-8");
                return LumenStatus::ErrBadArg;
            }
        };
        app.exposed.push(ExposedFn {
            name: name_str,
            fn_ptr: ExposedPtr::V2(func),
            user_data: UserData::from_raw(user_data),
            arg_count: arg_count as usize,
        });
        LumenStatus::Ok
    })
}

/// Register an id-scoped native click handler (ABI 0.3).
///
/// `cb` fires once per click on the element whose `LumenId` equals `id`,
/// routed by the runtime - no `main.lmn` forwarding boilerplate and no
/// per-embedder dispatch table over the global `on_click(id)` hook. A
/// second registration for the same `id` **replaces** the first. Must be
/// called before `lumen_app_run` / `lumen_app_run_headless`.
///
/// The handler fires on the Lumen tick thread; `user_data` carries the
/// same Send/Sync contract as [`lumen_app_expose`]'s. The native routing
/// coexists with any script-side `on_click(id)` handler - both observe
/// the same click.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_on_click(
    app: *mut LumenApp,
    id: *const c_char,
    cb: Option<LumenClickFn>,
    user_data: *mut c_void,
) -> LumenStatus {
    catch(|| {
        let Some(cb) = cb else {
            set_last_error("lumen_app_on_click: null callback");
            return LumenStatus::ErrBadArg;
        };
        if app.is_null() {
            set_last_error("lumen_app_on_click: null app");
            return LumenStatus::ErrInvalidHandle;
        }
        if id.is_null() {
            set_last_error("lumen_app_on_click: null id");
            return LumenStatus::ErrBadArg;
        }
        let app = unsafe { &mut *app };
        let id_str = match unsafe { CStr::from_ptr(id) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                set_last_error("lumen_app_on_click: id not utf-8");
                return LumenStatus::ErrBadArg;
            }
        };
        app.click_handlers
            .insert(id_str, (cb, UserData::from_raw(user_data)));
        LumenStatus::Ok
    })
}

/// Register an app-level close hook (ABI 0.5).
///
/// `cb` fires once per OS close request - the window close button, or
/// (Unix) the first SIGINT/SIGTERM - on the Lumen tick thread, *before*
/// the runtime tears down the window, GPU state, or script host. Return
/// nonzero to allow the close; return 0 to veto it and keep the window
/// open (the hook fires again on the next close request). A second
/// registration **replaces** the first. Must be called before
/// `lumen_app_run`. The hook never fires under `lumen_app_run_headless`
/// (no window, no OS close request).
///
/// The native hook coexists with any script-side `on_close()` - both
/// observe the same close request, and either may veto it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_on_close(
    app: *mut LumenApp,
    cb: Option<LumenCloseFn>,
    user_data: *mut c_void,
) -> LumenStatus {
    catch(|| {
        let Some(cb) = cb else {
            set_last_error("lumen_app_on_close: null callback");
            return LumenStatus::ErrBadArg;
        };
        if app.is_null() {
            set_last_error("lumen_app_on_close: null app");
            return LumenStatus::ErrInvalidHandle;
        }
        let app = unsafe { &mut *app };
        app.close_handler = Some((cb, UserData::from_raw(user_data)));
        LumenStatus::Ok
    })
}

/// One registered signal watcher: the native callback plus its opaque
/// embedder pointer.
type WatcherEntry = (LumenWatchFn, UserData);

/// Process-wide signal-change subscription registry (ABI 0.4). Keyed on
/// the global signal name; each name may carry several watchers. Read
/// every tick by the dispatch system installed in [`build_run_options`],
/// mutated by [`lumen_signal_watch`] from any thread.
static SIGNAL_WATCHERS: OnceLock<Mutex<HashMap<String, Vec<WatcherEntry>>>> = OnceLock::new();

fn signal_watchers() -> &'static Mutex<HashMap<String, Vec<WatcherEntry>>> {
    SIGNAL_WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Subscribe to changes of the global signal `name` (ABI 0.4).
///
/// `cb` fires on the Lumen tick thread once per tick in which `name`'s
/// committed [`PropertyStore`] value changed - and once on the first tick
/// the value is observed after registration, so a freshly-registered
/// watcher immediately learns the current state. This is a real
/// commit-fired subscription wired through the running app's
/// [`PropertyStore`] dirty machinery, not a background polling loop; it
/// only fires while the app is running (`lumen_app_run` /
/// `lumen_app_run_headless`).
///
/// Registration is global and independent of any `LumenApp` handle, so it
/// may be called before or after the app is built, from any thread. A
/// second `lumen_signal_watch` for the same `name` adds another watcher
/// (they do not replace one another). `user_data` carries the same
/// Send/Sync contract as [`lumen_app_expose`]'s.
///
/// Returns [`LumenStatus::ErrBadArg`] when `cb` is null or `name` is
/// null / non-UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_watch(
    name: *const c_char,
    cb: Option<LumenWatchFn>,
    user_data: *mut c_void,
) -> LumenStatus {
    catch(|| {
        let Some(cb) = cb else {
            set_last_error("lumen_signal_watch: null callback");
            return LumenStatus::ErrBadArg;
        };
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_watch: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        signal_watchers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(n)
            .or_default()
            .push((cb, UserData::from_raw(user_data)));
        LumenStatus::Ok
    })
}

/// Materialise a stored [`PropertyValue`] into a borrowed [`LumenValue`]
/// for one watch callback. A `Str` payload's backing `CString` is stashed
/// in `keep` so its pointer stays live for the duration of the call;
/// `Color` is packed into a `LUMEN_INT` as big-endian `0xRRGGBBAA`.
fn property_to_lumen(v: &PropertyValue, keep: &mut Option<CString>) -> LumenValue {
    match v {
        PropertyValue::Bool(b) => LumenValue {
            kind: LumenKind::Bool,
            as_: LumenValueData {
                boolean: *b as c_int,
            },
        },
        PropertyValue::I64(n) => LumenValue {
            kind: LumenKind::Int,
            as_: LumenValueData { integer: *n },
        },
        PropertyValue::F64(n) => LumenValue {
            kind: LumenKind::Float,
            as_: LumenValueData { float_: *n },
        },
        PropertyValue::Str(s) => {
            let cs = CString::new(s.as_ref()).unwrap_or_default();
            let ptr = cs.as_ptr();
            *keep = Some(cs);
            LumenValue {
                kind: LumenKind::String,
                as_: LumenValueData { string: ptr },
            }
        }
        PropertyValue::Color(c) => {
            let [r, g, b, a] = c.to_rgba8();
            let packed = ((r as i64) << 24) | ((g as i64) << 16) | ((b as i64) << 8) | (a as i64);
            LumenValue {
                kind: LumenKind::Int,
                as_: LumenValueData { integer: packed },
            }
        }
        PropertyValue::Vec2(_) | PropertyValue::Custom(_) => LumenValue {
            kind: LumenKind::Nil,
            as_: LumenValueData { integer: 0 },
        },
    }
}

/// Override the window title (default: derived from `lumen.toml` or
/// the directory name).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_set_title(
    app: *mut LumenApp,
    title: *const c_char,
) -> LumenStatus {
    catch(|| {
        if app.is_null() {
            return LumenStatus::ErrInvalidHandle;
        }
        if title.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let s = match unsafe { CStr::from_ptr(title) }.to_str() {
            Ok(s) => s,
            Err(_) => return LumenStatus::ErrBadArg,
        };
        unsafe { &mut *app }.title = Some(s.to_owned());
        LumenStatus::Ok
    })
}

/// Override the initial window size in logical pixels.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_set_size(app: *mut LumenApp, w: u32, h: u32) -> LumenStatus {
    catch(|| {
        if app.is_null() {
            return LumenStatus::ErrInvalidHandle;
        }
        unsafe { &mut *app }.size = Some((w, h));
        LumenStatus::Ok
    })
}

/// Drop the app handle without running. Safe to call on null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_free(app: *mut LumenApp) {
    if app.is_null() {
        return;
    }
    let _ = catch(|| {
        unsafe {
            drop(Box::from_raw(app));
        }
        LumenStatus::Ok
    });
}

/// Consume the app handle and enter the Lumen event loop. Blocks
/// until the window closes. After this returns, `app` is freed -
/// do not call `lumen_app_free` on the same pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_run(app: *mut LumenApp) -> LumenStatus {
    catch(|| {
        if app.is_null() {
            return LumenStatus::ErrInvalidHandle;
        }
        let app = unsafe { Box::from_raw(app) };
        run_inner(*app)
    })
}

/// Consume the app handle and drive `ticks` main-schedule ticks without
/// opening a window or GPU surface (ABI 0.3). After this returns, `app`
/// is freed - do not call `lumen_app_free` on the same pointer.
///
/// This is the headless / CI entry point: it builds the full app (same
/// plugin stack, scripts, and reactive bindings as `lumen_app_run`) and
/// calls `App::tick()` `ticks` times, then returns. Signal round-trips,
/// script execution, `<for>` / `<if>` reconciliation, and typed-property
/// draining all run; there is no windowing, no input source, and no
/// GPU-backed rendering. Native click handlers registered with
/// `lumen_app_on_click` will not fire (no input is injected in headless
/// mode). Pass `ticks = 0` to build-and-drop (validates the app loads).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_app_run_headless(app: *mut LumenApp, ticks: u32) -> LumenStatus {
    catch(|| {
        if app.is_null() {
            return LumenStatus::ErrInvalidHandle;
        }
        let app = unsafe { Box::from_raw(app) };
        run_headless_inner(*app, ticks)
    })
}

/// Last error message set by any `lumen_*` call on this thread.
/// Returns null if no error has been recorded on this thread AND no
/// error has been recorded globally. The pointer is valid until the
/// next `lumen_*` call on this thread that produces an error.
///
/// W6.12 added a global fallback: when the thread-local slot is
/// empty (the common multi-thread embedder mistake of writing on
/// thread A and reading on thread B), we return the most recent
/// error from any thread instead of null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_last_error() -> *const c_char {
    let tls = LAST_ERROR.with(|c| {
        c.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null())
    });
    if !tls.is_null() {
        return tls;
    }
    lumen_last_error_global()
}

/// Returns the most recent error message recorded by any thread.
/// May return null if no error has ever been recorded. The returned
/// pointer is valid until the next `lumen_*` call anywhere in the
/// process that produces an error.
///
/// Distinguished from [`lumen_last_error`] for embedders that
/// explicitly want the global slot and don't want the TLS fallback.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_last_error_global() -> *const c_char {
    // We cannot safely return a borrowed pointer out of the mutex
    // guard. Stash the CString in a separate thread-local "trampoline"
    // so the returned pointer outlives this function call.
    thread_local! {
        static GLOBAL_BUFFER: RefCell<Option<CString>> = const { RefCell::new(None) };
    }
    let snapshot: Option<CString> = match GLOBAL_LAST_ERROR.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    GLOBAL_BUFFER.with(|cell| {
        *cell.borrow_mut() = snapshot;
        cell.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null())
    })
}

// ============================================================
// Internal: Rhai <-> LumenValue marshaling
// ============================================================

/// Translate the accumulated [`LumenApp`] configuration into a
/// [`RunOptions`], installing the exposed-fn Rhai extensions and the
/// id-scoped native click router. Shared by [`lumen_app_run`] (windowed)
/// and [`lumen_app_run_headless`].
fn build_run_options(app: LumenApp) -> RunOptions {
    let LumenApp {
        dir,
        artifact_bytes,
        title,
        size,
        exposed,
        click_handlers,
        close_handler,
    } = app;

    // Two source shapes:
    //   - `artifact_bytes` present (link-not-embed launcher): run the prebuilt
    //     LMNA bytes with NO parser; `dir` is only the asset base directory.
    //   - otherwise (classic embed): inject the compiler's front-end so the
    //     runtime can parse markup / CSS from source.
    let mut opts = match artifact_bytes {
        Some(bytes) => RunOptions::new(&dir).with_artifact_bytes(bytes),
        // From-source embed path: inject the compiler's front-end so the
        // runtime can parse markup / CSS. Compiled only with `embed-parser`
        // (Part B): a trimmed static `--bundle` launcher drops lumenc and runs
        // prebuilt LMNA bytes only, so a from-source request without a parser
        // surfaces `RunError::ParserDisabled` at load.
        #[cfg(feature = "embed-parser")]
        None => RunOptions::new(&dir).with_parser(lumenc::default_parser()),
        #[cfg(not(feature = "embed-parser"))]
        None => RunOptions::new(&dir),
    };
    if let Some(t) = title {
        opts.title = Some(t);
    }
    if let Some(sz) = size {
        opts.size = sz;
    }

    for ef in exposed {
        let ExposedFn {
            name,
            fn_ptr,
            user_data,
            arg_count,
        } = ef;
        opts = opts.with_rhai_extension(move |engine| {
            let arg_types: Vec<TypeId> = std::iter::repeat_with(TypeId::of::<Dynamic>)
                .take(arg_count)
                .collect();
            engine.register_raw_fn::<Dynamic>(name, arg_types, move |_ctx, args| {
                // Hold temporary CStrings until the call returns so
                // any string arg pointers stay valid.
                let mut keep: Vec<CString> = Vec::new();
                let lvs: Vec<LumenValue> =
                    args.iter().map(|d| dyn_to_lumen(d, &mut keep)).collect();
                let rv = match fn_ptr {
                    ExposedPtr::V1(f) => unsafe {
                        f(lvs.len() as c_int, lvs.as_ptr(), user_data.as_ptr())
                    },
                    ExposedPtr::V2(f) => {
                        // Zero-initialise the out slot to nil so a callback
                        // that leaves it untouched returns unit.
                        let mut out = LumenValue {
                            kind: LumenKind::Nil,
                            as_: LumenValueData { integer: 0 },
                        };
                        unsafe {
                            f(
                                &mut out,
                                lvs.len() as c_int,
                                lvs.as_ptr(),
                                user_data.as_ptr(),
                            );
                        }
                        out
                    }
                };
                Ok(lumen_to_dyn(&rv))
            });
        });
    }

    // Id-scoped native click routing (ABI 0.3). Install a per-tick system
    // via an app hook that reads this tick's `ClickEvent`s, resolves each
    // target entity's `LumenId`, and calls the matching native handler.
    // Runs alongside (not instead of) any script-side `on_click(id)`.
    if !click_handlers.is_empty() {
        opts = opts.with_app_hook(move |app| {
            use lumen_core::prelude::{ClickEvent, LumenId, MessageReader, Query, TickStage};
            let handlers = click_handlers;
            app.add_systems(
                TickStage::Systems,
                move |mut clicks: MessageReader<ClickEvent>, ids: Query<&LumenId>| {
                    for click in clicks.read() {
                        let Ok(id) = ids.get(click.entity) else {
                            continue;
                        };
                        if let Some(&(cb, ud)) = handlers.get(id.0.as_str())
                            && let Ok(cid) = CString::new(id.0.as_str())
                        {
                            unsafe { cb(cid.as_ptr(), ud.as_ptr()) };
                        }
                    }
                },
            );
        });
    }

    // App-level close hook (ABI 0.5). The window backend emits
    // `CloseRequest { vetoed: false }` on the OS close request and runs
    // one veto tick before tearing anything down; this system observes
    // the request during that tick, calls the native hook, and - when
    // the hook returns 0 - writes `CloseRequest { vetoed: true }` so the
    // backend keeps the window open (the same veto protocol app systems
    // and the script-side `on_close()` use). Reads the buffer through a
    // `MessageCursor` because the veto write needs `ResMut` access to
    // the same `Messages<CloseRequest>` resource.
    if let Some((close_cb, close_ud)) = close_handler {
        opts = opts.with_app_hook(move |app| {
            use bevy_ecs::message::{MessageCursor, Messages};
            use lumen_core::input::CloseRequest;
            use lumen_core::prelude::TickStage;
            let mut cursor = MessageCursor::<CloseRequest>::default();
            app.add_systems(
                TickStage::Systems,
                move |mut msgs: bevy_ecs::system::ResMut<Messages<CloseRequest>>| {
                    let requests = cursor.read(&msgs).filter(|ev| !ev.vetoed).count();
                    let mut veto = false;
                    for _ in 0..requests {
                        let allow = unsafe { close_cb(close_ud.as_ptr()) };
                        if allow == 0 {
                            veto = true;
                        }
                    }
                    if veto {
                        msgs.write(CloseRequest { vetoed: true });
                    }
                },
            );
        });
    }

    // Signal-change subscription dispatch (ABI 0.4). Installs one late-tick
    // system reading the committed `PropertyStore`. It keeps a per-system
    // `last` map of the value it last delivered per name, so a change is any
    // committed value that differs from the previous delivery - the real
    // commit signal, coalesced per tick exactly like `PropertyStore`'s own
    // dirty/notify path (multiple mid-tick writes collapse to the final
    // value). No background thread: this runs inside the app's own tick.
    // Always installed; it early-returns when nothing is registered.
    opts = opts.with_app_hook(move |app| {
        use lumen_core::prelude::{PropertyKey, PropertyStore, PropertyValue, Res, TickStage};
        let mut last: HashMap<String, PropertyValue> = HashMap::new();
        app.add_systems(
            TickStage::A11ySync,
            move |store: Option<Res<PropertyStore>>| {
                let Some(store) = store else {
                    return;
                };
                // Determine which watched names changed, cloning the callback
                // set + new value out from under the registry lock so a
                // callback that re-enters `lumen_signal_watch` can't deadlock.
                let mut fires: Vec<(CString, PropertyValue, LumenWatchFn, UserData)> = Vec::new();
                {
                    let reg = signal_watchers().lock().unwrap_or_else(|e| e.into_inner());
                    if reg.is_empty() {
                        return;
                    }
                    for (name, entries) in reg.iter() {
                        let key = PropertyKey::Global(Arc::<str>::from(name.as_str()));
                        let Some(value) = store.get(&key) else {
                            continue;
                        };
                        let changed = match last.get(name) {
                            Some(prev) => !prev.eq_value(value),
                            None => true,
                        };
                        if changed {
                            last.insert(name.clone(), value.clone());
                            if let Ok(cname) = CString::new(name.as_str()) {
                                for (cb, ud) in entries {
                                    fires.push((cname.clone(), value.clone(), *cb, *ud));
                                }
                            }
                        }
                    }
                }
                for (cname, value, cb, ud) in fires {
                    let mut keep: Option<CString> = None;
                    let lv = property_to_lumen(&value, &mut keep);
                    unsafe { cb(cname.as_ptr(), &lv, ud.as_ptr()) };
                }
            },
        );
    });

    opts
}

fn run_inner(app: LumenApp) -> LumenStatus {
    let opts = build_run_options(app);
    match lumen_runtime::run_app(opts) {
        Ok(()) => LumenStatus::Ok,
        Err(e) => {
            set_last_error(format!("{e}"));
            classify_runtime_error(&format!("{e}"))
        }
    }
}

fn run_headless_inner(app: LumenApp, ticks: u32) -> LumenStatus {
    let opts = build_run_options(app);
    match lumen_runtime::run_app_headless(opts, ticks) {
        Ok(()) => LumenStatus::Ok,
        Err(e) => {
            set_last_error(format!("{e}"));
            classify_runtime_error(&format!("{e}"))
        }
    }
}

/// Map a `lumenc` runtime error message onto the richest [`LumenStatus`]
/// variant we can identify. This is best-effort textual classification;
/// `lumenc::Error` doesn't carry a stable kind discriminant today.
/// W6.12 picked these prefixes by walking lumenc's error-construction
/// sites; new error kinds default to [`LumenStatus::ErrRuntime`].
fn classify_runtime_error(msg: &str) -> LumenStatus {
    let m = msg.to_ascii_lowercase();
    if m.contains("css") || m.contains("stylesheet") {
        LumenStatus::ErrCss
    } else if m.contains("parse") || m.contains("xml") || m.contains("html") {
        LumenStatus::ErrParse
    } else if m.contains("asset") || m.contains("decode") || m.contains("png") || m.contains("svg")
    {
        LumenStatus::ErrAsset
    } else if m.contains("window") || m.contains("winit") || m.contains("surface") {
        LumenStatus::ErrWindow
    } else if m.contains("rhai") || m.contains("script") {
        LumenStatus::ErrScript
    } else if m.contains("io") || m.contains("file") || m.contains("read") || m.contains("write") {
        LumenStatus::ErrIo
    } else {
        LumenStatus::ErrRuntime
    }
}

/// Borrow a Rhai `Dynamic` argument into a `LumenValue` for one
/// callback dispatch. Strings get a temporary `CString` stashed in
/// `keep` so the C-side pointer stays live for the duration of the
/// FFI call.
fn dyn_to_lumen(d: &Dynamic, keep: &mut Vec<CString>) -> LumenValue {
    if d.is_int() {
        LumenValue {
            kind: LumenKind::Int,
            as_: LumenValueData {
                integer: d.as_int().unwrap(),
            },
        }
    } else if d.is_float() {
        LumenValue {
            kind: LumenKind::Float,
            as_: LumenValueData {
                float_: d.as_float().unwrap(),
            },
        }
    } else if d.is_bool() {
        LumenValue {
            kind: LumenKind::Bool,
            as_: LumenValueData {
                boolean: d.as_bool().unwrap() as c_int,
            },
        }
    } else if d.is_string() {
        let s = d.clone().into_immutable_string().unwrap_or_default();
        let cs = CString::new(s.as_str()).unwrap_or_default();
        let ptr = cs.as_ptr();
        keep.push(cs);
        LumenValue {
            kind: LumenKind::String,
            as_: LumenValueData { string: ptr },
        }
    } else {
        // Map array, map, and custom argument values to `LumenKind::Nil`.
        LumenValue {
            kind: LumenKind::Nil,
            as_: LumenValueData { integer: 0 },
        }
    }
}

/// Copy a `LumenValue` produced by C into an owned Rhai `Dynamic`.
/// Strings are cloned into `ImmutableString`; arrays / maps recurse;
/// nothing on the Rust side keeps a pointer into the C-side buffer
/// after this returns.
fn lumen_to_dyn(v: &LumenValue) -> Dynamic {
    match v.kind {
        LumenKind::Nil => Dynamic::UNIT,
        LumenKind::Bool => Dynamic::from(unsafe { v.as_.boolean } != 0),
        LumenKind::Int => Dynamic::from(unsafe { v.as_.integer }),
        LumenKind::Float => Dynamic::from(unsafe { v.as_.float_ }),
        LumenKind::String => {
            let p = unsafe { v.as_.string };
            if p.is_null() {
                Dynamic::from(ImmutableString::new())
            } else {
                let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
                Dynamic::from(ImmutableString::from(s))
            }
        }
        LumenKind::Array => {
            let view = unsafe { v.as_.array };
            let items: &[LumenValue] = if view.items.is_null() || view.len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(view.items, view.len) }
            };
            let arr: rhai::Array = items.iter().map(lumen_to_dyn).collect();
            Dynamic::from(arr)
        }
        LumenKind::Map => {
            let view = unsafe { v.as_.map };
            let entries: &[LumenMapEntry] = if view.entries.is_null() || view.len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(view.entries, view.len) }
            };
            let mut m = rhai::Map::new();
            for e in entries {
                let key = if e.key.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(e.key) }
                        .to_string_lossy()
                        .into_owned()
                };
                m.insert(key.into(), lumen_to_dyn(&e.value));
            }
            Dynamic::from(m)
        }
    }
}

// ============================================================
// String / array read-back caches (ABI 0.3).
//
// The legacy string setters (`lumen_signal_set_string` / `_int` / `_f64`
// / `_array` / `clear`) write into Lumen's reactive store but had no
// read-back path. These process-wide caches mirror every FFI-originated
// legacy write so `lumen_signal_get_string` / `lumen_signal_array_*` can
// answer "what did I last push into this signal" from any thread,
// before or during a run - the same pre-run cache pattern the typed
// accessors use (`TYPED_SIGNALS`).
//
// Scope note (documented in the header + SDK READMEs): these read back
// the value the *embedder* last pushed through the FFI. A write that
// originates inside the running app (a Rhai `signals.x.set(..)` or a
// two-way input binding) lands in `PropertyStore` / `ArraySignals` but
// is not mirrored here, so it is not visible to these getters. Reading
// live in-app state cross-thread would require sharing the running
// `App`'s world across the FFI (tracked with the typed-getter TODO).
// ============================================================

/// One record-shaped array-signal row: field name -> stringified value.
type ArrayRow = HashMap<String, String>;
/// FFI-local mirror of every array signal the embedder has pushed.
type ArraySignalMap = HashMap<String, Vec<ArrayRow>>;

static STRING_SIGNALS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static ARRAY_SIGNALS: OnceLock<Mutex<ArraySignalMap>> = OnceLock::new();

fn string_signals() -> &'static Mutex<HashMap<String, String>> {
    STRING_SIGNALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn array_signals() -> &'static Mutex<ArraySignalMap> {
    ARRAY_SIGNALS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record an FFI string write so `lumen_signal_get_string` can read it back.
fn cache_string_signal(name: &str, value: &str) {
    string_signals()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_owned(), value.to_owned());
}

/// Copy `value` (UTF-8) into the caller buffer following the shared
/// string-out convention: on success writes the bytes plus a trailing
/// NUL and sets `*out_len` (when non-null) to the byte length excluding
/// the NUL. When the buffer is null or too small, sets `*out_len` to the
/// required capacity (byte length + 1 for the NUL) and returns
/// [`LumenStatus::ErrBufferTooSmall`] without touching `buf`.
fn write_string_out(
    value: &str,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    let bytes = value.as_bytes();
    let needed = bytes.len() + 1; // include NUL
    if buf.is_null() || buf_len < needed {
        if !out_len.is_null() {
            unsafe { *out_len = needed };
        }
        return LumenStatus::ErrBufferTooSmall;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
        *buf.add(bytes.len()) = 0;
    }
    if !out_len.is_null() {
        unsafe { *out_len = bytes.len() };
    }
    LumenStatus::Ok
}

// ============================================================
// Direct signal mutation - DOM-style mutation without Rhai.
//
// Any thread (the C++ embedder's sampler thread, a Python ctypes
// caller, a tokio task) may call these to push a value into a Lumen
// named signal. `bind-text="cpu_label"` markup observes the new
// string the next tick. Mirrors the Rhai `signal(name).set(value)`
// path but works in apps with no `main.rhai`.
// ============================================================

/// Set a scalar signal to a UTF-8 string. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_string(
    name: *const c_char,
    value: *const c_char,
) -> LumenStatus {
    catch(|| {
        if name.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let n = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s,
            Err(_) => return LumenStatus::ErrBadArg,
        };
        let v = if value.is_null() {
            String::new()
        } else {
            match unsafe { CStr::from_ptr(value) }.to_str() {
                Ok(s) => s.to_owned(),
                Err(_) => return LumenStatus::ErrBadArg,
            }
        };
        cache_string_signal(n, &v);
        push_external_signal(n, v);
        LumenStatus::Ok
    })
}

/// Set a scalar signal to a 64-bit signed integer. Stringified.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_int(name: *const c_char, value: i64) -> LumenStatus {
    catch(|| {
        if name.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let n = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s,
            Err(_) => return LumenStatus::ErrBadArg,
        };
        let v = value.to_string();
        cache_string_signal(n, &v);
        push_external_signal(n, v);
        LumenStatus::Ok
    })
}

/// Set a scalar signal to a double. Stringified with the default
/// Rust `Display` (no rounding).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_f64(name: *const c_char, value: f64) -> LumenStatus {
    catch(|| {
        if name.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let n = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s,
            Err(_) => return LumenStatus::ErrBadArg,
        };
        let v = format!("{value}");
        cache_string_signal(n, &v);
        push_external_signal(n, v);
        LumenStatus::Ok
    })
}

// ============================================================
// File-based-pages navigation (ABI 0.6).
//
// Navigation is a command on the shared bus, NOT a per-language builtin:
// these exports write the reserved `route.request` cell through the same
// `lumen_core::nav` surface the Rhai `page()` builtin and the Rust SDK use,
// so every embedding (C/C++, Python ctypes, C# P/Invoke, plugins) reaches
// the ONE resolver. Thread-safe; callable before or during a run. The
// runtime resolves the target by longest existing `.lmn` prefix.
// ============================================================

/// Navigate the active page to `path` (UTF-8, NUL-terminated). `path` is a
/// page path (`"settings"`, `"/user/7"`, `"/"`), resolved by longest
/// existing `.lmn` prefix - not a URL scheme. Equivalent to the script
/// `page("...")` command and the Rust SDK `Signals::navigate`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_navigate(path: *const c_char) -> LumenStatus {
    catch(|| {
        if path.is_null() {
            set_last_error("lumen_navigate: null path");
            return LumenStatus::ErrBadArg;
        }
        let p = match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                set_last_error("lumen_navigate: path not utf-8");
                return LumenStatus::ErrBadArg;
            }
        };
        lumen_core::nav::navigate(p);
        LumenStatus::Ok
    })
}

/// Step one entry back in the in-memory history stack (desktop). No-op at the
/// start of history. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_navigate_back() -> LumenStatus {
    catch(|| {
        lumen_core::nav::back();
        LumenStatus::Ok
    })
}

/// Step one entry forward in the in-memory history stack (desktop). No-op at
/// the end of history. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_navigate_forward() -> LumenStatus {
    catch(|| {
        lumen_core::nav::forward();
        LumenStatus::Ok
    })
}

/// Read the current active page key into `buf` (UTF-8 + trailing NUL),
/// following the shared string-out convention: on success `*out_len` (when
/// non-null) is the byte length excluding the NUL; when `buf` is null or too
/// small, `*out_len` is set to the required capacity and
/// [`LumenStatus::ErrBufferTooSmall`] is returned. Empty before the first
/// page mounts. Thread-safe (reads the `lumen_core::nav` current-page mirror,
/// which lags a resolved navigation by at most one tick).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_current_page(
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| write_string_out(&lumen_core::nav::current(), buf, buf_len, out_len))
}

/// Replace the contents of an array signal. `value` must be a
/// `LUMEN_ARRAY` of `LUMEN_MAP` entries - each map becomes one row
/// (string->string after stringification) consumed by `<for>` markup.
/// Pointer is borrowed for the duration of the call; Lumen copies
/// immediately. Embedder may free buffers as soon as this returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_array(
    name: *const c_char,
    value: *const LumenValue,
) -> LumenStatus {
    catch(|| {
        if name.is_null() || value.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let n = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => return LumenStatus::ErrBadArg,
        };
        let v = unsafe { &*value };
        if v.kind != LumenKind::Array {
            set_last_error("lumen_signal_set_array: value.kind must be LUMEN_ARRAY");
            return LumenStatus::ErrInvalidValue;
        }
        let arr = unsafe { v.as_.array };
        let items_slice: &[LumenValue] = if arr.items.is_null() || arr.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(arr.items, arr.len) }
        };
        let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(items_slice.len());
        for row in items_slice {
            let mut map: HashMap<String, String> = HashMap::new();
            if row.kind == LumenKind::Map {
                let mv = unsafe { row.as_.map };
                let entries: &[LumenMapEntry] = if mv.entries.is_null() || mv.len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(mv.entries, mv.len) }
                };
                for e in entries {
                    let k = if e.key.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(e.key) }
                            .to_string_lossy()
                            .into_owned()
                    };
                    map.insert(k, stringify_lumen(&e.value));
                }
            }
            rows.push(map);
        }
        array_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.clone(), rows.clone());
        push_external_array(n, rows);
        LumenStatus::Ok
    })
}

/// Clear a signal (string => empty, array => empty vec).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_clear(name: *const c_char) -> LumenStatus {
    catch(|| {
        if name.is_null() {
            return LumenStatus::ErrBadArg;
        }
        let n = match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(s) => s,
            Err(_) => return LumenStatus::ErrBadArg,
        };
        cache_string_signal(n, "");
        array_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.to_owned(), Vec::new());
        push_external_clear(n);
        LumenStatus::Ok
    })
}

/// Read a scalar signal back as a UTF-8 string into a caller-provided
/// buffer (ABI 0.3).
///
/// Reads the value the embedder last pushed through the FFI string
/// setters (`lumen_signal_set_string` / `_int` / `_f64`; a `clear`
/// leaves an empty string). On success copies the value plus a trailing
/// NUL into `buf` and, when `out_len` is non-null, sets `*out_len` to the
/// byte length (excluding the NUL). When `buf` is null or `buf_len` is
/// too small, sets `*out_len` to the required capacity (byte length + 1)
/// and returns [`LumenStatus::ErrBufferTooSmall`] without writing `buf`;
/// call once with a null/zero buffer to size it, then again to fill.
///
/// Returns [`LumenStatus::ErrBadArg`] when `name` is null / non-UTF-8, or
/// when the signal has never been set through the FFI string setters. See
/// the read-back scope note above: in-app writes are not visible here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_get_string(
    _app: *mut LumenApp,
    name: *const c_char,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_get_string: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        let value = string_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&n)
            .cloned();
        match value {
            Some(v) => write_string_out(&v, buf, buf_len, out_len),
            None => {
                set_last_error("lumen_signal_get_string: no string signal by that name");
                LumenStatus::ErrBadArg
            }
        }
    })
}

/// Report the row count of an array signal (ABI 0.3).
///
/// Writes the number of rows the embedder last pushed through
/// `lumen_signal_set_array` (0 after a `clear`) into `*out_len`. Returns
/// [`LumenStatus::ErrBadArg`] when `name` / `out_len` is null, `name` is
/// non-UTF-8, or the array signal has never been set through the FFI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_array_len(
    _app: *mut LumenApp,
    name: *const c_char,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_array_len: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if out_len.is_null() {
            set_last_error("lumen_signal_array_len: null out_len");
            return LumenStatus::ErrBadArg;
        }
        let len = array_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&n)
            .map(Vec::len);
        match len {
            Some(l) => {
                unsafe { *out_len = l };
                LumenStatus::Ok
            }
            None => {
                set_last_error("lumen_signal_array_len: no array signal by that name");
                LumenStatus::ErrBadArg
            }
        }
    })
}

/// Read one field of one row of an array signal as a UTF-8 string
/// (ABI 0.3).
///
/// Rows are the record-shaped (field -> stringified value) maps pushed
/// through `lumen_signal_set_array`. Looks up `row`-th row's `field`
/// entry and copies it out following the same buffer convention as
/// [`lumen_signal_get_string`] (NUL-terminated; `ErrBufferTooSmall` with
/// `*out_len` = required capacity when `buf` is too small).
///
/// Returns [`LumenStatus::ErrBadArg`] when `name` / `field` is null or
/// non-UTF-8, the array signal is absent, `row` is out of range, or the
/// row has no such field.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_array_get_field(
    _app: *mut LumenApp,
    name: *const c_char,
    row: usize,
    field: *const c_char,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_array_get_field: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        let Some(key) = typed_signal_name(field) else {
            set_last_error("lumen_signal_array_get_field: null or non-utf8 field");
            return LumenStatus::ErrBadArg;
        };
        let value = {
            let guard = array_signals().lock().unwrap_or_else(|e| e.into_inner());
            let Some(rows) = guard.get(&n) else {
                set_last_error("lumen_signal_array_get_field: no array signal by that name");
                return LumenStatus::ErrBadArg;
            };
            let Some(row_map) = rows.get(row) else {
                set_last_error("lumen_signal_array_get_field: row index out of range");
                return LumenStatus::ErrBadArg;
            };
            row_map.get(&key).cloned()
        };
        match value {
            Some(v) => write_string_out(&v, buf, buf_len, out_len),
            None => {
                set_last_error("lumen_signal_array_get_field: no such field in row");
                LumenStatus::ErrBadArg
            }
        }
    })
}

fn stringify_lumen(v: &LumenValue) -> String {
    match v.kind {
        LumenKind::Nil => String::new(),
        LumenKind::Bool => (unsafe { v.as_.boolean } != 0).to_string(),
        LumenKind::Int => unsafe { v.as_.integer }.to_string(),
        LumenKind::Float => format!("{}", unsafe { v.as_.float_ }),
        LumenKind::String => {
            let p = unsafe { v.as_.string };
            if p.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        }
        LumenKind::Array | LumenKind::Map => String::new(),
    }
}

// ============================================================
// W7.x typed scalar accessors.
//
// Round 4 typed-signal closure: the typed setters now push directly
// into the foundation `PropertyStore` via
// `lumen_core::property_store::push_external_property`. The receiving
// `drain_external_properties` system (installed by
// `lumen-script-rhai`'s `ScriptRhaiPlugin`) lands the typed
// `PropertyValue::I64` / `F64` / `Bool` / `Color` cell on the next
// tick - no stringify-on-write, no parse-on-read.
//
// The accessors keep a thread-safe in-process map (`TYPED_SIGNALS`)
// as a *pre-run cache*: embedders that call `lumen_signal_set_int64`
// before `lumen_app_run` configure the seed values here, and the
// read path consults the cache first (so reads work before the
// PropertyStore exists). After `lumen_app_run` starts pushing the
// queued external writes into the live store, the cache continues
// to serve as the read-back surface (writes mirror into both).
//
// Architectural compromise: the FFI typed get exports CANNOT trivially
// take a read lock on the running `App`'s `PropertyStore` resource
// because the App is consumed by `lumen_app_run` and owned by the
// winit event loop for the duration of the run. Sharing a
// `Send + Sync` World handle across the FFI would require touching
// the `lumenc` runtime and the `lumen-core::app::App` ownership
// surface; both are out of scope for this round. The pragmatic
// alternative the code below implements:
//
//   - typed setters mirror to BOTH the `TYPED_SIGNALS` cache AND the
//     external typed-property channel (pre-run pushes get drained
//     once the App ticks);
//   - typed getters consult `external_property_snapshot()` first
//     (catches pending pre-run writes that haven't been drained yet),
//     then fall back to the `TYPED_SIGNALS` cache;
//   - the cache is updated by every typed set so embedders that
//     never call `lumen_app_run` (test harnesses, headless probes)
//     still see a working round-trip.
//
// TODO(round-5): expose `Arc<RwLock<App>>` from `lumen-ffi`'s
// `lumen_app_run` so post-run reads can hit the live `PropertyStore`
// directly without the channel snapshot dance. Tracked in TODO.md.
//
// Each accessor takes a `LumenApp*` first arg for API parity with
// the design doc and future-proofing. The pointer is currently only
// used for null-checking; NULL is accepted for embedders that want
// to set/read signals before constructing the `LumenApp`.
// ============================================================

use std::sync::OnceLock;

#[derive(Clone, Debug)]
enum TypedSignalValue {
    Int64(i64),
    Float64(f64),
    Bool(bool),
    Color([u8; 4]),
}

static TYPED_SIGNALS: OnceLock<Mutex<HashMap<String, TypedSignalValue>>> = OnceLock::new();

fn typed_signals() -> &'static Mutex<HashMap<String, TypedSignalValue>> {
    TYPED_SIGNALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn typed_signal_name(name: *const c_char) -> Option<String> {
    if name.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(name) }.to_str().ok()?;
    Some(s.to_owned())
}

/// Build a `PropertyKey::Global` from an interned `Arc<str>`. Helper used by
/// every typed setter so the channel send doesn't re-allocate.
fn global_key(name: &str) -> PropertyKey {
    PropertyKey::Global(Arc::<str>::from(name))
}

/// Read a typed value, consulting three caches in order:
///
/// 1. [`lumen_core::property_store::typed_property_snapshot`] -
///    the post-tick mirror of [`PropertyStore`]. Sees writes from any
///    source (ECS, script, FFI) that committed during the previous
///    tick. This is the authoritative post-run path.
/// 2. [`external_property_snapshot`] - pending bus writes not yet
///    drained into [`PropertyStore`]. Covers FFI typed-setter writes
///    that happened mid-tick before the next drain runs.
/// 3. The local `TYPED_SIGNALS` cache - authoritative pre-run, when
///    no App has been built yet. Setter writes seed this immediately.
///
/// Returns `None` when none of the three holds the key.
fn typed_read(name: &str) -> Option<TypedSignalValue> {
    let key = global_key(name);
    // (1) Post-tick mirror - sees every PropertyStore typed cell.
    if let Some(value) = lumen_core::property_store::typed_property_snapshot().remove(&key) {
        return Some(TypedSignalValue::from(value));
    }
    // (2) In-flight bus writes not yet drained.
    if let Some(value) = external_property_snapshot().remove(&key) {
        return Some(TypedSignalValue::from(value));
    }
    // (3) Pre-run / local cache.
    typed_signals()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .cloned()
}

impl From<PropertyValue> for TypedSignalValue {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::I64(n) => Self::Int64(n),
            PropertyValue::F64(n) => Self::Float64(n),
            PropertyValue::Bool(b) => Self::Bool(b),
            PropertyValue::Color(c) => Self::Color(c.to_rgba8()),
            // Strings, Vec2, Custom fall back to a sentinel - callers
            // only ever request the matching variant. The match arms in
            // each `lumen_signal_get_*` handle the mismatch by returning
            // ErrBadArg.
            PropertyValue::Str(_) | PropertyValue::Vec2(_) | PropertyValue::Custom(_) => {
                Self::Bool(false)
            }
        }
    }
}

/// Set a scalar signal to a 64-bit signed integer, typed.
///
/// Round 4 closure: pushes a `PropertyValue::I64` through the foundation
/// typed-property bus so the receiving cell in `PropertyStore` keeps the
/// typed variant (no stringify-on-write, no parse-on-read). Mirrors the
/// write into the FFI-local cache for pre-run read-back.
///
/// Prefer this over `lumen_signal_set_int` (which stringifies on the
/// Rust side and forces every reader to parse back).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_int64(
    _app: *mut LumenApp,
    name: *const c_char,
    value: i64,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_set_int64: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        typed_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.clone(), TypedSignalValue::Int64(value));
        push_external_property(global_key(&n), PropertyValue::I64(value));
        LumenStatus::Ok
    })
}

/// Read a scalar signal as a 64-bit signed integer, typed. Returns
/// [`LumenStatus::ErrBadArg`] when the signal was never set with a
/// typed setter (the legacy string-typed setters do NOT populate the
/// typed-value map).
///
/// Round 4 closure: peeks the foundation typed-property bus snapshot
/// first (catches pending pre-run writes that haven't been drained
/// yet) before falling back to the local cache.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_get_int64(
    _app: *mut LumenApp,
    name: *const c_char,
    out: *mut i64,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_get_int64: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if out.is_null() {
            set_last_error("lumen_signal_get_int64: null out pointer");
            return LumenStatus::ErrBadArg;
        }
        match typed_read(&n) {
            Some(TypedSignalValue::Int64(v)) => {
                unsafe { *out = v };
                LumenStatus::Ok
            }
            Some(TypedSignalValue::Float64(v)) => {
                unsafe { *out = v as i64 };
                LumenStatus::Ok
            }
            Some(TypedSignalValue::Bool(b)) => {
                unsafe { *out = b as i64 };
                LumenStatus::Ok
            }
            _ => LumenStatus::ErrBadArg,
        }
    })
}

/// Set a scalar signal to an IEEE-754 double, typed.
///
/// Round 4 closure: pushes `PropertyValue::F64` through the typed-property
/// bus so the `PropertyStore` cell receives the typed variant directly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_float64(
    _app: *mut LumenApp,
    name: *const c_char,
    value: f64,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_set_float64: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        typed_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.clone(), TypedSignalValue::Float64(value));
        push_external_property(global_key(&n), PropertyValue::F64(value));
        LumenStatus::Ok
    })
}

/// Read a scalar signal as an IEEE-754 double, typed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_get_float64(
    _app: *mut LumenApp,
    name: *const c_char,
    out: *mut f64,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_get_float64: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if out.is_null() {
            set_last_error("lumen_signal_get_float64: null out pointer");
            return LumenStatus::ErrBadArg;
        }
        match typed_read(&n) {
            Some(TypedSignalValue::Float64(v)) => {
                unsafe { *out = v };
                LumenStatus::Ok
            }
            Some(TypedSignalValue::Int64(v)) => {
                unsafe { *out = v as f64 };
                LumenStatus::Ok
            }
            _ => LumenStatus::ErrBadArg,
        }
    })
}

/// Set a scalar signal to a boolean, typed.
///
/// Round 4 closure: pushes `PropertyValue::Bool` through the typed-property
/// bus so the `PropertyStore` cell receives the typed variant directly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_bool(
    _app: *mut LumenApp,
    name: *const c_char,
    value: bool,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_set_bool: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        typed_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.clone(), TypedSignalValue::Bool(value));
        push_external_property(global_key(&n), PropertyValue::Bool(value));
        LumenStatus::Ok
    })
}

/// Read a scalar signal as a boolean, typed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_get_bool(
    _app: *mut LumenApp,
    name: *const c_char,
    out: *mut bool,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_get_bool: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if out.is_null() {
            set_last_error("lumen_signal_get_bool: null out pointer");
            return LumenStatus::ErrBadArg;
        }
        match typed_read(&n) {
            Some(TypedSignalValue::Bool(b)) => {
                unsafe { *out = b };
                LumenStatus::Ok
            }
            Some(TypedSignalValue::Int64(v)) => {
                unsafe { *out = v != 0 };
                LumenStatus::Ok
            }
            _ => LumenStatus::ErrBadArg,
        }
    })
}

/// Set a scalar signal to a 4-byte RGBA color (each channel in 0..=255).
/// `rgba` must point to at least 4 bytes (`R`, `G`, `B`, `A`).
///
/// Round 4 closure: pushes `PropertyValue::Color` (channels normalised
/// to `[0, 1]` floats) through the typed-property bus.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_set_color(
    _app: *mut LumenApp,
    name: *const c_char,
    rgba: *const u8,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_set_color: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if rgba.is_null() {
            set_last_error("lumen_signal_set_color: null rgba pointer");
            return LumenStatus::ErrBadArg;
        }
        let bytes = unsafe { std::slice::from_raw_parts(rgba, 4) };
        let arr = [bytes[0], bytes[1], bytes[2], bytes[3]];
        typed_signals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n.clone(), TypedSignalValue::Color(arr));
        let color = Color::rgba(
            (arr[0] as f32) / 255.0,
            (arr[1] as f32) / 255.0,
            (arr[2] as f32) / 255.0,
            (arr[3] as f32) / 255.0,
        );
        push_external_property(global_key(&n), PropertyValue::Color(color));
        LumenStatus::Ok
    })
}

/// Read a scalar signal as a 4-byte RGBA color. `out` must point to at
/// least 4 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signal_get_color(
    _app: *mut LumenApp,
    name: *const c_char,
    out: *mut u8,
) -> LumenStatus {
    catch(|| {
        let Some(n) = typed_signal_name(name) else {
            set_last_error("lumen_signal_get_color: null or non-utf8 name");
            return LumenStatus::ErrBadArg;
        };
        if out.is_null() {
            set_last_error("lumen_signal_get_color: null out pointer");
            return LumenStatus::ErrBadArg;
        }
        match typed_read(&n) {
            Some(TypedSignalValue::Color(c)) => {
                let dst = unsafe { std::slice::from_raw_parts_mut(out, 4) };
                dst.copy_from_slice(&c);
                LumenStatus::Ok
            }
            _ => LumenStatus::ErrBadArg,
        }
    })
}

/// Returns a static, NUL-terminated UTF-8 description of `status`. Useful for
/// log messages on a non-OK return without an `lumen_last_error` round-trip
/// (which carries the thread-local context message instead of the status
/// enum's canonical name). The returned pointer lives for the program's
/// lifetime; callers must not free it.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_status_message(status: LumenStatus) -> *const c_char {
    let s: &'static [u8] = match status {
        LumenStatus::Ok => b"ok\0",
        LumenStatus::ErrBadPath => b"bad path argument\0",
        LumenStatus::ErrBadArg => b"bad argument\0",
        LumenStatus::ErrRuntime => b"runtime error\0",
        LumenStatus::ErrInternal => b"internal error\0",
        LumenStatus::ErrParse => b"parse error\0",
        LumenStatus::ErrCss => b"css error\0",
        LumenStatus::ErrAsset => b"asset error\0",
        LumenStatus::ErrWindow => b"window backend error\0",
        LumenStatus::ErrScript => b"script error\0",
        LumenStatus::ErrIo => b"io error\0",
        LumenStatus::ErrInvalidHandle => b"invalid handle\0",
        LumenStatus::ErrInvalidValue => b"invalid value\0",
        LumenStatus::ErrPanic => b"rust panic across ffi\0",
        LumenStatus::ErrBufferTooSmall => b"output buffer too small\0",
    };
    s.as_ptr() as *const c_char
}

// ============================================================
// Dynamic DOM read side (ABI 0.8): query / get_by_id / traversal
//
// A `LumenNode` is a packed handle (`0` = no node). All calls read the
// process-shared per-tick DOM snapshot the runtime publishes each frame,
// so they take no `LumenApp` handle (matching `lumen_navigate`). Selector
// grammar is the CSS Selectors-4 subset the cascade matcher accepts.
// ============================================================

/// Opaque packed node handle. `0` means "no node".
pub type LumenNode = u64;

/// Owned list of node handles returned by a query / children call. Free
/// with [`lumen_nodelist_free`]; index with [`lumen_nodelist_get`].
#[repr(C)]
pub struct LumenNodeList {
    /// Heap pointer to `len` contiguous [`LumenNode`] handles, or null
    /// when `len == 0`.
    pub ptr: *mut LumenNode,
    /// Number of handles.
    pub len: usize,
}

fn empty_node_list() -> LumenNodeList {
    LumenNodeList {
        ptr: ptr::null_mut(),
        len: 0,
    }
}

fn node_list_from(nodes: Vec<u64>) -> LumenNodeList {
    if nodes.is_empty() {
        return empty_node_list();
    }
    let mut boxed = nodes.into_boxed_slice();
    let list = LumenNodeList {
        ptr: boxed.as_mut_ptr(),
        len: boxed.len(),
    };
    std::mem::forget(boxed);
    list
}

fn ffi_selector(selector: *const c_char, ctx: &str) -> Result<String, LumenStatus> {
    if selector.is_null() {
        set_last_error(format!("{ctx}: null selector"));
        return Err(LumenStatus::ErrBadArg);
    }
    match unsafe { CStr::from_ptr(selector) }.to_str() {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => {
            set_last_error(format!("{ctx}: selector not utf-8"));
            Err(LumenStatus::ErrBadArg)
        }
    }
}

/// Run a CSS selector against the current DOM snapshot, writing the
/// matches (document order) into `*out_list`. On success the caller owns
/// the list and must release it with [`lumen_nodelist_free`]. A bad
/// selector returns [`LumenStatus::ErrCss`]. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_query(
    selector: *const c_char,
    out_list: *mut LumenNodeList,
) -> LumenStatus {
    catch(|| {
        if out_list.is_null() {
            set_last_error("lumen_query: null out_list");
            return LumenStatus::ErrBadArg;
        }
        let sel = match ffi_selector(selector, "lumen_query") {
            Ok(s) => s,
            Err(status) => return status,
        };
        match lumen_script::node_query::run_query(&sel) {
            Ok(q) => {
                unsafe { *out_list = node_list_from(q.nodes) };
                LumenStatus::Ok
            }
            Err(e) => {
                set_last_error(format!("lumen_query: {e}"));
                LumenStatus::ErrCss
            }
        }
    })
}

/// Number of matches for `selector`, written to `*out_len`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_query_len(
    selector: *const c_char,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| {
        if out_len.is_null() {
            set_last_error("lumen_query_len: null out_len");
            return LumenStatus::ErrBadArg;
        }
        let sel = match ffi_selector(selector, "lumen_query_len") {
            Ok(s) => s,
            Err(status) => return status,
        };
        match lumen_script::node_query::run_query(&sel) {
            Ok(q) => {
                unsafe { *out_len = q.len() };
                LumenStatus::Ok
            }
            Err(e) => {
                set_last_error(format!("lumen_query_len: {e}"));
                LumenStatus::ErrCss
            }
        }
    })
}

/// Bevy `single()` contract: succeed only when `selector` matches exactly
/// one node, writing it to `*out`. Zero or many matches returns
/// [`LumenStatus::ErrBadArg`] (and sets `*out` to `0`). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_query_single(
    selector: *const c_char,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_query_single: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = 0 };
        let sel = match ffi_selector(selector, "lumen_query_single") {
            Ok(s) => s,
            Err(status) => return status,
        };
        match lumen_script::node_query::run_query(&sel) {
            Ok(q) => match q.single() {
                Ok(node) => {
                    unsafe { *out = node };
                    LumenStatus::Ok
                }
                Err(msg) => {
                    set_last_error(format!("lumen_query_single: {msg}"));
                    LumenStatus::ErrBadArg
                }
            },
            Err(e) => {
                set_last_error(format!("lumen_query_single: {e}"));
                LumenStatus::ErrCss
            }
        }
    })
}

/// Fast id lookup. Writes the matching node to `*out`, or `0` when no
/// element carries `id`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_get_by_id(id: *const c_char, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_get_by_id: null out");
            return LumenStatus::ErrBadArg;
        }
        let id = match ffi_selector(id, "lumen_get_by_id") {
            Ok(s) => s,
            Err(status) => return status,
        };
        unsafe { *out = lumen_script::node_query::run_get_by_id(&id).unwrap_or(0) };
        LumenStatus::Ok
    })
}

/// Write the document root node to `*out` (`0` before the first tick).
/// Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_document(out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_document: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = lumen_script::node_query::run_document().unwrap_or(0) };
        LumenStatus::Ok
    })
}

/// Shared body for the single-handle traversal getters: resolve `node`,
/// apply `f`, write the result (`0` when absent) to `*out`.
fn node_relation(
    node: LumenNode,
    out: *mut LumenNode,
    ctx: &str,
    f: impl FnOnce(u64) -> Option<u64>,
) -> LumenStatus {
    if out.is_null() {
        set_last_error(format!("{ctx}: null out"));
        return LumenStatus::ErrBadArg;
    }
    unsafe { *out = f(node).unwrap_or(0) };
    LumenStatus::Ok
}

/// Parent of `node` (`0` for a root or unknown handle). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_parent(node: LumenNode, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        node_relation(
            node,
            out,
            "lumen_node_parent",
            lumen_script::node_query::node_parent,
        )
    })
}

/// First child of `node` (`0` when none). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_first_child(
    node: LumenNode,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        node_relation(
            node,
            out,
            "lumen_node_first_child",
            lumen_script::node_query::node_first_child,
        )
    })
}

/// Last child of `node` (`0` when none). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_last_child(
    node: LumenNode,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        node_relation(
            node,
            out,
            "lumen_node_last_child",
            lumen_script::node_query::node_last_child,
        )
    })
}

/// Next sibling of `node` (`0` when none). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_next(node: LumenNode, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        node_relation(
            node,
            out,
            "lumen_node_next",
            lumen_script::node_query::node_next,
        )
    })
}

/// Previous sibling of `node` (`0` when none). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_prev(node: LumenNode, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        node_relation(
            node,
            out,
            "lumen_node_prev",
            lumen_script::node_query::node_prev,
        )
    })
}

/// Children of `node` in document order, written to `*out_list` (own +
/// free with [`lumen_nodelist_free`]). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_children(
    node: LumenNode,
    out_list: *mut LumenNodeList,
) -> LumenStatus {
    catch(|| {
        if out_list.is_null() {
            set_last_error("lumen_node_children: null out_list");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out_list = node_list_from(lumen_script::node_query::node_children(node)) };
        LumenStatus::Ok
    })
}

/// Nearest ancestor-or-self of `node` matching `selector`, written to
/// `*out` (`0` when none). Bad selector returns [`LumenStatus::ErrCss`].
/// Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_closest(
    node: LumenNode,
    selector: *const c_char,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_closest: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = 0 };
        let sel = match ffi_selector(selector, "lumen_node_closest") {
            Ok(s) => s,
            Err(status) => return status,
        };
        match lumen_script::node_query::node_closest(node, &sel) {
            Ok(hit) => {
                unsafe { *out = hit.unwrap_or(0) };
                LumenStatus::Ok
            }
            Err(e) => {
                set_last_error(format!("lumen_node_closest: {e}"));
                LumenStatus::ErrCss
            }
        }
    })
}

/// Whether `node` is present in the current snapshot (`1`) or not (`0`),
/// written to `*out`. The snapshot rebuilds each tick, so a despawned node
/// reads `0`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_valid(node: LumenNode, out: *mut c_int) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_valid: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = lumen_script::node_query::node_valid(node) as c_int };
        LumenStatus::Ok
    })
}

/// Read the handle at `index` in `list`, written to `*out`. Out-of-range
/// (or null list) returns [`LumenStatus::ErrBadArg`]. The iteration
/// primitive: walk `0..list.len`. Thread-safe.
///
/// # Safety
/// `list` must be a list returned by a query / children call and not yet
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_nodelist_get(
    list: LumenNodeList,
    index: usize,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_nodelist_get: null out");
            return LumenStatus::ErrBadArg;
        }
        if list.ptr.is_null() || index >= list.len {
            set_last_error("lumen_nodelist_get: index out of range");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = *list.ptr.add(index) };
        LumenStatus::Ok
    })
}

/// Release a [`LumenNodeList`] returned by a query / children call.
/// Double-free / freeing a non-Lumen list is undefined; call once.
///
/// # Safety
/// `list` must come from a Lumen query / children call and not have been
/// freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_nodelist_free(list: LumenNodeList) {
    if list.ptr.is_null() || list.len == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            list.ptr, list.len,
        )));
    }
}

// ============================================================
// Low-level introspection (phase 5), over the C-ABI. Read-only over the
// per-tick snapshot; no `LumenApp` handle. `computed_style` / `attrs` /
// `component` / `signals_all` return owned key-value buffers freed with
// `lumen_kvlist_free`; `classes` / `components` return an owned string
// buffer freed with `lumen_strlist_free`; `dump_tree` / `outer_markup`
// return an owned C string freed with `lumen_string_free`.
// ============================================================

/// Post-layout box (design 4.7 `rect()` / `content_rect()`). Local `x` / `y`
/// are relative to the parent; `client_*` are window coordinates.
#[repr(C)]
pub struct LumenRect {
    /// Local x.
    pub x: f64,
    /// Local y.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
    /// Window-space x.
    pub client_x: f64,
    /// Window-space y.
    pub client_y: f64,
}

/// Scroll offsets + travel limits (`scroll()`).
#[repr(C)]
pub struct LumenScroll {
    /// Horizontal offset.
    pub x: f64,
    /// Vertical offset.
    pub y: f64,
    /// Max horizontal offset.
    pub max_x: f64,
    /// Max vertical offset.
    pub max_y: f64,
}

/// Pointer state snapshot (`pointer_state()`).
#[repr(C)]
pub struct LumenPointerState {
    /// Window-space x.
    pub x: f64,
    /// Window-space y.
    pub y: f64,
    /// Non-zero while the pointer is inside the window.
    pub inside: c_int,
    /// Bit 0 set while the primary button is held.
    pub buttons: u32,
    /// Shift held.
    pub shift: c_int,
    /// Control held.
    pub ctrl: c_int,
    /// Alt held.
    pub alt: c_int,
    /// Super / Command held.
    pub super_: c_int,
}

/// Per-frame counters (`frame_info()`).
#[repr(C)]
pub struct LumenFrameInfo {
    /// Monotonic tick counter.
    pub frame: u64,
    /// Milliseconds since the previous frame.
    pub dt_ms: f64,
    /// Layout-dirty element count.
    pub dirty_count: u64,
}

/// One `(key, value)` pair in a [`LumenKVList`]. Both are owned, UTF-8,
/// NUL-terminated strings freed by [`lumen_kvlist_free`].
#[repr(C)]
pub struct LumenKV {
    /// Owned key.
    pub key: *mut c_char,
    /// Owned value.
    pub value: *mut c_char,
}

/// Owned key-value buffer returned by the string-map introspection reads.
/// Free with [`lumen_kvlist_free`].
#[repr(C)]
pub struct LumenKVList {
    /// Heap pointer to `len` pairs, or null when `len == 0`.
    pub ptr: *mut LumenKV,
    /// Number of pairs.
    pub len: usize,
}

/// Owned string buffer returned by `classes` / `components`. Free with
/// [`lumen_strlist_free`].
#[repr(C)]
pub struct LumenStrList {
    /// Heap pointer to `len` owned C strings, or null when `len == 0`.
    pub ptr: *mut *mut c_char,
    /// Number of strings.
    pub len: usize,
}

fn owned_cstring(s: &str) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

fn kvlist_from(pairs: Vec<(String, String)>) -> LumenKVList {
    if pairs.is_empty() {
        return LumenKVList {
            ptr: ptr::null_mut(),
            len: 0,
        };
    }
    let mut boxed: Box<[LumenKV]> = pairs
        .into_iter()
        .map(|(k, v)| LumenKV {
            key: owned_cstring(&k),
            value: owned_cstring(&v),
        })
        .collect();
    let list = LumenKVList {
        ptr: boxed.as_mut_ptr(),
        len: boxed.len(),
    };
    std::mem::forget(boxed);
    list
}

fn strlist_from(items: Vec<String>) -> LumenStrList {
    if items.is_empty() {
        return LumenStrList {
            ptr: ptr::null_mut(),
            len: 0,
        };
    }
    let mut boxed: Box<[*mut c_char]> = items.iter().map(|s| owned_cstring(s)).collect();
    let list = LumenStrList {
        ptr: boxed.as_mut_ptr(),
        len: boxed.len(),
    };
    std::mem::forget(boxed);
    list
}

fn rect_to_ffi(r: lumen_script::introspect::NodeRect) -> LumenRect {
    LumenRect {
        x: r.x as f64,
        y: r.y as f64,
        width: r.width as f64,
        height: r.height as f64,
        client_x: r.client_x as f64,
        client_y: r.client_y as f64,
    }
}

/// Post-layout border-box of `node`, written to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_rect(node: LumenNode, out: *mut LumenRect) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_rect: null out");
            return LumenStatus::ErrBadArg;
        }
        match lumen_script::introspect::node_rect(node) {
            Some(r) => {
                unsafe { *out = rect_to_ffi(r) };
                LumenStatus::Ok
            }
            None => LumenStatus::ErrInvalidHandle,
        }
    })
}

/// Content-box (inner box minus padding + border) of `node`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_content_rect(
    node: LumenNode,
    out: *mut LumenRect,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_content_rect: null out");
            return LumenStatus::ErrBadArg;
        }
        match lumen_script::introspect::node_content_rect(node) {
            Some(r) => {
                unsafe { *out = rect_to_ffi(r) };
                LumenStatus::Ok
            }
            None => LumenStatus::ErrInvalidHandle,
        }
    })
}

/// Scroll offsets + limits of `node`, written to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_scroll(node: LumenNode, out: *mut LumenScroll) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_scroll: null out");
            return LumenStatus::ErrBadArg;
        }
        match lumen_script::introspect::node_scroll(node) {
            Some(s) => {
                unsafe {
                    *out = LumenScroll {
                        x: s.x as f64,
                        y: s.y as f64,
                        max_x: s.max_x as f64,
                        max_y: s.max_y as f64,
                    }
                };
                LumenStatus::Ok
            }
            None => LumenStatus::ErrInvalidHandle,
        }
    })
}

/// Effective visibility of `node` (`1` / `0`), written to `*out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_is_visible(node: LumenNode, out: *mut c_int) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_is_visible: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = c_int::from(lumen_script::introspect::node_is_visible(node)) };
        LumenStatus::Ok
    })
}

/// Resolved stacking order of `node`, written to `*out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_z_index(node: LumenNode, out: *mut c_int) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_z_index: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = lumen_script::introspect::node_z_index(node) as c_int };
        LumenStatus::Ok
    })
}

/// Raw `(index, generation)` of `node`, written to `*out_index` / `*out_gen`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_entity_id(
    node: LumenNode,
    out_index: *mut u32,
    out_gen: *mut u32,
) -> LumenStatus {
    catch(|| {
        if out_index.is_null() || out_gen.is_null() {
            set_last_error("lumen_node_entity_id: null out");
            return LumenStatus::ErrBadArg;
        }
        match lumen_script::introspect::node_entity_id(node) {
            Some((index, generation)) => {
                unsafe {
                    *out_index = index;
                    *out_gen = generation;
                }
                LumenStatus::Ok
            }
            None => LumenStatus::ErrInvalidHandle,
        }
    })
}

/// Full computed style of `node` as an owned key-value buffer. Free with
/// [`lumen_kvlist_free`]. An inspection call. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_computed_style(
    node: LumenNode,
    out: *mut LumenKVList,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_computed_style: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = kvlist_from(lumen_script::introspect::node_computed_style_map(node)) };
        LumenStatus::Ok
    })
}

/// Full attribute map of `node`. Free with [`lumen_kvlist_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_attrs(node: LumenNode, out: *mut LumenKVList) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_attrs: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = kvlist_from(lumen_script::introspect::node_attrs(node)) };
        LumenStatus::Ok
    })
}

/// Inline-style override map of `node`. Free with [`lumen_kvlist_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_inline_style(
    node: LumenNode,
    out: *mut LumenKVList,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_inline_style: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = kvlist_from(lumen_script::introspect::node_inline_style(node)) };
        LumenStatus::Ok
    })
}

/// Field map of `node`'s `name` component. Free with [`lumen_kvlist_free`].
/// A non-whitelisted component name returns [`LumenStatus::ErrBadArg`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_component(
    node: LumenNode,
    name: *const c_char,
    out: *mut LumenKVList,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_component: null out");
            return LumenStatus::ErrBadArg;
        }
        let name = match ffi_selector(name, "lumen_node_component") {
            Ok(s) => s,
            Err(status) => return status,
        };
        match lumen_script::introspect::node_component(node, &name) {
            Ok(map) => {
                unsafe { *out = kvlist_from(map.unwrap_or_default()) };
                LumenStatus::Ok
            }
            Err(e) => {
                set_last_error(format!("lumen_node_component: {e}"));
                LumenStatus::ErrBadArg
            }
        }
    })
}

/// Class list of `node`. Free with [`lumen_strlist_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_classes(
    node: LumenNode,
    out: *mut LumenStrList,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_classes: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = strlist_from(lumen_script::introspect::node_classes(node)) };
        LumenStatus::Ok
    })
}

/// Names of the whitelisted components present on `node`. Free with
/// [`lumen_strlist_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_components(
    node: LumenNode,
    out: *mut LumenStrList,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_components: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = strlist_from(lumen_script::introspect::node_components(node)) };
        LumenStatus::Ok
    })
}

/// Serialize `node`'s subtree to `.lmn`-ish text. Owned C string, free with
/// [`lumen_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_outer_markup(
    node: LumenNode,
    out: *mut *mut c_char,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_outer_markup: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = owned_cstring(&lumen_script::introspect::outer_markup(node)) };
        LumenStatus::Ok
    })
}

/// Serialize `node`'s children (not the node itself) to `.lmn`-ish text --
/// the read half of [`lumen_node_set_inner_markup`]. Owned C string, free
/// with [`lumen_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_inner_markup(
    node: LumenNode,
    out: *mut *mut c_char,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_inner_markup: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = owned_cstring(&lumen_script::introspect::inner_markup(node)) };
        LumenStatus::Ok
    })
}

/// Whole-tree structural dump. Owned C string, free with
/// [`lumen_string_free`]. An inspection call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_dump_tree(out: *mut *mut c_char) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_dump_tree: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = owned_cstring(&lumen_script::introspect::dump_tree()) };
        LumenStatus::Ok
    })
}

/// Current pointer state, written to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_pointer_state(out: *mut LumenPointerState) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_pointer_state: null out");
            return LumenStatus::ErrBadArg;
        }
        let p = lumen_script::introspect::pointer_state();
        unsafe {
            *out = LumenPointerState {
                x: p.x as f64,
                y: p.y as f64,
                inside: c_int::from(p.inside),
                buttons: p.buttons,
                shift: c_int::from(p.shift),
                ctrl: c_int::from(p.ctrl),
                alt: c_int::from(p.alt),
                super_: c_int::from(p.super_),
            }
        };
        LumenStatus::Ok
    })
}

/// Current frame counters, written to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_frame_info(out: *mut LumenFrameInfo) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_frame_info: null out");
            return LumenStatus::ErrBadArg;
        }
        let f = lumen_script::introspect::frame_info();
        unsafe {
            *out = LumenFrameInfo {
                frame: f.frame,
                dt_ms: f.dt_ms,
                dirty_count: f.dirty_count,
            }
        };
        LumenStatus::Ok
    })
}

/// The whole signal set as an owned key-value buffer. Free with
/// [`lumen_kvlist_free`]. An inspection call. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_signals_all(out: *mut LumenKVList) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_signals_all: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = kvlist_from(lumen_script::introspect::signals_all()) };
        LumenStatus::Ok
    })
}

/// Release a [`LumenKVList`] returned by an introspection read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_kvlist_free(list: LumenKVList) {
    if list.ptr.is_null() || list.len == 0 {
        return;
    }
    unsafe {
        let slice = Box::from_raw(std::ptr::slice_from_raw_parts_mut(list.ptr, list.len));
        for kv in slice.iter() {
            if !kv.key.is_null() {
                drop(CString::from_raw(kv.key));
            }
            if !kv.value.is_null() {
                drop(CString::from_raw(kv.value));
            }
        }
    }
}

/// Release a [`LumenStrList`] returned by `classes` / `components`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_strlist_free(list: LumenStrList) {
    if list.ptr.is_null() || list.len == 0 {
        return;
    }
    unsafe {
        let slice = Box::from_raw(std::ptr::slice_from_raw_parts_mut(list.ptr, list.len));
        for s in slice.iter() {
            if !s.is_null() {
                drop(CString::from_raw(*s));
            }
        }
    }
}

/// Release an owned C string returned by `dump_tree` / `outer_markup`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe { drop(CString::from_raw(s)) };
}

// ============================================================
// Dynamic DOM mutation (phases 2 + 3) + `window` / `document` /
// `history` (section 4.8), over the C-ABI.
//
// Mutations are fire-and-forget: each pushes a command onto the
// process-global external DOM bus, which the runtime drains into the same
// applier as script-issued mutations, so a `spawn` + chained edits from a C
// caller materialize together in one tick. Every mutator returns
// `LumenStatus` (no panic crosses the ABI); a fluent SDK wrapper returns the
// same `LumenNode` it passed in. `spawn` / `clone` write the new handle to
// an out-param.
// ============================================================

fn push_dom(cmd: lumen_script::ScriptCommand) -> LumenStatus {
    lumen_script::node_query::push_external_dom_command(cmd);
    LumenStatus::Ok
}

/// Set an attribute on `node`. KNOWN attrs (`id` / `class` / `text` /
/// `disabled`) route to their typed component; others land in the generic
/// attribute map. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_set_attr(
    node: LumenNode,
    name: *const c_char,
    value: *const c_char,
) -> LumenStatus {
    catch(|| {
        let name = match ffi_selector(name, "lumen_node_set_attr") {
            Ok(s) => s,
            Err(status) => return status,
        };
        let value = match ffi_selector(value, "lumen_node_set_attr") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::SetAttr { node, name, value })
    })
}

/// Remove an attribute from `node`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_remove_attr(
    node: LumenNode,
    name: *const c_char,
) -> LumenStatus {
    catch(|| {
        let name = match ffi_selector(name, "lumen_node_remove_attr") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::RemoveAttr { node, name })
    })
}

/// Replace `node`'s text content. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_set_text(node: LumenNode, text: *const c_char) -> LumenStatus {
    catch(|| {
        let text = match ffi_selector(text, "lumen_node_set_text") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::SetNodeText { node, text })
    })
}

/// Replace `node`'s children with the subtree parsed from `markup`
/// (`element.innerHTML = ...`). Parsed by the injected front-end and spawned
/// through the same path the `<for>` reconciler uses; a no-op on the
/// precompiled-artifact path (no parser linked). Guarded: do NOT feed
/// untrusted content -- this injects live markup. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_set_inner_markup(
    node: LumenNode,
    markup: *const c_char,
) -> LumenStatus {
    catch(|| {
        let markup = match ffi_selector(markup, "lumen_node_set_inner_markup") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::SetInnerMarkup { node, markup })
    })
}

/// Add one class to `node`'s class list. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_class_add(
    node: LumenNode,
    class: *const c_char,
) -> LumenStatus {
    catch(|| {
        let class = match ffi_selector(class, "lumen_node_class_add") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::ClassAdd { node, class })
    })
}

/// Remove one class from `node`'s class list. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_class_remove(
    node: LumenNode,
    class: *const c_char,
) -> LumenStatus {
    catch(|| {
        let class = match ffi_selector(class, "lumen_node_class_remove") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::ClassRemove { node, class })
    })
}

/// Toggle one class on `node`'s class list. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_class_toggle(
    node: LumenNode,
    class: *const c_char,
) -> LumenStatus {
    catch(|| {
        let class = match ffi_selector(class, "lumen_node_class_toggle") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::ClassToggle { node, class })
    })
}

/// Set an inline style property on `node` (`element.style`). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_set_style(
    node: LumenNode,
    name: *const c_char,
    value: *const c_char,
) -> LumenStatus {
    catch(|| {
        let name = match ffi_selector(name, "lumen_node_set_style") {
            Ok(s) => s,
            Err(status) => return status,
        };
        let value = match ffi_selector(value, "lumen_node_set_style") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::SetStyleProp { node, name, value })
    })
}

/// Remove an inline style property from `node`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_remove_style(
    node: LumenNode,
    name: *const c_char,
) -> LumenStatus {
    catch(|| {
        let name = match ffi_selector(name, "lumen_node_remove_style") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::RemoveStyleProp { node, name })
    })
}

/// Create a fresh detached element with markup tag `tag`, writing its handle
/// to `*out`. The handle is valid for the rest of the tick; attach it with
/// [`lumen_node_append`] / [`lumen_node_set_parent`]. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_spawn(tag: *const c_char, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_spawn: null out");
            return LumenStatus::ErrBadArg;
        }
        let tag = match ffi_selector(tag, "lumen_node_spawn") {
            Ok(s) => s,
            Err(status) => return status,
        };
        let (handle, cmd) = lumen_script::node_query::build_spawn(&tag);
        unsafe { *out = handle };
        push_dom(cmd)
    })
}

/// Deep-clone `source`'s subtree into a fresh detached node, writing its
/// handle to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_clone(source: LumenNode, out: *mut LumenNode) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_node_clone: null out");
            return LumenStatus::ErrBadArg;
        }
        let (handle, cmd) = lumen_script::node_query::build_clone(source);
        unsafe { *out = handle };
        push_dom(cmd)
    })
}

/// Append `child` under `parent` (`appendChild`). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_append(parent: LumenNode, child: LumenNode) -> LumenStatus {
    catch(|| {
        push_dom(lumen_script::ScriptCommand::Insert {
            parent,
            node: child,
            before: 0,
        })
    })
}

/// Insert `child` under `parent` before `reference` (`insertBefore`).
/// A `reference` of `0` appends. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_insert_before(
    parent: LumenNode,
    child: LumenNode,
    reference: LumenNode,
) -> LumenStatus {
    catch(|| {
        push_dom(lumen_script::ScriptCommand::Insert {
            parent,
            node: child,
            before: reference,
        })
    })
}

/// Attach `node` under `parent` (`node.set_parent` / reparent). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_set_parent(node: LumenNode, parent: LumenNode) -> LumenStatus {
    catch(|| {
        push_dom(lumen_script::ScriptCommand::Insert {
            parent,
            node,
            before: 0,
        })
    })
}

/// Replace `old` with `new` in `old`'s parent, despawning `old`'s subtree.
/// Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_replace_with(old: LumenNode, new: LumenNode) -> LumenStatus {
    catch(|| push_dom(lumen_script::ScriptCommand::ReplaceWith { old, new }))
}

/// Detach and despawn `node` and its subtree (`node.remove`). Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_node_remove(node: LumenNode) -> LumenStatus {
    catch(|| push_dom(lumen_script::ScriptCommand::RemoveNode { node }))
}

/// `window.set_href` -- navigate to a page path. Binds onto the same
/// [`lumen_core::nav`] bus as [`lumen_navigate`]. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_window_set_href(path: *const c_char) -> LumenStatus {
    catch(|| {
        let path = match ffi_selector(path, "lumen_window_set_href") {
            Ok(s) => s,
            Err(status) => return status,
        };
        lumen_core::nav::navigate(path);
        LumenStatus::Ok
    })
}

/// `window.reload` -- re-navigate to the current page. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_window_reload() -> LumenStatus {
    catch(|| {
        lumen_core::nav::navigate(lumen_core::nav::current());
        LumenStatus::Ok
    })
}

/// `window.set_title`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_window_set_title(title: *const c_char) -> LumenStatus {
    catch(|| {
        let title = match ffi_selector(title, "lumen_window_set_title") {
            Ok(s) => s,
            Err(status) => return status,
        };
        push_dom(lumen_script::ScriptCommand::WindowSetTitle { title })
    })
}

/// `window.set_size` in logical pixels. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_window_set_size(width: f32, height: f32) -> LumenStatus {
    catch(|| push_dom(lumen_script::ScriptCommand::WindowSetSize { width, height }))
}

/// `window.dpr` -- current device-pixel ratio, written to `*out`.
/// Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_window_dpr(out: *mut f32) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_window_dpr: null out");
            return LumenStatus::ErrBadArg;
        }
        unsafe { *out = lumen_core::window_state::dpr() };
        LumenStatus::Ok
    })
}

/// `history.go(delta)` -- step `delta` entries (negative back, positive
/// forward) through the in-memory history stack. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_history_go(delta: c_int) -> LumenStatus {
    catch(|| {
        for _ in 0..delta.unsigned_abs() {
            if delta < 0 {
                lumen_core::nav::back();
            } else {
                lumen_core::nav::forward();
            }
        }
        LumenStatus::Ok
    })
}

/// `document.spawn(tag)` -- document-scoped create verb; writes the new
/// handle to `*out`. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_document_spawn(
    tag: *const c_char,
    out: *mut LumenNode,
) -> LumenStatus {
    catch(|| {
        if out.is_null() {
            set_last_error("lumen_document_spawn: null out");
            return LumenStatus::ErrBadArg;
        }
        let tag = match ffi_selector(tag, "lumen_document_spawn") {
            Ok(s) => s,
            Err(status) => return status,
        };
        let (handle, cmd) = lumen_script::node_query::build_spawn(&tag);
        unsafe { *out = handle };
        push_dom(cmd)
    })
}

// ============================================================
// Dynamic DOM events (phase 4), over the C-ABI.
//
// Register a C callback + user data against a node and event type with
// `lumen_on`; unbind with `lumen_off`. During capture -> target -> bubble
// propagation the runtime invokes the callback, passing a `LumenEvent` with
// the scalar fields; the string fields (type / key / value) and the
// propagation controls are reached through the accessor functions, which
// read + mutate the current event. Unlike the mutators, `lumen_on` registers
// synchronously (the callback lives in a process-global binding registry the
// dispatcher shares) so no `LumenApp` handle or command drain is involved.
// ============================================================

/// Off token returned by [`lumen_on`]; pass to [`lumen_off`] to unbind.
pub type LumenEventToken = u64;

/// Scalar snapshot of the event delivered to a [`LumenEventFn`]. The string
/// fields (type / key / value) are read separately via [`lumen_event_type`] /
/// [`lumen_event_key`] / [`lumen_event_value`]. `#[repr(C)]` (never packed):
/// the fields are naturally aligned.
#[repr(C)]
pub struct LumenEvent {
    /// Target node (packed handle).
    pub target: LumenNode,
    /// Node whose handler is currently running (packed handle).
    pub current_target: LumenNode,
    /// Pointer x relative to the target, logical pixels.
    pub local_x: f64,
    /// Pointer y relative to the target, logical pixels.
    pub local_y: f64,
    /// Pointer x in window coordinates, logical pixels.
    pub client_x: f64,
    /// Pointer y in window coordinates, logical pixels.
    pub client_y: f64,
    /// Wheel delta x, logical pixels.
    pub delta_x: f64,
    /// Wheel delta y, logical pixels.
    pub delta_y: f64,
    /// Pointer button (`0` primary, `1` middle, `2` secondary, `-1` none).
    pub button: i64,
    /// Shift held (`0` / `1`).
    pub shift: u8,
    /// Control held (`0` / `1`).
    pub ctrl: u8,
    /// Alt held (`0` / `1`).
    pub alt: u8,
    /// Super / Cmd held (`0` / `1`).
    pub super_: u8,
}

/// C callback invoked when a bound event fires. `event` is borrowed for the
/// duration of the call; copy anything retained. `user_data` is the pointer
/// passed to [`lumen_on`].
pub type LumenEventFn = unsafe extern "C" fn(event: *const LumenEvent, user_data: *mut c_void);

/// Sendable capture of a C callback + its user data. The pointers are only
/// dereferenced on the main (dispatch) thread; the newtype asserts the
/// send/sync the binding registry requires.
struct CEventCallback {
    callback: LumenEventFn,
    user_data: *mut c_void,
}

// SAFETY: the callback + user_data are only invoked from the runtime's
// single-threaded event dispatch. The embedder owns thread-safety of the
// user_data it hands over, matching every other C callback in this crate.
unsafe impl Send for CEventCallback {}
unsafe impl Sync for CEventCallback {}

/// Build the scalar [`LumenEvent`] from the current-event cell.
fn current_lumen_event() -> LumenEvent {
    use lumen_script::event;
    let (lx, ly) = event::event_position_local();
    let (cx, cy) = event::event_position_client();
    let (dx, dy) = event::event_delta();
    let (shift, ctrl, alt, super_) = event::event_modifiers();
    LumenEvent {
        target: event::event_target(),
        current_target: event::event_current_target(),
        local_x: lx,
        local_y: ly,
        client_x: cx,
        client_y: cy,
        delta_x: dx,
        delta_y: dy,
        button: event::event_button(),
        shift: shift as u8,
        ctrl: ctrl as u8,
        alt: alt as u8,
        super_: super_ as u8,
    }
}

/// Bind `callback` to `node` for `event_type`. `capture` (non-zero) makes it
/// a capture-phase listener. Returns an off token (`0` on a bad argument);
/// unbind with [`lumen_off`]. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_on(
    node: LumenNode,
    event_type: *const c_char,
    capture: c_int,
    callback: Option<LumenEventFn>,
    user_data: *mut c_void,
) -> LumenEventToken {
    catch_val(0u64, || {
        let Some(callback) = callback else {
            set_last_error("lumen_on: null callback");
            return 0;
        };
        let etype = match ffi_selector(event_type, "lumen_on") {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let cb = CEventCallback {
            callback,
            user_data,
        };
        let invoke: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
            // Force whole-struct capture (edition-2021 closures otherwise
            // capture `cb.user_data` disjointly, defeating the Send/Sync
            // marker on `CEventCallback`).
            let cb = &cb;
            let ev = current_lumen_event();
            // SAFETY: invoked on the dispatch thread; `cb` outlives the call.
            unsafe { (cb.callback)(&ev, cb.user_data) };
        });
        lumen_script::event::register_native_binding(node, etype, capture != 0, invoke)
    })
}

/// Unbind a callback previously registered with [`lumen_on`]. No-op for an
/// unknown token. Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_off(token: LumenEventToken) -> LumenStatus {
    catch(|| {
        lumen_script::event::unregister_binding(token);
        LumenStatus::Ok
    })
}

/// Copy the current event's type name into `buf` (string-out convention:
/// `*out_len` excludes the NUL; too-small returns
/// [`LumenStatus::ErrBufferTooSmall`] with the required capacity). Valid only
/// inside a [`LumenEventFn`] callback. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_event_type(
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| write_string_out(&lumen_script::event::event_type(), buf, buf_len, out_len))
}

/// Copy the current event's `key` (keyboard events) into `buf`. See
/// [`lumen_event_type`] for the convention. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_event_key(
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| write_string_out(&lumen_script::event::event_key(), buf, buf_len, out_len))
}

/// Copy the current event's `value` (input / change events) into `buf`. See
/// [`lumen_event_type`] for the convention. Thread-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lumen_event_value(
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> LumenStatus {
    catch(|| write_string_out(&lumen_script::event::event_value(), buf, buf_len, out_len))
}

/// The current event's target node (packed handle), or `0` outside a
/// callback. Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_event_target() -> LumenNode {
    catch_val(0u64, lumen_script::event::event_target)
}

/// The current event's `current_target` node (packed handle). Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_event_current_target() -> LumenNode {
    catch_val(0u64, lumen_script::event::event_current_target)
}

/// Cancel the current event's default action (link navigation for `click`,
/// form submission for `submit`). Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_event_prevent_default() -> LumenStatus {
    catch(|| {
        lumen_script::event::event_prevent_default();
        LumenStatus::Ok
    })
}

/// Stop the current event propagating to further nodes. Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_event_stop_propagation() -> LumenStatus {
    catch(|| {
        lumen_script::event::event_stop_propagation();
        LumenStatus::Ok
    })
}

/// Stop the current event immediately: no further handlers run, on this node
/// or any other. Thread-safe.
#[unsafe(no_mangle)]
pub extern "C" fn lumen_event_stop_immediate_propagation() -> LumenStatus {
    catch(|| {
        lumen_script::event::event_stop_immediate_propagation();
        LumenStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_is_packed_correctly() {
        let v = lumen_abi_version();
        assert_eq!(v >> 16, LUMEN_ABI_MAJOR);
        assert_eq!((v >> 8) & 0xff, LUMEN_ABI_MINOR);
        assert_eq!(v & 0xff, LUMEN_ABI_PATCH);
    }

    #[test]
    fn user_data_round_trips() {
        let mut x = 7u32;
        let raw = &mut x as *mut u32 as *mut c_void;
        let ud = UserData::from_raw(raw);
        assert_eq!(ud.as_ptr(), raw);
        let null = UserData::from_raw(ptr::null_mut());
        assert!(null.as_ptr().is_null());
    }

    #[test]
    fn last_error_thread_local_then_global() {
        set_last_error("test message");
        let p = unsafe { lumen_last_error() };
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        assert_eq!(s, "test message");

        // From a fresh thread the TLS is empty - the global fallback kicks in.
        let handle = std::thread::spawn(|| {
            let p = unsafe { lumen_last_error() };
            assert!(
                !p.is_null(),
                "global fallback should surface the prior thread's error"
            );
            let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
            assert_eq!(s, "test message");
        });
        handle.join().unwrap();
    }

    #[test]
    fn classify_runtime_error_picks_specific_variant() {
        assert_eq!(
            classify_runtime_error("css parse failed"),
            LumenStatus::ErrCss
        );
        assert_eq!(
            classify_runtime_error("HTML XML error"),
            LumenStatus::ErrParse
        );
        assert_eq!(
            classify_runtime_error("asset PNG decode failed"),
            LumenStatus::ErrAsset
        );
        assert_eq!(
            classify_runtime_error("window surface create"),
            LumenStatus::ErrWindow
        );
        assert_eq!(
            classify_runtime_error("rhai script error"),
            LumenStatus::ErrScript
        );
        assert_eq!(classify_runtime_error("file io"), LumenStatus::ErrIo);
        assert_eq!(
            classify_runtime_error("completely opaque thing"),
            LumenStatus::ErrRuntime
        );
    }

    #[test]
    fn typed_signal_int64_round_trips() {
        unsafe {
            let name = CString::new("typed_int_test").unwrap();
            assert_eq!(
                lumen_signal_set_int64(ptr::null_mut(), name.as_ptr(), 1234),
                LumenStatus::Ok
            );
            let mut out: i64 = 0;
            assert_eq!(
                lumen_signal_get_int64(ptr::null_mut(), name.as_ptr(), &mut out),
                LumenStatus::Ok
            );
            assert_eq!(out, 1234);
        }
    }

    #[test]
    fn typed_signal_float64_round_trips() {
        unsafe {
            let name = CString::new("typed_float_test").unwrap();
            assert_eq!(
                lumen_signal_set_float64(ptr::null_mut(), name.as_ptr(), 2.5),
                LumenStatus::Ok
            );
            let mut out: f64 = 0.0;
            assert_eq!(
                lumen_signal_get_float64(ptr::null_mut(), name.as_ptr(), &mut out),
                LumenStatus::Ok
            );
            assert_eq!(out, 2.5);
        }
    }

    #[test]
    fn typed_signal_bool_round_trips() {
        unsafe {
            let name = CString::new("typed_bool_test").unwrap();
            assert_eq!(
                lumen_signal_set_bool(ptr::null_mut(), name.as_ptr(), true),
                LumenStatus::Ok
            );
            let mut out = false;
            assert_eq!(
                lumen_signal_get_bool(ptr::null_mut(), name.as_ptr(), &mut out),
                LumenStatus::Ok
            );
            assert!(out);
        }
    }

    #[test]
    fn typed_signal_color_round_trips() {
        unsafe {
            let name = CString::new("typed_color_test").unwrap();
            let rgba = [0xffu8, 0x88, 0x00, 0xff];
            assert_eq!(
                lumen_signal_set_color(ptr::null_mut(), name.as_ptr(), rgba.as_ptr()),
                LumenStatus::Ok
            );
            let mut out = [0u8; 4];
            assert_eq!(
                lumen_signal_get_color(ptr::null_mut(), name.as_ptr(), out.as_mut_ptr()),
                LumenStatus::Ok
            );
            assert_eq!(out, rgba);
        }
    }

    #[test]
    fn typed_signal_get_missing_returns_err_bad_arg() {
        unsafe {
            let name = CString::new("never_set_typed").unwrap();
            let mut out: i64 = -1;
            let s = lumen_signal_get_int64(ptr::null_mut(), name.as_ptr(), &mut out);
            assert_eq!(s, LumenStatus::ErrBadArg);
        }
    }

    #[test]
    fn lumen_status_message_returns_known_strings() {
        unsafe {
            let p = lumen_status_message(LumenStatus::Ok);
            assert!(!p.is_null());
            let s = CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(s, "ok");
            let p2 = lumen_status_message(LumenStatus::ErrBadArg);
            let s2 = CStr::from_ptr(p2).to_str().unwrap();
            assert!(s2.contains("argument"));
        }
    }

    #[test]
    fn typed_setter_routes_through_external_property_bus() {
        // Round 4 closure: the typed FFI setters push a typed
        // `PropertyValue` through `lumen_core::property_store`'s external
        // bus. A synthetic drain (running the system against a fresh
        // `PropertyStore` resource) confirms the typed cell lands without
        // any stringify-on-write round-trip.
        use lumen_core::prelude::Schedule;
        use lumen_core::prelude::World;
        use lumen_core::property_store::{
            PropertyKey, PropertyStore, PropertyValue, drain_external_properties,
            init_external_properties,
        };
        init_external_properties();
        unsafe {
            let name = CString::new("ffi_pre_run_int").unwrap();
            assert_eq!(
                lumen_signal_set_int64(ptr::null_mut(), name.as_ptr(), 7777),
                LumenStatus::Ok
            );
        }
        // Build a tiny world with just the property store and run the
        // drain system once. The synthetic schedule stands in for the
        // ScriptRhaiPlugin's per-tick drain wiring.
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(drain_external_properties);
        schedule.run(&mut world);
        let store = world.resource::<PropertyStore>();
        let cell = store.get(&PropertyKey::Global(Arc::<str>::from("ffi_pre_run_int")));
        assert!(
            matches!(cell, Some(PropertyValue::I64(7777))),
            "typed FFI setter must land a PropertyValue::I64 in PropertyStore; got {cell:?}"
        );
    }

    #[test]
    fn typed_read_falls_back_to_local_cache_when_bus_drained() {
        // Once the bus is drained the FFI typed reads still see the
        // value via the local `TYPED_SIGNALS` cache (round 4 keeps the
        // cache as the always-authoritative read-back surface). This
        // models the post-`lumen_app_run` flow where the runtime drained
        // the bus and the FFI caller hits the cache fallback.
        unsafe {
            let name = CString::new("ffi_cache_fallback_bool").unwrap();
            assert_eq!(
                lumen_signal_set_bool(ptr::null_mut(), name.as_ptr(), true),
                LumenStatus::Ok
            );
        }
        // Forcefully drain the external bus snapshot (no PropertyStore
        // around in this test) by reading once. Subsequent typed reads
        // must still succeed because the cache mirrors every typed set.
        let _ = lumen_core::property_store::external_property_snapshot();
        unsafe {
            let name = CString::new("ffi_cache_fallback_bool").unwrap();
            let mut out = false;
            assert_eq!(
                lumen_signal_get_bool(ptr::null_mut(), name.as_ptr(), &mut out),
                LumenStatus::Ok
            );
            assert!(out, "typed read should fall back to cache after bus drain");
        }
    }

    extern "C" fn noop_watch(_name: *const c_char, _value: *const LumenValue, _ud: *mut c_void) {}

    #[test]
    fn signal_watch_registers_additively() {
        // Only the success path is exercised here: the null-argument paths
        // call `set_last_error`, which writes the process-global error slot
        // and would race the parallel `last_error_thread_local_then_global`
        // test. Null-rejection is covered in the headless integration binary
        // (a separate process with its own global state).
        unsafe {
            let name = CString::new("watch_reg_test").unwrap();
            let cb: LumenWatchFn = noop_watch;
            assert_eq!(
                lumen_signal_watch(name.as_ptr(), Some(cb), ptr::null_mut()),
                LumenStatus::Ok
            );
            // A second registration for the same name is additive (accepted).
            assert_eq!(
                lumen_signal_watch(name.as_ptr(), Some(cb), ptr::null_mut()),
                LumenStatus::Ok
            );
        }
        // Two watchers landed for the name.
        let reg = signal_watchers().lock().unwrap();
        assert_eq!(reg.get("watch_reg_test").map(Vec::len), Some(2));
    }

    #[test]
    fn property_to_lumen_encodes_each_variant() {
        let mut keep: Option<CString> = None;
        assert_eq!(
            property_to_lumen(&PropertyValue::I64(7), &mut keep).kind,
            LumenKind::Int
        );
        assert_eq!(
            property_to_lumen(&PropertyValue::Bool(true), &mut keep).kind,
            LumenKind::Bool
        );
        assert_eq!(
            property_to_lumen(&PropertyValue::F64(1.5), &mut keep).kind,
            LumenKind::Float
        );
        let s = property_to_lumen(&PropertyValue::Str(Arc::<str>::from("hi")), &mut keep);
        assert_eq!(s.kind, LumenKind::String);
        assert!(keep.is_some());
        // Color packs into a LUMEN_INT as 0xRRGGBBAA.
        let c = property_to_lumen(
            &PropertyValue::Color(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            &mut keep,
        );
        assert_eq!(c.kind, LumenKind::Int);
        assert_eq!(unsafe { c.as_.integer } & 0xff, 0xff); // alpha
        assert_eq!((unsafe { c.as_.integer } >> 24) & 0xff, 0xff); // red
    }

    #[test]
    fn status_codes_are_stable() {
        // Numeric stability for embedders. Adding variants is fine;
        // renumbering breaks ABI.
        assert_eq!(LumenStatus::Ok as u32, 0);
        assert_eq!(LumenStatus::ErrBadPath as u32, 1);
        assert_eq!(LumenStatus::ErrBadArg as u32, 2);
        assert_eq!(LumenStatus::ErrRuntime as u32, 3);
        assert_eq!(LumenStatus::ErrInternal as u32, 4);
        assert_eq!(LumenStatus::ErrParse as u32, 5);
        assert_eq!(LumenStatus::ErrCss as u32, 6);
        assert_eq!(LumenStatus::ErrAsset as u32, 7);
        assert_eq!(LumenStatus::ErrWindow as u32, 8);
        assert_eq!(LumenStatus::ErrScript as u32, 9);
        assert_eq!(LumenStatus::ErrIo as u32, 10);
        assert_eq!(LumenStatus::ErrInvalidHandle as u32, 11);
        assert_eq!(LumenStatus::ErrInvalidValue as u32, 12);
        assert_eq!(LumenStatus::ErrPanic as u32, 13);
        assert_eq!(LumenStatus::ErrBufferTooSmall as u32, 14);
    }

    #[test]
    fn string_signal_round_trips_via_get() {
        unsafe {
            let name = CString::new("ffi_get_string_test").unwrap();
            let value = CString::new("hello world").unwrap();
            assert_eq!(
                lumen_signal_set_string(name.as_ptr(), value.as_ptr()),
                LumenStatus::Ok
            );
            // Size query: null buffer reports the required capacity.
            let mut needed: usize = 0;
            assert_eq!(
                lumen_signal_get_string(
                    ptr::null_mut(),
                    name.as_ptr(),
                    ptr::null_mut(),
                    0,
                    &mut needed
                ),
                LumenStatus::ErrBufferTooSmall
            );
            assert_eq!(needed, "hello world".len() + 1);
            // Fill.
            let mut buf = vec![0i8; needed];
            let mut out_len: usize = 0;
            assert_eq!(
                lumen_signal_get_string(
                    ptr::null_mut(),
                    name.as_ptr(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut out_len
                ),
                LumenStatus::Ok
            );
            assert_eq!(out_len, "hello world".len());
            let got = CStr::from_ptr(buf.as_ptr()).to_str().unwrap();
            assert_eq!(got, "hello world");
        }
    }

    #[test]
    fn array_signal_len_and_field_round_trip() {
        unsafe {
            // Build a LUMEN_ARRAY of two LUMEN_MAP rows: [{name:"a"},{name:"b"}].
            let key = CString::new("name").unwrap();
            let va = CString::new("a").unwrap();
            let vb = CString::new("b").unwrap();
            let row0 = [LumenMapEntry {
                key: key.as_ptr(),
                value: LumenValue {
                    kind: LumenKind::String,
                    as_: LumenValueData {
                        string: va.as_ptr(),
                    },
                },
            }];
            let row1 = [LumenMapEntry {
                key: key.as_ptr(),
                value: LumenValue {
                    kind: LumenKind::String,
                    as_: LumenValueData {
                        string: vb.as_ptr(),
                    },
                },
            }];
            let items = [
                LumenValue {
                    kind: LumenKind::Map,
                    as_: LumenValueData {
                        map: LumenMapView {
                            entries: row0.as_ptr(),
                            len: 1,
                        },
                    },
                },
                LumenValue {
                    kind: LumenKind::Map,
                    as_: LumenValueData {
                        map: LumenMapView {
                            entries: row1.as_ptr(),
                            len: 1,
                        },
                    },
                },
            ];
            let arr = LumenValue {
                kind: LumenKind::Array,
                as_: LumenValueData {
                    array: LumenArrayView {
                        items: items.as_ptr(),
                        len: 2,
                    },
                },
            };
            let name = CString::new("ffi_array_test").unwrap();
            assert_eq!(lumen_signal_set_array(name.as_ptr(), &arr), LumenStatus::Ok);

            let mut len: usize = 0;
            assert_eq!(
                lumen_signal_array_len(ptr::null_mut(), name.as_ptr(), &mut len),
                LumenStatus::Ok
            );
            assert_eq!(len, 2);

            let mut buf = [0i8; 8];
            let mut out_len: usize = 0;
            assert_eq!(
                lumen_signal_array_get_field(
                    ptr::null_mut(),
                    name.as_ptr(),
                    1,
                    key.as_ptr(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut out_len
                ),
                LumenStatus::Ok
            );
            assert_eq!(CStr::from_ptr(buf.as_ptr()).to_str().unwrap(), "b");
        }
    }
}
