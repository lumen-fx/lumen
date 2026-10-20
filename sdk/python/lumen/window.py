"""The app's window, for the Python SDK.

Thin wrappers over the ``lumen_window_*`` C exports, which apply to the
current window: the window of the event being handled, else the primary
window. The setters ask for a change, which the window applies after the
tick; the getters report what the window has, so a title or size reads back
once the app has ticked and the platform has applied it. The window manager
may refuse or adjust a size.

Usage::

    from lumen import window

    @app.on_click("compact")
    def compact(_):
        window.set_size(480, 320)

    @app.on_click("done")
    def done(_):
        window.close()      # runs the app's close hooks, which may veto
"""

from __future__ import annotations

import ctypes

from . import _ffi
from ._lib import _raise_for_status, get_library

__all__ = ["title", "set_title", "size", "set_size", "dpr", "close", "CURRENT"]

#: The current window (``LUMEN_WINDOW_CURRENT``).
CURRENT = 0


def title() -> str:
    """The title the window has."""

    lib = get_library()
    out_len = ctypes.c_size_t(0)
    status = lib.lumen_window_title(CURRENT, None, 0, ctypes.byref(out_len))
    if status == _ffi.LumenStatus.ERR_BUFFER_TOO_SMALL:
        buf = ctypes.create_string_buffer(out_len.value)
        status = lib.lumen_window_title(CURRENT, buf, out_len.value, ctypes.byref(out_len))
        _raise_for_status(lib, status, "lumen_window_title")
        return buf.value.decode("utf-8", errors="replace")
    _raise_for_status(lib, status, "lumen_window_title")
    return ""


def set_title(title: str) -> None:
    """Ask for a new window title."""

    lib = get_library()
    status = lib.lumen_window_set_title(CURRENT, title.encode("utf-8"))
    _raise_for_status(lib, status, "lumen_window_set_title")


def size() -> tuple[float, float]:
    """The window's inner size in logical pixels, ``(width, height)``."""

    lib = get_library()
    w, h = ctypes.c_float(0.0), ctypes.c_float(0.0)
    status = lib.lumen_window_size(CURRENT, ctypes.byref(w), ctypes.byref(h))
    _raise_for_status(lib, status, "lumen_window_size")
    return (w.value, h.value)


def set_size(width: float, height: float) -> None:
    """Ask for a new inner size in logical pixels. Both sides must be
    positive; otherwise :class:`~lumen.errors.LumenBadArgError` is raised."""

    lib = get_library()
    status = lib.lumen_window_set_size(CURRENT, float(width), float(height))
    _raise_for_status(lib, status, "lumen_window_set_size")


def dpr() -> float:
    """The window's device-pixel ratio."""

    lib = get_library()
    out = ctypes.c_float(1.0)
    status = lib.lumen_window_dpr(CURRENT, ctypes.byref(out))
    _raise_for_status(lib, status, "lumen_window_dpr")
    return out.value


def close() -> None:
    """Close the window, through the same veto path its close button takes:
    an ``on_close`` hook can keep it open. Closing the only window ends the
    app."""

    lib = get_library()
    _raise_for_status(lib, lib.lumen_window_close(CURRENT), "lumen_window_close")
