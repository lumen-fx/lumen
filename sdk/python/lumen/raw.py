"""Low-level signal surface - the thin, stringly layer under
:mod:`lumen.signals`.

Everyday code should reach for the typed reactive handles in the package
root instead: :class:`lumen.Signal`, :class:`lumen.Model`,
:func:`lumen.computed`. This module is the escape hatch for when you need
to talk to a raw ``lumen_signal_*`` C entry point directly - every method
here maps onto exactly one C call and takes/returns the primitive the ABI
speaks (names as ``str``, values pre-typed by which method you call).

:class:`Signal` (this module's ``lumen.raw.Signal``) mirrors
``lumen::Signal`` in the C++ SDK: a static, thread-safe, process-wide
namespace for named signals, usable from any thread before or after
:meth:`lumen.App.run` starts. Two families live here:

* The legacy string-based setters (``set_string`` / ``set_int`` /
  ``set_f64`` / ``set_array`` / ``clear``) stringify everything so
  ``bind-text`` / ``<for>`` markup can read it, plus their ABI-0.3
  read-back (``get_string`` / ``array_len`` / ``array_field``) which
  return the value the *embedder* last pushed through the FFI (not live
  in-app state).
* The typed scalar accessors (``set_int64`` / ``get_int64`` and friends)
  round-trip ``int`` / ``float`` / ``bool`` / ``color`` with no
  stringification.
"""

from __future__ import annotations

import ctypes

from . import _ffi
from ._lib import _raise_for_status, get_library

__all__ = ["Signal"]


