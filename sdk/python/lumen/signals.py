"""Typed reactive signal handles - the effortless surface.

:class:`Signal` is a typed handle over one named Lumen signal. Construct
it with a name and (optionally) an initial value; read and write it with
:attr:`~Signal.value` / :meth:`~Signal.get` / :meth:`~Signal.set` (or the
``+=`` / ``-=`` operators for numeric signals). Subscribe to changes with
:meth:`~Signal.watch`, which fires on the Lumen tick thread every time the
value commits - a real ABI subscription (``lumen_signal_watch``), not a
polling loop.

    import lumen

    count = lumen.Signal("count", 0)          # inferred int64
    count += 1                                 # typed, syncs to the runtime

    @count.watch
    def _(new_value):
        print("count is now", new_value)

Supported value types: ``int`` (int64), ``float`` (float64), ``bool``,
``str``, :class:`Color` (RGBA), and ``list`` (array signals for ``<for>``
markup, rows are ``dict``s). Watches only fire while an app is running
(``App.run`` / ``App.run_headless``) - that is when ticks happen.
"""

from __future__ import annotations

import ctypes
from typing import Callable, Generic, TypeVar

from . import _ffi
from ._lib import _raise_for_status, get_library
from .raw import Signal as _raw

__all__ = ["Signal", "Color", "computed"]

T = TypeVar("T")

# Sentinel distinct from ``None`` (a valid absent-initial marker; ``None``
# is not itself a storable signal value here).
_UNSET = object()

# ctypes callback objects handed to ``lumen_signal_watch`` must stay
# referenced for as long as the app can still deliver changes. Watches
# have no unsubscribe in the ABI, so this process-lifetime anchor is the
# right scope.
_WATCH_KEEPALIVE: list = []


