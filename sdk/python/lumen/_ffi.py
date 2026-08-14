"""ctypes bindings for the Lumen C ABI.

Stdlib-only - no ``cffi``, no build step. This module is the thin,
unopinionated layer: it finds ``liblumen``, declares every exported
function's ``argtypes``/``restype``, mirrors the C structs byte-for-byte,
and converts between :class:`LumenValue` and plain Python values.

Everything Pythonic (the ``App`` class, decorators, typed signal
get/set) lives in :mod:`lumen.api`. Import this module directly only if
you need the raw C surface.

Source of truth for the layout below: ``src/lib.rs`` and the
generated header pair ``include/{lumen.h,lumen_simple.h}``.
Read those before changing a ``_fields_`` list - a mismatched struct
layout is silent memory corruption, not a Python exception.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import shutil
import sys
from pathlib import Path

from .errors import LumenAbiVersionError, LumenLibraryNotFoundError

__all__ = [
    "load_library",
    "LumenKind",
    "LumenStatus",
    "LumenValue",
    "LumenValueData",
    "LumenArrayView",
    "LumenMapView",
    "LumenMapEntry",
    "make_callback",
    "make_click_callback",
    "make_watch_callback",
    "make_event_callback",
    "LumenNodeList",
    "LumenRect",
    "LumenScroll",
    "LumenKV",
    "LumenKVList",
    "LumenStrList",
    "LumenPointerState",
    "LumenFrameInfo",
    "LumenEvent",
    "to_lumen_value",
    "from_lumen_value",
    "ABI_MAJOR",
    "ABI_MINOR",
    "ABI_PATCH",
]

# ============================================================
# ABI version this SDK was written against.
#
# Mirrors LUMEN_ABI_{MAJOR,MINOR,PATCH} in src/lib.rs at the
# time this SDK was written. `load_library()` compares this against
# the *loaded* library's `lumen_abi_version()` at import time.
# ============================================================

ABI_MAJOR = 0
ABI_MINOR = 13
ABI_PATCH = 0


def _packed(major: int, minor: int, patch: int) -> int:
    return (major << 16) | (minor << 8) | patch


EXPECTED_ABI_VERSION = _packed(ABI_MAJOR, ABI_MINOR, ABI_PATCH)


# ============================================================
# LumenKind / LumenStatus - mirror the Rust #[repr(u32)] enums.
# ============================================================


class LumenKind:
    """Discriminant for :class:`LumenValue`. Not a ``ctypes`` type
    itself - ``LumenValue.kind`` is a plain ``c_uint32``; use these as
    the comparison constants."""

    NIL = 0
    BOOL = 1
    INT = 2
    FLOAT = 3
    STRING = 4
    ARRAY = 5
    MAP = 6


class LumenStatus:
    """Numeric ``LumenStatus`` codes. See :mod:`lumen.errors` for the
    exception each one maps to."""

    OK = 0
    ERR_BAD_PATH = 1
    ERR_BAD_ARG = 2
    ERR_RUNTIME = 3
    ERR_INTERNAL = 4
    ERR_PARSE = 5
    ERR_CSS = 6
    ERR_ASSET = 7
    ERR_WINDOW = 8
    ERR_SCRIPT = 9
    ERR_IO = 10
    ERR_INVALID_HANDLE = 11
    ERR_INVALID_VALUE = 12
    ERR_PANIC = 13
    ERR_BUFFER_TOO_SMALL = 14


# ============================================================
# LumenValue tree - must match the #[repr(C)] layout in lib.rs exactly:
#
#   #[repr(u32)] enum LumenKind { Nil=0, Bool, Int, Float, String, Array, Map }
#
#   #[repr(C)] struct LumenArrayView { items: *const LumenValue, len: usize }
#   #[repr(C)] struct LumenMapView   { entries: *const LumenMapEntry, len: usize }
#
#   #[repr(C)] union LumenValueData {
#       boolean: c_int, integer: i64, float_: f64, string: *const c_char,
#       array: LumenArrayView, map: LumenMapView,
#   }
#
#   #[repr(C)] struct LumenValue { kind: LumenKind, as_: LumenValueData }
#   #[repr(C)] struct LumenMapEntry { key: *const c_char, value: LumenValue }
#
# ctypes lays out Structure/Union fields using the platform C ABI rules
# (same as the Rust compiler's repr(C)), so declaring the fields in the
# same order reproduces the same padding automatically - no manual
# alignment arithmetic needed.
# ============================================================


class LumenValue(ctypes.Structure):
    pass


class LumenArrayView(ctypes.Structure):
    _fields_ = [
        ("items", ctypes.POINTER(LumenValue)),
        ("len", ctypes.c_size_t),
    ]


class LumenMapEntry(ctypes.Structure):
    pass


class LumenMapView(ctypes.Structure):
    _fields_ = [
        ("entries", ctypes.POINTER(LumenMapEntry)),
        ("len", ctypes.c_size_t),
    ]


class LumenValueData(ctypes.Union):
    _fields_ = [
        ("boolean", ctypes.c_int),
        ("integer", ctypes.c_int64),
        ("float_", ctypes.c_double),
        ("string", ctypes.c_char_p),
        ("array", LumenArrayView),
        ("map", LumenMapView),
    ]


LumenValue._fields_ = [
    ("kind", ctypes.c_uint32),
    ("as_", LumenValueData),
]

LumenMapEntry._fields_ = [
    ("key", ctypes.c_char_p),
    ("value", LumenValue),
]

# ============================================================
# Dynamic DOM structs (ABI 0.8 read side, 0.11 introspection, 0.12
# inner_markup). Mirror the #[repr(C)] layouts in lib.rs / the generated
# lumen_simple.h. `LumenNode` and `LumenEventToken` are bare uint64
# typedefs (use ctypes.c_uint64 directly); the rest are Structures.
# ============================================================


class LumenNodeList(ctypes.Structure):
    """Owned list of node handles from a query / children call. Free with
    ``lumen_nodelist_free``."""

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_uint64)),
        ("len", ctypes.c_size_t),
    ]


class LumenRect(ctypes.Structure):
    """Post-layout box (``rect`` / ``content_rect``). Local ``x`` / ``y``
    relative to the parent; ``client_*`` in window coordinates."""

    _fields_ = [
        ("x", ctypes.c_double),
        ("y", ctypes.c_double),
        ("width", ctypes.c_double),
        ("height", ctypes.c_double),
        ("client_x", ctypes.c_double),
        ("client_y", ctypes.c_double),
    ]


class LumenScroll(ctypes.Structure):
    """Scroll offsets + travel limits (``scroll``)."""

    _fields_ = [
        ("x", ctypes.c_double),
        ("y", ctypes.c_double),
        ("max_x", ctypes.c_double),
        ("max_y", ctypes.c_double),
    ]


class LumenKV(ctypes.Structure):
    """One owned ``(key, value)`` pair in a :class:`LumenKVList`."""

    _fields_ = [
        ("key", ctypes.c_char_p),
        ("value", ctypes.c_char_p),
    ]


class LumenKVList(ctypes.Structure):
    """Owned key-value buffer from a string-map introspection read. Free
    with ``lumen_kvlist_free``."""

    _fields_ = [
        ("ptr", ctypes.POINTER(LumenKV)),
        ("len", ctypes.c_size_t),
    ]


class LumenStrList(ctypes.Structure):
    """Owned string buffer from ``classes`` / ``components``. Free with
    ``lumen_strlist_free``."""

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_char_p)),
        ("len", ctypes.c_size_t),
    ]


class LumenPointerState(ctypes.Structure):
    """Pointer state snapshot (``pointer_state``)."""

    _fields_ = [
        ("x", ctypes.c_double),
        ("y", ctypes.c_double),
        ("inside", ctypes.c_int),
        ("buttons", ctypes.c_uint32),
        ("shift", ctypes.c_int),
        ("ctrl", ctypes.c_int),
        ("alt", ctypes.c_int),
        ("super_", ctypes.c_int),
    ]


class LumenFrameInfo(ctypes.Structure):
    """Per-frame counters (``frame_info``)."""

    _fields_ = [
        ("frame", ctypes.c_uint64),
        ("dt_ms", ctypes.c_double),
        ("dirty_count", ctypes.c_uint64),
    ]


class LumenEvent(ctypes.Structure):
    """Scalar snapshot of a dynamic DOM event (ABI 0.10). String fields
    (type / key / value) are read separately via the current-event
    getters. Field order mirrors ``LumenEvent`` in lumen.h."""

    _fields_ = [
        ("target", ctypes.c_uint64),
        ("current_target", ctypes.c_uint64),
        ("local_x", ctypes.c_double),
        ("local_y", ctypes.c_double),
        ("client_x", ctypes.c_double),
        ("client_y", ctypes.c_double),
        ("delta_x", ctypes.c_double),
        ("delta_y", ctypes.c_double),
        ("button", ctypes.c_int64),
        ("shift", ctypes.c_uint8),
        ("ctrl", ctypes.c_uint8),
        ("alt", ctypes.c_uint8),
        ("super_", ctypes.c_uint8),
    ]


# Dynamic DOM event callback (ABI 0.10):
#   type LumenEventFn = extern "C" fn(event: *const LumenEvent,
#                                     user_data: *mut c_void);
# `event` is borrowed for the call. `dom.Node.on` wraps a Python handler
# through make_event_callback below.
_LumenEventFnRaw = ctypes.CFUNCTYPE(None, ctypes.POINTER(LumenEvent), ctypes.c_void_p)

# Signature of an exposed callback. As of ABI 0.3 the SDK targets the
# out-parameter variant, ``LumenFnV2``:
#
#   type LumenFnV2 = unsafe extern "C" fn(out: *mut LumenValue, argc: c_int,
#       argv: *const LumenValue, user_data: *mut c_void);   // no return value
#
# The callback writes its result through ``out`` instead of returning a
# ``LumenValue`` by value. This is a *first-class, documented* ABI
# signature, not a workaround: it exists precisely so ctypes / libffi
# bindings don't have to hand-encode a platform's aggregate-return
# (``sret``) convention.
#
# This matters because ctypes cannot build a callback whose ``restype``
# is a Structure/Union at all (``TypeError: invalid result type for
# callback function``), and ``LumenValue`` is 24 bytes -- past the SysV
# x86-64 16-byte register-pair threshold -- so the by-value ``LumenFn``
# (v1) return has to travel through a hidden ABI-implicit sret pointer
# that we'd otherwise have to reconstruct per target. ``LumenFnV2``'s
# explicit leading ``out`` pointer *is* that pointer, made part of the
# contract, so ``restype=None`` here is exactly right on every platform
# rather than only on x86_64 Linux by luck. We register through
# ``lumen_app_expose_v2`` accordingly. The v1 ``lumen_app_expose`` +
# by-value ``LumenFn`` path is intentionally not used by this SDK.
_LumenFnV2Raw = ctypes.CFUNCTYPE(
    None,
    ctypes.POINTER(LumenValue),  # out: caller-allocated result slot (ABI-explicit)
    ctypes.c_int,
    ctypes.POINTER(LumenValue),
    ctypes.c_void_p,
)

# Backwards-compatible alias: older code / tests referred to the raw
# callback factory as ``_LumenFnRaw``. Same shape (the v1 sret hack and
# the v2 out-param have identical ctypes signatures), now the v2 ABI.
_LumenFnRaw = _LumenFnV2Raw

# Id-scoped native click callback (ABI 0.3):
#   type LumenClickFn = unsafe extern "C" fn(id: *const c_char,
#                                            user_data: *mut c_void);
_LumenClickFnRaw = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_void_p)

# Signal-change subscription callback (ABI 0.4):
#   type LumenWatchFn = unsafe extern "C" fn(name: *const c_char,
#       value: *const LumenValue, user_data: *mut c_void);
# `value` is borrowed for the call; we deep-copy it into a plain Python
# value before invoking the user handler.
_LumenWatchFnRaw = ctypes.CFUNCTYPE(
    None, ctypes.c_char_p, ctypes.POINTER(LumenValue), ctypes.c_void_p
)


def make_callback(func):
    """Wrap ``func(argc: int, argv: POINTER(LumenValue), user_data: int) ->
    LumenValue`` into the actual ctypes callback object passed to
    ``lumen_app_expose_v2`` (the out-parameter ABI variant). Keep the
    return value referenced by the caller (e.g. in ``App._callbacks``)
    for as long as the callback might still be invoked -- see the module
    docstring.
    """

    def raw(out, argc, argv, user_data):
        result = func(argc, argv, user_data)
        out[0] = result

    return _LumenFnV2Raw(raw)


def make_click_callback(func):
    """Wrap ``func(id: str)`` into a ctypes ``LumenClickFn`` callback for
    ``lumen_app_on_click``. Keep the return value referenced for as long
    as the app can still deliver clicks.
    """

    def raw(id_ptr, _user_data):
        id_str = id_ptr.decode("utf-8", errors="replace") if id_ptr else ""
        func(id_str)

    return _LumenClickFnRaw(raw)


def make_watch_callback(func):
    """Wrap ``func(name: str, value: object)`` into a ctypes ``LumenWatchFn``
    callback for ``lumen_signal_watch``. ``value`` is the new committed
    signal value, deep-copied out of the borrowed ``LumenValue`` before the
    handler runs (so nothing aliases C memory). Keep the return value
    referenced for as long as the app can still deliver changes.

    An exception raised inside ``func`` is printed via
    ``traceback.print_exc()`` and swallowed - a Python exception must never
    unwind across the Rust FFI frames (that is UB).
    """

    def raw(name_ptr, value_ptr, _user_data):
        try:
            name = name_ptr.decode("utf-8", errors="replace") if name_ptr else ""
            value = from_lumen_value(value_ptr[0]) if value_ptr else None
            func(name, value)
        except Exception:
            import traceback

            traceback.print_exc()

    return _LumenWatchFnRaw(raw)


def make_event_callback(func):
    """Wrap ``func(event_ptr: POINTER(LumenEvent))`` into a ctypes
    ``LumenEventFn`` callback for ``lumen_on``. ``dom.Node.on`` passes a
    handler that builds a :class:`~lumen.dom.Event` from the pointer. Keep
    the return value referenced for as long as the binding is live.

    An exception raised inside ``func`` is printed via
    ``traceback.print_exc()`` and swallowed -- a Python exception must never
    unwind across the Rust FFI frames (that is UB).
    """

    def raw(event_ptr, _user_data):
        try:
            func(event_ptr)
        except Exception:
            import traceback

            traceback.print_exc()

    return _LumenEventFnRaw(raw)


# ============================================================
# Value marshaling: Python <-> LumenValue
# ============================================================


def to_lumen_value(value: object, keepalive: list) -> LumenValue:
    """Convert a plain Python value into an owned :class:`LumenValue`.

    ``keepalive`` must be a list that outlives the returned
    ``LumenValue`` (e.g. the list backing a callback's per-call scratch
    space) - it collects the intermediate ``bytes``/``ctypes`` buffers
    that back any ``STRING``/``ARRAY``/``MAP`` payload so they aren't
    garbage-collected while C still holds the pointer. Scalar payloads
    (nil/bool/int/float) need no such buffer.

    Supported Python types: ``None``, ``bool``, ``int``, ``float``,
    ``str`` (UTF-8 encoded), ``list``/``tuple`` (-> ``LUMEN_ARRAY``,
    recursively), ``dict`` (-> ``LUMEN_MAP``, keys stringified to
    UTF-8, recursively). Anything else raises ``TypeError`` - callers
    should convert first rather than rely on ``str(value)``.
    """

    if value is None:
        data = LumenValueData(integer=0)
        return LumenValue(kind=LumenKind.NIL, as_=data)
    # bool is a subclass of int in Python -- check it first.
    if isinstance(value, bool):
        data = LumenValueData(boolean=1 if value else 0)
        return LumenValue(kind=LumenKind.BOOL, as_=data)
    if isinstance(value, int):
        data = LumenValueData(integer=value)
        return LumenValue(kind=LumenKind.INT, as_=data)
    if isinstance(value, float):
        data = LumenValueData(float_=value)
        return LumenValue(kind=LumenKind.FLOAT, as_=data)
    if isinstance(value, str):
        buf = ctypes.create_string_buffer(value.encode("utf-8"))
        keepalive.append(buf)
        data = LumenValueData(string=ctypes.cast(buf, ctypes.c_char_p))
        return LumenValue(kind=LumenKind.STRING, as_=data)
    if isinstance(value, (list, tuple)):
        items = [to_lumen_value(v, keepalive) for v in value]
        arr = (LumenValue * len(items))(*items)
        keepalive.append(arr)
        view = LumenArrayView(
            items=ctypes.cast(arr, ctypes.POINTER(LumenValue)), len=len(items)
        )
        data = LumenValueData(array=view)
        return LumenValue(kind=LumenKind.ARRAY, as_=data)
    if isinstance(value, dict):
        entries = []
        for k, v in value.items():
            key_buf = ctypes.create_string_buffer(str(k).encode("utf-8"))
            keepalive.append(key_buf)
            entries.append(
                LumenMapEntry(
                    key=ctypes.cast(key_buf, ctypes.c_char_p),
                    value=to_lumen_value(v, keepalive),
                )
            )
        entry_arr = (LumenMapEntry * len(entries))(*entries)
        keepalive.append(entry_arr)
        view = LumenMapView(
            entries=ctypes.cast(entry_arr, ctypes.POINTER(LumenMapEntry)),
            len=len(entries),
        )
        data = LumenValueData(map=view)
        return LumenValue(kind=LumenKind.MAP, as_=data)
    raise TypeError(
        f"cannot convert {type(value).__name__!r} to a LumenValue; "
        "use None/bool/int/float/str/list/dict"
    )


def from_lumen_value(v: "LumenValue") -> object:
    """Copy a (possibly borrowed) :class:`LumenValue` into a plain,
    independent Python value. Mirrors ``lumen.hpp``'s ``Value::adopt``.
    Safe to call on a value borrowed for only the duration of one
    call - nothing in the result aliases C memory afterwards.
    """

    kind = v.kind
    if kind == LumenKind.NIL:
        return None
    if kind == LumenKind.BOOL:
        return v.as_.boolean != 0
    if kind == LumenKind.INT:
        return v.as_.integer
    if kind == LumenKind.FLOAT:
        return v.as_.float_
    if kind == LumenKind.STRING:
        p = v.as_.string
        return p.decode("utf-8", errors="replace") if p else ""
    if kind == LumenKind.ARRAY:
        view = v.as_.array
        if not view.items or view.len == 0:
            return []
        return [from_lumen_value(view.items[i]) for i in range(view.len)]
    if kind == LumenKind.MAP:
        view = v.as_.map
        if not view.entries or view.len == 0:
            return {}
        out = {}
        for i in range(view.len):
            entry = view.entries[i]
            key = entry.key.decode("utf-8", errors="replace") if entry.key else ""
            out[key] = from_lumen_value(entry.value)
        return out
    raise ValueError(f"unknown LumenKind discriminant: {kind}")


# ============================================================
# Library loading
# ============================================================

_PLATFORM_LIBNAMES: dict[str, tuple[str, ...]] = {
    "linux": ("liblumen.so",),
    "darwin": ("liblumen.dylib",),
    "win32": ("lumen.dll",),
}


def _libnames_for_platform() -> tuple[str, ...]:
    for prefix, names in _PLATFORM_LIBNAMES.items():
        if sys.platform.startswith(prefix):
            return names
    # Best-effort fallback for platforms we haven't special-cased.
    return ("liblumen.so", "liblumen.dylib", "lumen.dll")


def _candidate_paths() -> list[Path]:
    """Build the ordered list of paths to try, per the load order
    documented in the README:

    1. ``LUMEN_LIBRARY_PATH`` env var - either a direct path to the
       library file, or a directory containing it.
    2. ``LUMEN_LIB_DIR`` - the same override ``lumenc`` itself honours,
       so one setting points both at the same runtime.
    3. ``target/{debug,release}`` relative to the current working
       directory (the common case: running from the repo root).
    4. ``target/{debug,release}`` relative to the repo root, found by
       walking up from this file looking for the workspace
       ``Cargo.toml`` (covers running the example from elsewhere).
    5. An installed toolchain: next to the ``lumenc`` on ``PATH``, then
       ``$LUMEN_PREFIX/bin`` (``~/.lumen/bin`` by default). The
       installer puts the shared library beside ``lumenc`` rather than
       in a sibling ``lib/``, and that directory is on ``PATH``, not on
       the loader's search path, so the system loader in step 6 does not
       find it on its own.
    6. System library search paths (handled separately by
       ``ctypes.util.find_library`` as a last resort - see
       ``load_library``).
    """

    names = _libnames_for_platform()
    candidates: list[Path] = []

    for var in ("LUMEN_LIBRARY_PATH", "LUMEN_LIB_DIR"):
        env_path = os.environ.get(var)
        if not env_path:
            continue
        p = Path(env_path)
        if p.is_file():
            candidates.append(p)
        else:
            # Treat as a directory containing the library.
            for name in names:
                candidates.append(p / name)

    cwd = Path.cwd()
    for profile in ("debug", "release"):
        for name in names:
            candidates.append(cwd / "target" / profile / name)

    # Walk up from this file looking for the workspace root (marked by
    # a top-level Cargo.toml with a [workspace] table listing "crates/*").
    # Bounded depth so a misplaced copy of the SDK can't walk all the way
    # to `/`.
    here = Path(__file__).resolve()
    for ancestor in list(here.parents)[:8]:
        cargo_toml = ancestor / "Cargo.toml"
        if cargo_toml.is_file():
            try:
                text = cargo_toml.read_text(encoding="utf-8", errors="ignore")
            except OSError:
                continue
            if "[workspace]" in text and "crates/*" in text:
                for profile in ("debug", "release"):
                    for name in names:
                        candidates.append(ancestor / "target" / profile / name)
                break

    # An installed toolchain. `lumenc` and the shared library live in the same
    # bin/ directory, so finding one finds the other.
    lumenc = shutil.which("lumenc")
    if lumenc:
        for name in names:
            candidates.append(Path(lumenc).resolve().parent / name)
    prefix = Path(os.environ.get("LUMEN_PREFIX", Path.home() / ".lumen"))
    for name in names:
        candidates.append(prefix / "bin" / name)

    return candidates


def load_library(path: str | os.PathLike | None = None) -> ctypes.CDLL:
    """Locate and load ``liblumen``, then run every exported
    function's prototype declaration and an ABI version check.

    Args:
        path: Skip the search entirely and load this exact file.

    Raises:
        LumenLibraryNotFoundError: no candidate path existed and the
            system loader couldn't resolve one either.
        LumenAbiVersionError: the loaded library's ``lumen_abi_version()``
            has a different major version, or an older minor version,
            than this SDK was written against.
    """

    lib: ctypes.CDLL | None = None
    tried: list[str] = []

    if path is not None:
        p = Path(path)
        tried.append(str(p))
        lib = ctypes.CDLL(str(p))
    else:
        for candidate in _candidate_paths():
            tried.append(str(candidate))
            if candidate.is_file():
                lib = ctypes.CDLL(str(candidate))
                break

        if lib is None:
            # Last resort: let the OS loader search its own paths
            # (LD_LIBRARY_PATH, /usr/lib, DYLD_LIBRARY_PATH, PATH, ...).
            for soname in ("lumen",):
                found = ctypes.util.find_library(soname)
                if found:
                    tried.append(found)
                    lib = ctypes.CDLL(found)
                    break

    if lib is None:
        raise LumenLibraryNotFoundError(
            "could not locate liblumen. Build it with "
            "`cargo build -p lumen` from the Lumen workspace root, "
            "then either set LUMEN_LIBRARY_PATH=target/debug (or "
            ".../release), or run from a directory below target/. "
            f"Tried: {tried}"
        )

    _declare_prototypes(lib)

    version = lib.lumen_abi_version()
    loaded_major = version >> 16
    loaded_minor = (version >> 8) & 0xFF
    loaded_patch = version & 0xFF
    if loaded_major != ABI_MAJOR or loaded_minor < ABI_MINOR:
        raise LumenAbiVersionError(
            "lumen ABI mismatch: this SDK was written against "
            f"{ABI_MAJOR}.{ABI_MINOR}.{ABI_PATCH} but the loaded library "
            f"reports {loaded_major}.{loaded_minor}.{loaded_patch}. "
            "A different major version, or an older minor version, is a "
            "hard incompatibility -- rebuild lumen or pin the SDK "
            "version that matches it."
        )

    return lib


def _declare_prototypes(lib: ctypes.CDLL) -> None:
    """Set ``argtypes``/``restype`` for every ``lumen_*`` export.

    Cross-referenced 1:1 against ``include/lumen.h`` and
    ``lumen_simple.h``. Keep this list in the same order as the header
    so a future ABI diff is easy to spot-check.
    """

    c = ctypes
    P = ctypes.POINTER

    lib.lumen_abi_version.argtypes = []
    lib.lumen_abi_version.restype = c.c_uint32

    lib.lumen_app_new.argtypes = [c.c_char_p]
    lib.lumen_app_new.restype = c.c_void_p

    # ABI 0.7 link-not-embed seam: build an app from prebuilt LMNA bytes with
    # no parser. data is a bytes buffer, len its length, base_dir a dir (or
    # NULL) for relative asset resolution.
    lib.lumen_app_new_from_lmna.argtypes = [c.c_char_p, c.c_size_t, c.c_char_p]
    lib.lumen_app_new_from_lmna.restype = c.c_void_p

    # 4th param is conceptually `Option<LumenFn>` -- declared as a bare
    # void* here because the actual object we pass is a `_LumenFnRaw`
    # closure (see make_callback's docstring for why), cast to c_void_p
    # at the call site.
    lib.lumen_app_expose.argtypes = [
        c.c_void_p,
        c.c_char_p,
        c.c_uint32,
        c.c_void_p,
        c.c_void_p,
    ]
    lib.lumen_app_expose.restype = c.c_uint32

    # ABI 0.3 out-parameter callback registration (LumenFnV2). Same arg
    # shape as v1; the difference is entirely in the callback's own
    # signature (out-pointer instead of by-value return).
    lib.lumen_app_expose_v2.argtypes = [
        c.c_void_p,
        c.c_char_p,
        c.c_uint32,
        c.c_void_p,
        c.c_void_p,
    ]
    lib.lumen_app_expose_v2.restype = c.c_uint32

    # ABI 0.3 id-scoped native click hook + headless run driver.
    lib.lumen_app_on_click.argtypes = [c.c_void_p, c.c_char_p, c.c_void_p, c.c_void_p]
    lib.lumen_app_on_click.restype = c.c_uint32

    lib.lumen_app_run_headless.argtypes = [c.c_void_p, c.c_uint32]
    lib.lumen_app_run_headless.restype = c.c_uint32

    # ABI 0.4 signal-change subscription. The callback is a `_LumenWatchFnRaw`
    # closure cast to c_void_p at the call site (same pattern as the expose /
    # click callbacks). Registration is global - no app handle argument.
    lib.lumen_signal_watch.argtypes = [c.c_char_p, c.c_void_p, c.c_void_p]
    lib.lumen_signal_watch.restype = c.c_uint32

    lib.lumen_app_set_title.argtypes = [c.c_void_p, c.c_char_p]
    lib.lumen_app_set_title.restype = c.c_uint32

    lib.lumen_app_set_size.argtypes = [c.c_void_p, c.c_uint32, c.c_uint32]
    lib.lumen_app_set_size.restype = c.c_uint32

    lib.lumen_app_free.argtypes = [c.c_void_p]
    lib.lumen_app_free.restype = None

    lib.lumen_app_run.argtypes = [c.c_void_p]
    lib.lumen_app_run.restype = c.c_uint32

    lib.lumen_last_error.argtypes = []
    lib.lumen_last_error.restype = c.c_char_p

    lib.lumen_last_error_global.argtypes = []
    lib.lumen_last_error_global.restype = c.c_char_p

    lib.lumen_signal_clear.argtypes = [c.c_char_p]
    lib.lumen_signal_clear.restype = c.c_uint32

    lib.lumen_signal_set_array.argtypes = [c.c_char_p, P(LumenValue)]
    lib.lumen_signal_set_array.restype = c.c_uint32

    lib.lumen_signal_set_str.argtypes = [c.c_char_p, c.c_char_p]
    lib.lumen_signal_set_str.restype = c.c_uint32

    # `buf` is a char* buffer and `out_len` a size_t*;
    # LUMEN_ERR_BUFFER_TOO_SMALL (14) sets *out_len to the required
    # capacity (byte length + 1 for the NUL).
    lib.lumen_signal_get_str.argtypes = [
        c.c_char_p,
        c.c_char_p,
        c.c_size_t,
        P(c.c_size_t),
    ]
    lib.lumen_signal_get_str.restype = c.c_uint32

    lib.lumen_signal_set_int64.argtypes = [c.c_char_p, c.c_int64]
    lib.lumen_signal_set_int64.restype = c.c_uint32

    lib.lumen_signal_get_int64.argtypes = [c.c_char_p, P(c.c_int64)]
    lib.lumen_signal_get_int64.restype = c.c_uint32

    lib.lumen_signal_set_float64.argtypes = [c.c_char_p, c.c_double]
    lib.lumen_signal_set_float64.restype = c.c_uint32

    lib.lumen_signal_get_float64.argtypes = [c.c_char_p, P(c.c_double)]
    lib.lumen_signal_get_float64.restype = c.c_uint32

    # bool is passed as a single byte on the Rust side (`bool` is
    # 1-byte, repr matches C `_Bool`/`bool` from <stdbool.h>).
    lib.lumen_signal_set_bool.argtypes = [c.c_char_p, c.c_bool]
    lib.lumen_signal_set_bool.restype = c.c_uint32

    lib.lumen_signal_get_bool.argtypes = [c.c_char_p, P(c.c_bool)]
    lib.lumen_signal_get_bool.restype = c.c_uint32

    lib.lumen_signal_set_color.argtypes = [c.c_char_p, P(c.c_uint8)]
    lib.lumen_signal_set_color.restype = c.c_uint32

    lib.lumen_signal_get_color.argtypes = [c.c_char_p, P(c.c_uint8)]
    lib.lumen_signal_get_color.restype = c.c_uint32

    # ABI 0.3 array read-back getters, same buffer convention as
    # lumen_signal_get_str.
    lib.lumen_signal_array_len.argtypes = [c.c_char_p, P(c.c_size_t)]
    lib.lumen_signal_array_len.restype = c.c_uint32

    lib.lumen_signal_array_get_field.argtypes = [
        c.c_char_p,
        c.c_size_t,
        c.c_char_p,
        c.c_char_p,
        c.c_size_t,
        P(c.c_size_t),
    ]
    lib.lumen_signal_array_get_field.restype = c.c_uint32

    lib.lumen_status_message.argtypes = [c.c_uint32]
    lib.lumen_status_message.restype = c.c_char_p

    # ------------------------------------------------------------------
    # Dynamic DOM (ABI 0.8 read side, 0.11 introspection, 0.12
    # inner_markup). `LumenNode` / `LumenEventToken` are uint64 handles;
    # out-params take a POINTER. Cross-referenced 1:1 with lumen_simple.h.
    # ------------------------------------------------------------------
    Node = c.c_uint64
    PNode = P(c.c_uint64)

    # -- query / traversal --
    lib.lumen_query.argtypes = [c.c_char_p, P(LumenNodeList)]
    lib.lumen_query.restype = c.c_uint32
    lib.lumen_query_len.argtypes = [c.c_char_p, P(c.c_size_t)]
    lib.lumen_query_len.restype = c.c_uint32
    lib.lumen_query_single.argtypes = [c.c_char_p, PNode]
    lib.lumen_query_single.restype = c.c_uint32
    lib.lumen_get_by_id.argtypes = [c.c_char_p, PNode]
    lib.lumen_get_by_id.restype = c.c_uint32
    lib.lumen_document.argtypes = [PNode]
    lib.lumen_document.restype = c.c_uint32
    lib.lumen_node_parent.argtypes = [Node, PNode]
    lib.lumen_node_parent.restype = c.c_uint32
    lib.lumen_node_first_child.argtypes = [Node, PNode]
    lib.lumen_node_first_child.restype = c.c_uint32
    lib.lumen_node_last_child.argtypes = [Node, PNode]
    lib.lumen_node_last_child.restype = c.c_uint32
    lib.lumen_node_next.argtypes = [Node, PNode]
    lib.lumen_node_next.restype = c.c_uint32
    lib.lumen_node_prev.argtypes = [Node, PNode]
    lib.lumen_node_prev.restype = c.c_uint32
    lib.lumen_node_children.argtypes = [Node, P(LumenNodeList)]
    lib.lumen_node_children.restype = c.c_uint32
    lib.lumen_node_closest.argtypes = [Node, c.c_char_p, PNode]
    lib.lumen_node_closest.restype = c.c_uint32
    lib.lumen_node_valid.argtypes = [Node, P(c.c_int)]
    lib.lumen_node_valid.restype = c.c_uint32
    lib.lumen_nodelist_get.argtypes = [LumenNodeList, c.c_size_t, PNode]
    lib.lumen_nodelist_get.restype = c.c_uint32
    lib.lumen_nodelist_free.argtypes = [LumenNodeList]
    lib.lumen_nodelist_free.restype = None

    # -- introspection reads (maps / lists / geometry) --
    lib.lumen_node_rect.argtypes = [Node, P(LumenRect)]
    lib.lumen_node_rect.restype = c.c_uint32
    lib.lumen_node_content_rect.argtypes = [Node, P(LumenRect)]
    lib.lumen_node_content_rect.restype = c.c_uint32
    lib.lumen_node_scroll.argtypes = [Node, P(LumenScroll)]
    lib.lumen_node_scroll.restype = c.c_uint32
    lib.lumen_node_is_visible.argtypes = [Node, P(c.c_int)]
    lib.lumen_node_is_visible.restype = c.c_uint32
    lib.lumen_node_z_index.argtypes = [Node, P(c.c_int)]
    lib.lumen_node_z_index.restype = c.c_uint32
    lib.lumen_node_entity_id.argtypes = [Node, P(c.c_uint32), P(c.c_uint32)]
    lib.lumen_node_entity_id.restype = c.c_uint32
    lib.lumen_node_computed_style.argtypes = [Node, P(LumenKVList)]
    lib.lumen_node_computed_style.restype = c.c_uint32
    lib.lumen_node_attrs.argtypes = [Node, P(LumenKVList)]
    lib.lumen_node_attrs.restype = c.c_uint32
    lib.lumen_node_inline_style.argtypes = [Node, P(LumenKVList)]
    lib.lumen_node_inline_style.restype = c.c_uint32
    lib.lumen_node_component.argtypes = [Node, c.c_char_p, P(LumenKVList)]
    lib.lumen_node_component.restype = c.c_uint32
    lib.lumen_node_classes.argtypes = [Node, P(LumenStrList)]
    lib.lumen_node_classes.restype = c.c_uint32
    lib.lumen_node_components.argtypes = [Node, P(LumenStrList)]
    lib.lumen_node_components.restype = c.c_uint32
    lib.lumen_node_outer_markup.argtypes = [Node, P(c.c_char_p)]
    lib.lumen_node_outer_markup.restype = c.c_uint32
    lib.lumen_node_inner_markup.argtypes = [Node, P(c.c_char_p)]
    lib.lumen_node_inner_markup.restype = c.c_uint32
    lib.lumen_dump_tree.argtypes = [P(c.c_char_p)]
    lib.lumen_dump_tree.restype = c.c_uint32
    lib.lumen_pointer_state.argtypes = [P(LumenPointerState)]
    lib.lumen_pointer_state.restype = c.c_uint32
    lib.lumen_frame_info.argtypes = [P(LumenFrameInfo)]
    lib.lumen_frame_info.restype = c.c_uint32
    lib.lumen_signals_all.argtypes = [P(LumenKVList)]
    lib.lumen_signals_all.restype = c.c_uint32

    # -- releasers for owned introspection buffers --
    lib.lumen_kvlist_free.argtypes = [LumenKVList]
    lib.lumen_kvlist_free.restype = None
    lib.lumen_strlist_free.argtypes = [LumenStrList]
    lib.lumen_strlist_free.restype = None
    lib.lumen_string_free.argtypes = [c.c_char_p]
    lib.lumen_string_free.restype = None

    # -- writes (queue on the external DOM bus) --
    lib.lumen_node_set_attr.argtypes = [Node, c.c_char_p, c.c_char_p]
    lib.lumen_node_set_attr.restype = c.c_uint32
    lib.lumen_node_remove_attr.argtypes = [Node, c.c_char_p]
    lib.lumen_node_remove_attr.restype = c.c_uint32
    lib.lumen_node_set_text.argtypes = [Node, c.c_char_p]
    lib.lumen_node_set_text.restype = c.c_uint32
    lib.lumen_node_set_inner_markup.argtypes = [Node, c.c_char_p]
    lib.lumen_node_set_inner_markup.restype = c.c_uint32
    lib.lumen_node_class_add.argtypes = [Node, c.c_char_p]
    lib.lumen_node_class_add.restype = c.c_uint32
    lib.lumen_node_class_remove.argtypes = [Node, c.c_char_p]
    lib.lumen_node_class_remove.restype = c.c_uint32
    lib.lumen_node_class_toggle.argtypes = [Node, c.c_char_p]
    lib.lumen_node_class_toggle.restype = c.c_uint32
    lib.lumen_node_set_style.argtypes = [Node, c.c_char_p, c.c_char_p]
    lib.lumen_node_set_style.restype = c.c_uint32
    lib.lumen_node_remove_style.argtypes = [Node, c.c_char_p]
    lib.lumen_node_remove_style.restype = c.c_uint32

    # -- structure --
    lib.lumen_node_spawn.argtypes = [c.c_char_p, PNode]
    lib.lumen_node_spawn.restype = c.c_uint32
    lib.lumen_document_spawn.argtypes = [c.c_char_p, PNode]
    lib.lumen_document_spawn.restype = c.c_uint32
    lib.lumen_node_clone.argtypes = [Node, PNode]
    lib.lumen_node_clone.restype = c.c_uint32
    lib.lumen_node_append.argtypes = [Node, Node]
    lib.lumen_node_append.restype = c.c_uint32
    lib.lumen_node_insert_before.argtypes = [Node, Node, Node]
    lib.lumen_node_insert_before.restype = c.c_uint32
    lib.lumen_node_set_parent.argtypes = [Node, Node]
    lib.lumen_node_set_parent.restype = c.c_uint32
    lib.lumen_node_replace_with.argtypes = [Node, Node]
    lib.lumen_node_replace_with.restype = c.c_uint32
    lib.lumen_node_remove.argtypes = [Node]
    lib.lumen_node_remove.restype = c.c_uint32

    # -- events --
    # The callback is a `_LumenEventFnRaw` closure cast to c_void_p at the
    # call site (same pattern as the expose / click / watch callbacks).
    lib.lumen_on.argtypes = [Node, c.c_char_p, c.c_int, c.c_void_p, c.c_void_p]
    lib.lumen_on.restype = c.c_uint64
    lib.lumen_off.argtypes = [c.c_uint64]
    lib.lumen_off.restype = c.c_uint32
    lib.lumen_event_type.argtypes = [c.c_char_p, c.c_size_t, P(c.c_size_t)]
    lib.lumen_event_type.restype = c.c_uint32
    lib.lumen_event_key.argtypes = [c.c_char_p, c.c_size_t, P(c.c_size_t)]
    lib.lumen_event_key.restype = c.c_uint32
    lib.lumen_event_value.argtypes = [c.c_char_p, c.c_size_t, P(c.c_size_t)]
    lib.lumen_event_value.restype = c.c_uint32
    lib.lumen_event_target.argtypes = []
    lib.lumen_event_target.restype = c.c_uint64
    lib.lumen_event_current_target.argtypes = []
    lib.lumen_event_current_target.restype = c.c_uint64
    lib.lumen_event_prevent_default.argtypes = []
    lib.lumen_event_prevent_default.restype = c.c_uint32
    lib.lumen_event_stop_propagation.argtypes = []
    lib.lumen_event_stop_propagation.restype = c.c_uint32
    lib.lumen_event_stop_immediate_propagation.argtypes = []
    lib.lumen_event_stop_immediate_propagation.restype = c.c_uint32