class Signal:
    """Static, thread-safe, process-wide access to Lumen's named signals.

    Every method maps straight onto one ``lumen_signal_*`` C call. See the
    module docstring for the two families (legacy string vs typed scalar).

    :meth:`set` picks a typed setter from ``type(value)``; there is no
    generic :meth:`get` because which typed getter to call can't be
    inferred without already knowing the signal's type.
    """

    # -- legacy string-stringified family -----------------------------

    @staticmethod
    def set_string(name: str, value: str) -> None:
        lib = get_library()
        status = lib.lumen_signal_set_string(
            name.encode("utf-8"), value.encode("utf-8")
        )
        _raise_for_status(lib, status, "lumen_signal_set_string")

    @staticmethod
    def set_int(name: str, value: int) -> None:
        """Legacy stringified setter. Prefer :meth:`set_int64` - this one
        round-trips through a ``Display`` string on the Rust side and
        cannot be read back typed."""

        lib = get_library()
        status = lib.lumen_signal_set_int(name.encode("utf-8"), value)
        _raise_for_status(lib, status, "lumen_signal_set_int")

    @staticmethod
    def set_f64(name: str, value: float) -> None:
        """Legacy stringified setter. Prefer :meth:`set_float64`."""

        lib = get_library()
        status = lib.lumen_signal_set_f64(name.encode("utf-8"), value)
        _raise_for_status(lib, status, "lumen_signal_set_f64")

    @staticmethod
    def clear(name: str) -> None:
        """Clear a signal (string -> empty, array -> empty list)."""

        lib = get_library()
        status = lib.lumen_signal_clear(name.encode("utf-8"))
        _raise_for_status(lib, status, "lumen_signal_clear")

    @staticmethod
    def set_array(name: str, rows: list[dict]) -> None:
        """Replace an array signal consumed by ``<for>`` markup. Each row
        must be a ``dict``; non-string values are stringified by Lumen
        before insertion."""

        lib = get_library()
        keepalive: list = []
        lv = _ffi.to_lumen_value(list(rows), keepalive)
        status = lib.lumen_signal_set_array(name.encode("utf-8"), ctypes.byref(lv))
        _raise_for_status(lib, status, "lumen_signal_set_array")

    # -- typed scalar family (read + write) ---------------------------

    @staticmethod
    def set_int64(name: str, value: int) -> None:
        lib = get_library()
        status = lib.lumen_signal_set_int64(None, name.encode("utf-8"), value)
        _raise_for_status(lib, status, "lumen_signal_set_int64")

    @staticmethod
    def get_int64(name: str) -> int:
        lib = get_library()
        out = ctypes.c_int64()
        status = lib.lumen_signal_get_int64(None, name.encode("utf-8"), ctypes.byref(out))
        _raise_for_status(lib, status, "lumen_signal_get_int64")
        return out.value

    @staticmethod
    def set_float64(name: str, value: float) -> None:
        lib = get_library()
        status = lib.lumen_signal_set_float64(None, name.encode("utf-8"), value)
        _raise_for_status(lib, status, "lumen_signal_set_float64")

    @staticmethod
    def get_float64(name: str) -> float:
        lib = get_library()
        out = ctypes.c_double()
        status = lib.lumen_signal_get_float64(None, name.encode("utf-8"), ctypes.byref(out))
        _raise_for_status(lib, status, "lumen_signal_get_float64")
        return out.value

    @staticmethod
    def set_bool(name: str, value: bool) -> None:
        lib = get_library()
        status = lib.lumen_signal_set_bool(None, name.encode("utf-8"), bool(value))
        _raise_for_status(lib, status, "lumen_signal_set_bool")

    @staticmethod
    def get_bool(name: str) -> bool:
        lib = get_library()
        out = ctypes.c_bool()
        status = lib.lumen_signal_get_bool(None, name.encode("utf-8"), ctypes.byref(out))
        _raise_for_status(lib, status, "lumen_signal_get_bool")
        return bool(out.value)

    @staticmethod
    def set_color(name: str, rgba: tuple[int, int, int, int]) -> None:
        """``rgba``: 4 ints in ``0..=255`` (R, G, B, A)."""

        lib = get_library()
        buf = (ctypes.c_uint8 * 4)(*rgba)
        status = lib.lumen_signal_set_color(None, name.encode("utf-8"), buf)
        _raise_for_status(lib, status, "lumen_signal_set_color")

    @staticmethod
    def get_color(name: str) -> tuple[int, int, int, int]:
        lib = get_library()
        buf = (ctypes.c_uint8 * 4)()
        status = lib.lumen_signal_get_color(None, name.encode("utf-8"), buf)
        _raise_for_status(lib, status, "lumen_signal_get_color")
        return (buf[0], buf[1], buf[2], buf[3])

    # -- string / array read-back (ABI 0.3) ---------------------------

    @staticmethod
    def _get_string_via(lib, cfunc, op, *lead_args) -> str:
        """Two-call size-then-fill helper for the string-out ABI
        convention shared by ``get_string`` and ``array_field``."""

        out_len = ctypes.c_size_t(0)
        status = cfunc(*lead_args, None, 0, ctypes.byref(out_len))
        if status == _ffi.LumenStatus.ERR_BUFFER_TOO_SMALL:
            buf = ctypes.create_string_buffer(out_len.value)
            status = cfunc(*lead_args, buf, out_len.value, ctypes.byref(out_len))
            _raise_for_status(lib, status, op)
            return buf.value.decode("utf-8", errors="replace")
        _raise_for_status(lib, status, op)
        return ""

    @staticmethod
    def get_string(name: str) -> str:
        """Read back a string signal previously set through the FFI string
        setters. Returns the value the *embedder* last pushed through the
        FFI, not live in-app state."""

        lib = get_library()
        return Signal._get_string_via(
            lib, lib.lumen_signal_get_string, "lumen_signal_get_string",
            None, name.encode("utf-8"),
        )

    @staticmethod
    def array_len(name: str) -> int:
        """Number of rows in an array signal previously set through
        :meth:`set_array`."""

        lib = get_library()
        out = ctypes.c_size_t(0)
        status = lib.lumen_signal_array_len(None, name.encode("utf-8"), ctypes.byref(out))
        _raise_for_status(lib, status, "lumen_signal_array_len")
        return out.value

    @staticmethod
    def array_field(name: str, row: int, field: str) -> str:
        """Read one field of one row (``record[field]`` as a string) of an
        array signal set through :meth:`set_array`."""

        lib = get_library()
        return Signal._get_string_via(
            lib, lib.lumen_signal_array_get_field, "lumen_signal_array_get_field",
            None, name.encode("utf-8"), ctypes.c_size_t(row), field.encode("utf-8"),
        )

    # -- Pythonic dispatch-by-type ------------------------------------

    @staticmethod
    def set(name: str, value: bool | int | float | str | list) -> None:
        """Set a signal, picking the right typed setter from
        ``type(value)`` (``bool`` before ``int`` - ``bool`` subclasses
        ``int`` in Python). For colors call :meth:`set_color` directly."""

        if isinstance(value, bool):
            Signal.set_bool(name, value)
        elif isinstance(value, int):
            Signal.set_int64(name, value)
        elif isinstance(value, float):
            Signal.set_float64(name, value)
        elif isinstance(value, str):
            Signal.set_string(name, value)
        elif isinstance(value, (list, tuple)):
            Signal.set_array(name, list(value))
        else:
            raise TypeError(
                f"Signal.set: no mapping for {type(value).__name__}; "
                "use set_string/set_int64/set_float64/set_bool/set_color/"
                "set_array explicitly"
            )
