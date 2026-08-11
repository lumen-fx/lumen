"""Exception hierarchy mirroring the C ABI's ``LumenStatus`` codes.

Every ``lumen_*`` C entry point returns a ``LumenStatus`` (an unsigned
32-bit enum, see ``src/lib.rs``). This module maps each
numeric code onto a dedicated Python exception class so callers can
``except LumenCssError`` instead of string-matching an error message.

All of them derive from :class:`LumenError`, which itself derives from
``RuntimeError`` so code that only cares "did a Lumen call fail" can
catch the base class.

Two extra exceptions exist that have no ``LumenStatus`` counterpart:

* :class:`LumenLibraryNotFoundError` - raised by ``_ffi.py`` when the
  shared library can't be located at all.
* :class:`LumenAbiVersionError` - raised by ``_ffi.py`` when the loaded
  library's ``lumen_abi_version()`` is incompatible with the version
  this SDK was written against.
"""

from __future__ import annotations

__all__ = [
    "LumenError",
    "LumenBadPathError",
    "LumenBadArgError",
    "LumenRuntimeError",
    "LumenInternalError",
    "LumenParseError",
    "LumenCssError",
    "LumenAssetError",
    "LumenWindowError",
    "LumenScriptError",
    "LumenIoError",
    "LumenInvalidHandleError",
    "LumenInvalidValueError",
    "LumenPanicError",
    "LumenBufferTooSmallError",
    "LumenLibraryNotFoundError",
    "LumenAbiVersionError",
    "status_to_exception",
]


class LumenError(RuntimeError):
    """Base class for every exception this SDK raises for a non-OK
    ``LumenStatus``.

    Attributes:
        status: The raw numeric ``LumenStatus`` code (``None`` for the
            two SDK-only errors that have no C-side status).
        message: The human-readable text, drawn from
            ``lumen_last_error()`` when available.
    """

    status: int | None = None

    def __init__(self, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.message = message
        if status is not None:
            self.status = status


class LumenBadPathError(LumenError):
    """LUMEN_ERR_BAD_PATH (1) - a path argument was missing or unresolvable."""

    status = 1


class LumenBadArgError(LumenError):
    """LUMEN_ERR_BAD_ARG (2) - a non-path argument was missing or malformed."""

    status = 2


class LumenRuntimeError(LumenError):
    """LUMEN_ERR_RUNTIME (3) - generic runtime error (catch-all)."""

    status = 3


class LumenInternalError(LumenError):
    """LUMEN_ERR_INTERNAL (4) - a Rust panic was caught at the FFI boundary."""

    status = 4


class LumenParseError(LumenError):
    """LUMEN_ERR_PARSE (5) - HTML / template parse failure."""

    status = 5


class LumenCssError(LumenError):
    """LUMEN_ERR_CSS (6) - CSS parse / cascade failure."""

    status = 6


class LumenAssetError(LumenError):
    """LUMEN_ERR_ASSET (7) - asset load / decode failure."""

    status = 7


class LumenWindowError(LumenError):
    """LUMEN_ERR_WINDOW (8) - window backend (winit / wgpu surface) failure."""

    status = 8


class LumenScriptError(LumenError):
    """LUMEN_ERR_SCRIPT (9) - Rhai script compile / runtime failure."""

    status = 9


class LumenIoError(LumenError):
    """LUMEN_ERR_IO (10) - generic I/O error (filesystem, network)."""

    status = 10


class LumenInvalidHandleError(LumenError):
    """LUMEN_ERR_INVALID_HANDLE (11) - a handle doesn't belong to a live
    ``LumenApp`` (use-after-free / use-after-run)."""

    status = 11


class LumenInvalidValueError(LumenError):
    """LUMEN_ERR_INVALID_VALUE (12) - a value was syntactically valid but
    semantically wrong (e.g. a ``kind``/payload mismatch)."""

    status = 12


class LumenPanicError(LumenError):
    """LUMEN_ERR_PANIC (13) - a Rust panic crossed the FFI boundary. Same
    numeric code as the legacy ``ErrInternal``; kept as an alias."""

    status = 13


class LumenBufferTooSmallError(LumenError):
    """LUMEN_ERR_BUFFER_TOO_SMALL (14) - a caller-provided output buffer
    was too small. The associated size out-parameter is set to the
    required capacity (byte length + 1 for the trailing NUL). Added in
    ABI 0.3 for the string / array read-back getters."""

    status = 14


class LumenLibraryNotFoundError(LumenError):
    """Raised when the ``lumen`` shared library could not be located.

    Not a ``LumenStatus`` - this happens before any C call is made.
    """


class LumenAbiVersionError(LumenError):
    """Raised when the loaded library's packed ABI version is incompatible
    with the version this SDK was written against.

    Not a ``LumenStatus`` - this happens before any C call is made.
    """


# Numeric LumenStatus -> exception class. LUMEN_OK (0) deliberately has
# no entry; callers must only consult this map on a non-zero status.
_STATUS_TABLE: dict[int, type[LumenError]] = {
    1: LumenBadPathError,
    2: LumenBadArgError,
    3: LumenRuntimeError,
    4: LumenInternalError,
    5: LumenParseError,
    6: LumenCssError,
    7: LumenAssetError,
    8: LumenWindowError,
    9: LumenScriptError,
    10: LumenIoError,
    11: LumenInvalidHandleError,
    12: LumenInvalidValueError,
    13: LumenPanicError,
    14: LumenBufferTooSmallError,
}


def status_to_exception(status: int, message: str) -> LumenError:
    """Build the right :class:`LumenError` subclass for a raw
    ``LumenStatus`` code. Unknown codes (future ABI additions this SDK
    doesn't know about yet) fall back to the base :class:`LumenError`
    so callers still get a catchable, informative exception instead of
    a ``KeyError``.
    """

    cls = _STATUS_TABLE.get(status, LumenError)
    return cls(message, status=status)
