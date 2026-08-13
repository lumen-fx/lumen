"""Pythonic wrapper over the Lumen C ABI.

:class:`App` is the main entry point - a context manager around the
opaque ``LumenApp*`` handle. The typed reactive signal surface
(:class:`lumen.Signal`, :class:`lumen.Model`, :func:`lumen.computed`)
lives in :mod:`lumen.signals` / :mod:`lumen.model`; the raw stringly
``lumen_signal_*`` layer is :mod:`lumen.raw`.

Threading note: ``lumen_app_expose`` callbacks - including the ones
registered by :meth:`App.on_click` and friends - fire on *Lumen's*
script thread, not necessarily the thread that called :meth:`App.run`.
ctypes acquires the GIL for you automatically before calling into
Python, so this is safe, but it also means **a slow or blocking
handler stalls the whole Lumen event loop** (rendering, input, timers,
everything) until it returns. Keep handlers fast; hand off real work
to a Python thread and have the handler just kick it off.

A second, more subtle threading note: Lumen's ECS scheduler
(`bevy_ecs` with the `multi_threaded` feature) and its Rhai host (built
with the `sync` feature specifically so its `Engine`/`Dynamic` values
are `Send + Sync`) make it architecturally possible for more than one
worker thread to be the one that ends up calling into an exposed
native function, even if in practice a given Lumen build only ever
dispatches one click at a time. `App` does not assume single-threaded
callback delivery: every per-id dispatcher installed by
`on_click`/`on_double_click`/`on_long_press` serializes the lookup +
handler call under `self._dispatch_lock`, so two concurrent deliveries
for different ids can't interleave a lookup in `self._click_handlers`
(etc.) with a concurrent mutation of it, and two handlers can't run
their bodies interleaved against each other. It does **not** protect
state *inside* your own handler that you also touch from other Python
threads you spawned yourself -- that's still on you.
"""

from __future__ import annotations

import ctypes
import inspect
import os
import threading
from typing import Callable

from . import _ffi
from ._lib import _raise_for_status, get_library
from .errors import LumenInvalidHandleError, status_to_exception

__all__ = ["App", "get_library"]


def _positional_arity(func: Callable[..., object]) -> int | None:
    """Number of positional parameters ``func`` accepts, or ``None`` when
    it takes a variable number (``*args``) or its signature can't be
    introspected (some C builtins). Used to let event handlers omit the
    ``element_id`` argument they don't care about, and to infer an
    exposed function's ``arg_count`` from its own signature.
    """

    try:
        params = inspect.signature(func).parameters.values()
    except (TypeError, ValueError):
        return None
    count = 0
    for p in params:
        if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD):
            count += 1
        elif p.kind == p.VAR_POSITIONAL:
            return None
    return count


def _adapt_id_handler(
    func: Callable[..., object],
) -> Callable[[str], object]:
    """Wrap an event handler so it is always callable as ``handler(id)``.

    Handlers that declare no positional parameters are invoked with none,
    so ``def on_bump(): ...`` and ``def on_bump(element_id): ...`` both
    work - matching the C++ SDK's ``void()`` / ``void(std::string)``
    overloads. ``*args`` handlers and un-introspectable callables receive
    the id (the safe default).
    """

    if _positional_arity(func) == 0:
        return lambda _id, _f=func: _f()
    return func


# ============================================================
# App
# ============================================================


