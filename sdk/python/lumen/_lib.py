"""Shared library handle + status-raising helper.

Split out of :mod:`lumen.api` so the typed-signal layer
(:mod:`lumen.signals`, :mod:`lumen.raw`) can reuse them without importing
the :class:`~lumen.api.App` class (and without a circular import).
"""

from __future__ import annotations

import ctypes

from . import _ffi
from .errors import status_to_exception

_LIB: ctypes.CDLL | None = None


def get_library() -> ctypes.CDLL:
    """Return the process-wide loaded ``liblumen_ffi``, loading it (and
    checking its ABI version) on first use. See
    :func:`lumen._ffi.load_library`.
    """

    global _LIB
    if _LIB is None:
        _LIB = _ffi.load_library()
    return _LIB


def _raise_for_status(lib: ctypes.CDLL, status: int, op: str) -> None:
    """Raise the right :class:`~lumen.errors.LumenError` subclass for a
    non-OK status, using ``lumen_last_error()`` for detail and falling back
    to ``lumen_status_message()`` if no thread-local/global detail was
    recorded.
    """

    if status == _ffi.LumenStatus.OK:
        return
    detail_ptr = lib.lumen_last_error()
    detail = detail_ptr.decode("utf-8", errors="replace") if detail_ptr else None
    canonical_ptr = lib.lumen_status_message(status)
    canonical = (
        canonical_ptr.decode("utf-8", errors="replace")
        if canonical_ptr
        else "unknown status"
    )
    message = f"{op}: {detail}" if detail else f"{op}: {canonical}"
    raise status_to_exception(status, message)