class Color(tuple):
    """An RGBA color, four ints in ``0..=255``. A thin ``tuple`` subclass so
    ``Color(255, 128, 0, 255)`` behaves like ``(255, 128, 0, 255)`` while
    still being recognised as a color-typed signal value.

    Construct it component-wise (``Color(255, 128, 0)``), from a CSS-style
    hex string (``Color("#ff8000")`` / ``Color("#ff8000ff")`` / short
    ``Color("#f80")``), or via :meth:`from_hex`. The named channels are
    exposed as :attr:`r` / :attr:`g` / :attr:`b` / :attr:`a`.
    """

    __slots__ = ()

    def __new__(
        cls,
        r: int | str,
        g: int | None = None,
        b: int | None = None,
        a: int = 255,
    ) -> "Color":
        # ``Color("#ff8000")`` convenience: a lone string is parsed as hex.
        if isinstance(r, str):
            if g is not None or b is not None:
                raise TypeError(
                    "Color(hex): pass only the hex string, no other channels"
                )
            return cls.from_hex(r)
        if g is None or b is None:
            raise TypeError("Color(r, g, b[, a]): r, g and b are all required")
        return super().__new__(cls, (int(r), int(g), int(b), int(a)))

    @property
    def r(self) -> int:
        """Red channel, ``0..=255``."""
        return self[0]

    @property
    def g(self) -> int:
        """Green channel, ``0..=255``."""
        return self[1]

    @property
    def b(self) -> int:
        """Blue channel, ``0..=255``."""
        return self[2]

    @property
    def a(self) -> int:
        """Alpha channel, ``0..=255`` (opaque by default)."""
        return self[3]

    @classmethod
    def from_hex(cls, value: str) -> "Color":
        """Parse a CSS-style hex color. Accepts ``#rgb``, ``#rgba``,
        ``#rrggbb`` and ``#rrggbbaa`` (the leading ``#`` is optional).
        Short forms are expanded per CSS (``#f80`` -> ``#ff8800``)."""

        s = value.strip().lstrip("#")
        if len(s) in (3, 4):  # short form: expand each nibble
            s = "".join(ch * 2 for ch in s)
        if len(s) == 6:
            s += "ff"
        if len(s) != 8:
            raise ValueError(
                f"Color.from_hex: {value!r} is not #rgb / #rgba / #rrggbb / "
                "#rrggbbaa"
            )
        try:
            r, g, b, alpha = (int(s[i : i + 2], 16) for i in (0, 2, 4, 6))
        except ValueError:
            raise ValueError(
                f"Color.from_hex: {value!r} has non-hex digits"
            ) from None
        return super().__new__(cls, (r, g, b, alpha))

    def to_hex(self) -> str:
        """Render as ``#rrggbbaa`` (lower-case, always 8 digits)."""
        return "#{:02x}{:02x}{:02x}{:02x}".format(*self)

    @classmethod
    def _from_packed(cls, v: int) -> "Color":
        """Decode the ``0xRRGGBBAA`` int the watch ABI delivers for a color
        signal (see ``LumenWatchFn`` in ``lumen.h``)."""

        return cls((v >> 24) & 0xFF, (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF)

    def __repr__(self) -> str:
        return f"Color(r={self[0]}, g={self[1]}, b={self[2]}, a={self[3]})"


def _kind_of(type_hint: object, initial: object) -> str:
    """Resolve a signal's storage kind from an explicit type hint (a type
    object or its ``str`` name, tolerating ``from __future__ import
    annotations``) and/or an initial value. ``bool`` is checked before
    ``int`` because ``bool`` subclasses ``int`` in Python.
    """

    def from_type(t: object) -> str | None:
        if t is bool or t == "bool":
            return "bool"
        if t is int or t == "int":
            return "int"
        if t is float or t == "float":
            return "float"
        if t is str or t == "str":
            return "str"
        if t is Color or t == "Color":
            return "color"
        if t is list or t == "list" or getattr(t, "__origin__", None) is list:
            return "list"
        return None

    if type_hint is not None:
        kind = from_type(type_hint)
        if kind is not None:
            return kind
    if initial is not _UNSET and initial is not None:
        if isinstance(initial, Color):
            return "color"
        if isinstance(initial, bool):
            return "bool"
        if isinstance(initial, int):
            return "int"
        if isinstance(initial, float):
            return "float"
        if isinstance(initial, str):
            return "str"
        if isinstance(initial, (list, tuple)):
            return "list"
    raise TypeError(
        "Signal: cannot infer type; pass an initial value or "
        "type=<int|float|bool|str|Color|list>"
    )


class Signal(Generic[T]):
    """A typed handle over one named Lumen signal.

    Args:
        name: The signal name markup binds against (``bind-text="name"`` /
            ``<for>``) and the runtime keys on.
        initial: Optional initial value; when given, it is pushed to the
            runtime immediately and its Python type infers the signal kind.
        type: Explicit kind override (``int`` / ``float`` / ``bool`` /
            ``str`` / :class:`Color` / ``list``), needed when no ``initial``
            is supplied or to widen an ``int`` literal to ``float``.
    """

    def __init__(
        self,
        name: str,
        initial: object = _UNSET,
        *,
        type: object = None,
    ) -> None:
        self.name = name
        self._kind = _kind_of(type, initial)
        # For array (``list``) signals the Python list is the source of
        # truth - the read-back ABI is row/field-scoped and lossy, so we
        # keep the structured value here and push it whole on write.
        self._local_list: list | None = [] if self._kind == "list" else None
        if initial is not _UNSET:
            self.set(initial)

    # -- read / write --------------------------------------------------

    def get(self) -> T:
        """Read the current value, typed."""

        k = self._kind
        if k == "int":
            return _raw.get_int64(self.name)  # type: ignore[return-value]
        if k == "float":
            return _raw.get_float64(self.name)  # type: ignore[return-value]
        if k == "bool":
            return _raw.get_bool(self.name)  # type: ignore[return-value]
        if k == "str":
            return _raw.get_str(self.name)  # type: ignore[return-value]
        if k == "color":
            return Color(*_raw.get_color(self.name))  # type: ignore[return-value]
        # list: the Python-side structured value is authoritative.
        return list(self._local_list or [])  # type: ignore[return-value]

    def set(self, value: T) -> None:
        """Write ``value`` to the runtime, typed."""

        k = self._kind
        if k == "int":
            _raw.set_int64(self.name, int(value))  # type: ignore[arg-type]
        elif k == "float":
            _raw.set_float64(self.name, float(value))  # type: ignore[arg-type]
        elif k == "bool":
            _raw.set_bool(self.name, bool(value))
        elif k == "str":
            _raw.set_str(self.name, str(value))
        elif k == "color":
            _raw.set_color(self.name, tuple(value))  # type: ignore[arg-type]
        else:  # list
            rows = [dict(r) for r in value]  # type: ignore[union-attr]
            self._local_list = rows
            _raw.set_array(self.name, rows)

    @property
    def value(self) -> T:
        """The current value. ``signal.value`` reads, ``signal.value = x``
        writes - sugar over :meth:`get` / :meth:`set`."""

        return self.get()

    @value.setter
    def value(self, new_value: T) -> None:
        self.set(new_value)

    # -- numeric convenience operators --------------------------------

    def __iadd__(self, delta: object) -> "Signal[T]":
        self.set(self.get() + delta)  # type: ignore[operator]
        return self

    def __isub__(self, delta: object) -> "Signal[T]":
        self.set(self.get() - delta)  # type: ignore[operator]
        return self

    # -- subscription --------------------------------------------------

    def watch(self, fn: Callable[[T], object]) -> Callable[[T], object]:
        """Call ``fn(new_value)`` every time this signal's value commits,
        on the Lumen tick thread. Usable as a plain call (``sig.watch(fn)``)
        or a decorator (``@sig.watch``). Returns ``fn`` so decorator use
        keeps the name bound.

        Fires only while an app is running (``App.run`` /
        ``App.run_headless``); a freshly-registered watch also fires once
        with the current value on the first tick it is observed. Keep a slow
        handler off this thread - it stalls the event loop.
        """

        kind = self._kind

        def on_change(_name: str, raw_value: object) -> None:
            if kind == "color" and isinstance(raw_value, int):
                fn(Color._from_packed(raw_value))  # type: ignore[arg-type]
            else:
                fn(raw_value)  # type: ignore[arg-type]

        lib = get_library()
        cb = _ffi.make_watch_callback(on_change)
        _WATCH_KEEPALIVE.append(cb)
        status = lib.lumen_signal_watch(
            self.name.encode("utf-8"), ctypes.cast(cb, ctypes.c_void_p), None
        )
        _raise_for_status(lib, status, f"lumen_signal_watch({self.name!r})")
        return fn

    def __repr__(self) -> str:
        return f"Signal(name={self.name!r}, kind={self._kind!r})"


def _bind_computed(
    target: "Signal[object] | str",
    fn: Callable[[], object],
    deps: "tuple[Signal[object], ...]",
) -> "Signal[object]":
    initial = fn()
    if isinstance(target, Signal):
        sig = target
        sig.set(initial)
    else:
        sig = Signal(target, initial)

    def recompute(_new_value: object) -> None:
        sig.set(fn())

    for dep in deps:
        dep.watch(recompute)
    return sig


def computed(
    target: "Signal[object] | str",
    fn: "Callable[[], object] | Signal[object] | None" = None,
    *deps: "Signal[object]",
):
    """Derive ``target`` from a function, recomputing whenever any
    dependency changes - sugar over :meth:`Signal.watch`.

    ``target`` is the derived signal (a :class:`Signal`, or a name to wrap
    in one inferred from the function's first result). Two call styles:

    Direct - pass the function then the dependency signals::

        count = lumen.Signal("count", 0)
        computed("label", lambda: f"{count.value} clicks", count)

    Decorator - pass only the dependency signals; decorate the function::

        count = lumen.Signal("count", 0)

        @computed("label", count)
        def label():
            return f"{count.value} clicks"

    The direct form returns the derived :class:`Signal`; the decorator
    form returns the (undecorated) function so its name stays bound.
    """

    # Decorator form: `computed(target, *deps)` where every trailing
    # argument is a Signal (or there are none) -> return a decorator.
    # A plain callable in the `fn` slot (a lambda/def, *not* a Signal,
    # which is not callable) selects the direct form.
    if fn is None or isinstance(fn, Signal):
        dep_signals = tuple(d for d in (fn, *deps) if d is not None)

        def decorator(func: Callable[[], object]) -> Callable[[], object]:
            _bind_computed(target, func, dep_signals)  # type: ignore[arg-type]
            return func

        return decorator

    return _bind_computed(target, fn, deps)