class App:
    """Owns one ``LumenApp*`` handle.

    Use as a context manager so the handle is always freed even if you
    never call :meth:`run`::

        with App("my-app") as app:
            @app.on_click("increment")
            def _(element_id: str) -> None:
                ...
            app.run()

    ``run()`` *consumes* the handle (matching ``lumen_app_run``'s C
    contract) and blocks until the window closes - it opens a real OS
    window, so it cannot be exercised in a headless/CI environment.
    Everything else on this class (``expose``, ``on_click``, signal
    access, ``set_title``/``set_size``) works before a window exists.
    """

    def __init__(
        self,
        directory: str | os.PathLike,
        *,
        title: str | None = None,
        size: tuple[int, int] | None = None,
    ) -> None:
        self._lib = get_library()
        dir_str = os.fspath(directory)
        ptr = self._lib.lumen_app_new(dir_str.encode("utf-8"))
        if not ptr:
            detail_ptr = self._lib.lumen_last_error()
            detail = (
                detail_ptr.decode("utf-8", errors="replace")
                if detail_ptr
                else "lumen_app_new returned NULL"
            )
            raise status_to_exception(
                _ffi.LumenStatus.ERR_BAD_PATH, f"lumen_app_new({dir_str!r}): {detail}"
            )
        self._ptr: int | None = ptr
        self._consumed = False

        # Every ctypes.CFUNCTYPE instance we hand to `lumen_app_expose`
        # must stay referenced for as long as Lumen might call it --
        # ctypes doesn't keep the callback trampoline alive on the C
        # side, only on the Python side. This list is that anchor.
        self._callbacks: list[_ffi.LumenFn] = []

        # element_id -> python handler, one dict per event kind. Built
        # lazily: the first `@app.on_click(...)` (etc.) installs a
        # single native dispatcher for that event kind and all
        # subsequent registrations just add to the dict.
        self._click_handlers: dict[str, Callable[[str], object]] = {}
        self._double_click_handlers: dict[str, Callable[[str], object]] = {}
        self._long_press_handlers: dict[str, Callable[[str], object]] = {}
        self._installed_dispatchers: set[str] = set()
        # Serializes per-id dispatch table lookups + handler calls --
        # see the module docstring's threading note on why concurrent
        # delivery is architecturally possible even if a given build
        # never actually exercises it.
        self._dispatch_lock = threading.RLock()

        if title is not None:
            self.set_title(title)
        if size is not None:
            self.set_size(*size)

    # -- lifecycle --------------------------------------------------

    def __enter__(self) -> "App":
        return self

    def __exit__(self, *exc_info: object) -> bool:
        self.close()
        return False

    def set_title(self, title: str) -> "App":
        """Override the window title. Must be called before :meth:`run`."""

        self._require_live()
        status = self._lib.lumen_app_set_title(self._ptr, title.encode("utf-8"))
        _raise_for_status(self._lib, status, "lumen_app_set_title")
        return self

    def set_size(self, width: int, height: int) -> "App":
        """Override the initial window size, in logical pixels."""

        self._require_live()
        status = self._lib.lumen_app_set_size(self._ptr, width, height)
        _raise_for_status(self._lib, status, "lumen_app_set_size")
        return self

    def run(self) -> None:
        """Consume the handle and enter the Lumen event loop. Blocks
        until the window closes. Opens a real OS window/GPU surface; for
        CI / no-display environments use :meth:`run_headless` instead.

        After this returns (or raises), the handle is gone; do not
        call any other method on this ``App`` afterwards.
        """

        self._require_live()
        ptr = self._ptr
        self._ptr = None
        self._consumed = True
        status = self._lib.lumen_app_run(ptr)
        _raise_for_status(self._lib, status, "lumen_app_run")

    def run_headless(self, ticks: int = 1) -> None:
        """Consume the handle and drive ``ticks`` main-schedule ticks
        **without** opening a window or GPU surface (ABI 0.3).

        Builds the full app -- same plugin stack, scripts, and reactive
        bindings as :meth:`run` -- and ticks it ``ticks`` times. Signal
        round-trips, exposed-callback wiring, script execution, and
        ``<for>`` / ``<if>`` reconciliation all run; there is no window,
        no input source, and no GPU rendering. This is the CI entry
        point: exercise callback wiring and signal regressions without
        ``xvfb-run`` or a real compositor.

        Native ``@app.on_click`` handlers do not fire here (no input is
        injected in headless mode). ``ticks=0`` builds-and-drops, which
        validates that the app loads. After this returns (or raises) the
        handle is gone.
        """

        self._require_live()
        ptr = self._ptr
        self._ptr = None
        self._consumed = True
        status = self._lib.lumen_app_run_headless(ptr, int(ticks))
        _raise_for_status(self._lib, status, "lumen_app_run_headless")

    def close(self) -> None:
        """Free the handle without running it. Safe to call more than
        once, and safe to call after :meth:`run` (a no-op in that case).
        """

        if self._ptr is not None:
            self._lib.lumen_app_free(self._ptr)
            self._ptr = None
        self._consumed = True

    def __del__(self) -> None:  # best-effort safety net, not a substitute for close()/`with`
        try:
            self.close()
        except Exception:
            pass

    def _require_live(self) -> None:
        if self._ptr is None:
            raise LumenInvalidHandleError(
                "this App handle has already been run or closed "
                "(lumen_app_run consumes the handle; lumen_app_free frees it)"
            )

    # -- native callbacks --------------------------------------------

    def expose(self, name: str, arg_count: int | None = None):
        """Decorator: expose ``func`` to the app's script as the native
        builtin ``name``. A Rhai or Lua script calls it as
        ``name(arg0, arg1, ...)``; a candela script declares
        ``host "native" { any name(...); }`` and calls
        ``native::name(arg0, arg1, ...)``.

        ``arg_count`` defaults to ``None``, which infers the arity from
        ``func``'s own signature (its number of positional parameters) -
        so ``@app.expose("greet")`` on ``def greet(name): ...`` registers
        a 1-argument builtin automatically. Pass an explicit integer to
        override (e.g. for ``*args`` handlers, whose arity can't be
        inferred).

        ``func`` receives ``arg_count`` plain Python positional
        arguments (already converted from ``LumenValue`` -- ``None``/
        ``bool``/``int``/``float``/``str``/``list``/``dict``) and may
        return any of those types (``None`` becomes the script's unit
        value).
        An exception raised inside ``func`` is caught, mapped to a nil
        return value so it can't unwind across the FFI boundary as a
        Rust panic, and re-raised into ``sys.excepthook`` via
        ``threading.excepthook``-style reporting is NOT done for you --
        wrap risky code in your own try/except if you need finer
        control; an uncaught exception here is printed via
        ``traceback.print_exc()`` and swallowed so the script always
        gets a value back.

        Fires on the Lumen script thread -- see the module docstring's
        threading note. Do not block.
        """

        self._require_live()

        def decorator(func: Callable[..., object]) -> Callable[..., object]:
            resolved_argc = arg_count
            if resolved_argc is None:
                inferred = _positional_arity(func)
                resolved_argc = inferred if inferred is not None else 0

            def trampoline(
                argc: int,
                argv: "ctypes._Pointer[_ffi.LumenValue]",
                user_data: int,
            ) -> _ffi.LumenValue:
                try:
                    args = [_ffi.from_lumen_value(argv[i]) for i in range(argc)]
                    result = func(*args)
                except Exception:
                    import traceback

                    traceback.print_exc()
                    result = None
                keepalive: list = []
                lv = _ffi.to_lumen_value(result, keepalive)
                # Pin the buffers backing any ARRAY/MAP payload to the
                # returned struct's own lifetime -- ctypes already does
                # this automatically for scalar pointer fields (STRING)
                # via its `_objects` bookkeeping, but our own nested
                # ctypes arrays need an explicit anchor.
                lv._lumen_keepalive = keepalive  # type: ignore[attr-defined]
                return lv

            # `_ffi.make_callback` builds the ABI 0.3 out-parameter
            # callback (LumenFnV2). We register through
            # `lumen_app_expose_v2`, so there is no by-value struct
            # return crossing the boundary and no hand-encoded sret
            # convention -- the out-pointer is part of the ABI contract
            # and portable to every target, not just x86_64 Linux.
            c_callback = _ffi.make_callback(trampoline)
            self._callbacks.append(c_callback)
            status = self._lib.lumen_app_expose_v2(
                self._ptr,
                name.encode("utf-8"),
                resolved_argc,
                ctypes.cast(c_callback, ctypes.c_void_p),
                None,
            )
            _raise_for_status(self._lib, status, f"lumen_app_expose_v2({name!r})")
            return func

        return decorator

    # -- per-id event decorators --------------------------------------
    #
    # The C ABI has no click-routing concept of its own -- clicks are
    # dispatched by the Rhai runtime to global script functions
    # (`on_click(id)`, `on_double_click(id)`, `on_long_press(id)`), or
    # per-element via the script-side `on("click", id, "fn_name")`
    # router (see apps/scroll-tiles/main.lmn). These decorators assume your
    # main.lmn/main.rhai forwards those globals to one exposed native
    # function per event kind, e.g.:
    #
    #   fn on_click(id) { __lumen_py_on_click(id); }
    #
    # `examples/counter_app/main.lmn` in this SDK shows the full
    # wiring for all three event kinds.

    def on_click(
        self,
        element_id: str,
        handler: Callable[..., object] | None = None,
    ):
        """Register a click handler for ``element_id``. Usable as a
        decorator (``@app.on_click("bump")``) or a direct call
        (``app.on_click("bump", fn)``).

        The handler may take the ``element_id`` string or no argument at
        all - ``def on_bump(): ...`` and ``def on_bump(element_id): ...``
        both work (mirrors the C++ SDK's ``void()`` / ``void(string)``
        overloads).

        As of ABI 0.3 this routes through the runtime's own id-scoped
        native click hook (``lumen_app_on_click``): the framework calls
        your handler directly for the matching element id, so **no
        ``main.lmn`` forwarding is required** -- you do not need an
        ``fn on_click(id) { __lumen_py_on_click(id); }`` shim in the
        markup for clicks. A second registration for the same id
        replaces the first (matches the C ABI's documented behaviour).

        (``on_double_click`` / ``on_long_press`` still use the
        script-forwarding path below -- the native hook currently covers
        the click event only.)
        """

        self._require_live()

        def decorator(func: Callable[..., object]) -> Callable[..., object]:
            adapted = _adapt_id_handler(func)

            # Register one native callback per id. It looks the current
            # handler up under the dispatch lock (so a concurrent
            # re-registration can't tear it) and calls it.
            def dispatch(id_: str) -> None:
                with self._dispatch_lock:
                    h = self._click_handlers.get(id_)
                if h is not None:
                    h(id_)

            with self._dispatch_lock:
                self._click_handlers[element_id] = adapted
            c_callback = _ffi.make_click_callback(dispatch)
            self._callbacks.append(c_callback)
            status = self._lib.lumen_app_on_click(
                self._ptr,
                element_id.encode("utf-8"),
                ctypes.cast(c_callback, ctypes.c_void_p),
                None,
            )
            _raise_for_status(self._lib, status, f"lumen_app_on_click({element_id!r})")
            return func

        if handler is not None:  # direct-call form: register now, return it
            return decorator(handler)
        return decorator

    def on_double_click(
        self,
        element_id: str,
        handler: Callable[..., object] | None = None,
    ):
        """Register a double-click handler for ``element_id``. Decorator
        or direct-call (``app.on_double_click(id, fn)``); the handler may
        take ``element_id`` or no argument. Requires ``on_double_click(id)``
        to forward to ``__lumen_py_on_double_click``."""

        return self._on_id_event(
            "double_click",
            "__lumen_py_on_double_click",
            self._double_click_handlers,
            element_id,
            handler,
        )

    def on_long_press(
        self,
        element_id: str,
        handler: Callable[..., object] | None = None,
    ):
        """Register a long-press handler for ``element_id``. Decorator or
        direct-call (``app.on_long_press(id, fn)``); the handler may take
        ``element_id`` or no argument. Requires ``on_long_press(id)`` to
        forward to ``__lumen_py_on_long_press``."""

        return self._on_id_event(
            "long_press",
            "__lumen_py_on_long_press",
            self._long_press_handlers,
            element_id,
            handler,
        )

    def _on_id_event(
        self,
        kind: str,
        dispatcher_name: str,
        table: dict[str, Callable[[str], object]],
        element_id: str,
        handler: Callable[..., object] | None = None,
    ):
        if kind not in self._installed_dispatchers:
            self._installed_dispatchers.add(kind)

            @self.expose(dispatcher_name, arg_count=1)
            def _dispatch(id_: str, _table=table) -> None:
                # Serialized: see `self._dispatch_lock`'s docstring note.
                # Table lookup and the handler call happen as one unit so
                # a concurrent `on_click(...)` registration (or a second
                # concurrent delivery) can't interleave with it.
                with self._dispatch_lock:
                    handler = _table.get(id_)
                    if handler is not None:
                        handler(id_)
                return None

        def decorator(func: Callable[..., object]) -> Callable[..., object]:
            with self._dispatch_lock:
                table[element_id] = _adapt_id_handler(func)
            return func

        if handler is not None:  # direct-call form: register now, return it
            return decorator(handler)
        return decorator
