"""Dynamic DOM handles for the Python SDK.

A :class:`Node` is a thin wrapper over a live element in the running app: a
packed handle (index + generation) that marshals as one ``LumenNode``. It
mirrors the Rust SDK's ``lumen::dom::Node`` and the host-neutral surface
every script host and the C-ABI bind (design 4.1-4.8): query and traverse
the tree, read and write attributes / classes / text / inline style, build
and rearrange nodes, inspect post-layout geometry and computed style, and
bind event handlers.

The layer stays thin -- every method maps onto one C export (the single-key
``attr`` / ``style`` convenience readers fold a map-returning call). Reads
are soft: a stale handle reads back ``None`` / ``False`` / an empty
container rather than raising, matching the signal getters. Mutations are
fire-and-forget: each queues on the command bus the app drains once per
tick, so a ``spawn`` plus its chained edits materialize together on the next
tick; the chainable setters return the node and swallow a queue error. Read
a value back after the app has ticked.

Usage::

    import lumen
    from lumen import dom

    row = dom.spawn("row").set_text("hello").add_class("item")
    dom.get_by_id("list").append(row)

    @dom.get_by_id("bump").on("click")
    def _(ev):
        print("clicked", ev.target().handle)
"""

from __future__ import annotations

import ctypes
from typing import Callable, NamedTuple, Optional

from . import _ffi
from ._lib import get_library

__all__ = [
    "Node",
    "Event",
    "Listener",
    "Rect",
    "Scroll",
    "PointerState",
    "FrameInfo",
    "query",
    "query_single",
    "get_by_id",
    "document",
    "spawn",
    "dump_tree",
    "signals_all",
    "pointer_state",
    "frame_info",
]

_OK = _ffi.LumenStatus.OK


class Rect(NamedTuple):
    """Post-layout box. ``x`` / ``y`` are local to the parent origin;
    ``client_*`` are window coordinates."""

    x: float
    y: float
    width: float
    height: float
    client_x: float
    client_y: float


class Scroll(NamedTuple):
    """Scroll offsets and their travel limits for a scroll container."""

    x: float
    y: float
    max_x: float
    max_y: float


class PointerState(NamedTuple):
    """Pointer state snapshot (``pointer_state``)."""

    x: float
    y: float
    inside: bool
    buttons: int
    shift: bool
    ctrl: bool
    alt: bool
    super: bool


class FrameInfo(NamedTuple):
    """Per-frame counters (``frame_info``)."""

    frame: int
    dt_ms: float
    dirty_count: int


# ---- owned-buffer readers -------------------------------------------


def _take_owned_string(lib, ptr: ctypes.c_char_p) -> str:
    """Adopt an owned C string (a ``char**``-out result) into ``str``,
    releasing it with ``lumen_string_free``."""

    if not ptr:
        return ""
    try:
        return ptr.value.decode("utf-8", errors="replace") if ptr.value else ""
    finally:
        lib.lumen_string_free(ptr)


def _read_kvlist(lib, cfunc, *lead) -> dict[str, str]:
    """Read a ``LumenKVList``-returning getter into a dict, freeing the C
    buffer. Empty on a non-OK status (a stale handle reads empty)."""

    out = _ffi.LumenKVList()
    if cfunc(*lead, ctypes.byref(out)) != _OK:
        return {}
    result: dict[str, str] = {}
    for i in range(out.len):
        kv = out.ptr[i]
        key = kv.key.decode("utf-8", errors="replace") if kv.key else ""
        val = kv.value.decode("utf-8", errors="replace") if kv.value else ""
        result[key] = val
    lib.lumen_kvlist_free(out)
    return result


def _read_strlist(lib, cfunc, *lead) -> list[str]:
    """Read a ``LumenStrList``-returning getter into a list, freeing the C
    buffer. Empty on a non-OK status."""

    out = _ffi.LumenStrList()
    if cfunc(*lead, ctypes.byref(out)) != _OK:
        return []
    result = [
        out.ptr[i].decode("utf-8", errors="replace") if out.ptr[i] else ""
        for i in range(out.len)
    ]
    lib.lumen_strlist_free(out)
    return result


def _read_nodelist(lib, out: "_ffi.LumenNodeList") -> list["Node"]:
    """Adopt a ``LumenNodeList`` into a list of nodes, freeing the C
    buffer."""

    result = [Node(out.ptr[i]) for i in range(out.len)]
    lib.lumen_nodelist_free(out)
    return result


def _read_string_out(cfunc, *lead) -> str:
    """Two-call size-then-fill helper for the ``(buf, len, out_len)``
    string-out convention (event ``type`` / ``key`` / ``value``)."""

    out_len = ctypes.c_size_t(0)
    status = cfunc(*lead, None, 0, ctypes.byref(out_len))
    if status == _ffi.LumenStatus.ERR_BUFFER_TOO_SMALL:
        buf = ctypes.create_string_buffer(out_len.value)
        if cfunc(*lead, buf, out_len.value, ctypes.byref(out_len)) == _OK:
            return buf.value.decode("utf-8", errors="replace")
    return ""


# ---- event listener anchoring ---------------------------------------
#
# lumen_on stores no owning reference to the ctypes callback; if it were
# garbage-collected the Rust side would call a freed trampoline. Anchor
# each callback (keyed by off token) for the program's duration, dropping
# it on Listener.off. Mirrors the C++ SDK's event_anchors.

_event_anchors: dict[int, object] = {}


class Node:
    """A live element handle. Cheap to copy; addresses one node by packed
    handle. ``Node(0)`` is the invalid handle."""

    __slots__ = ("_h",)

    def __init__(self, handle: int) -> None:
        self._h = int(handle)

    @property
    def handle(self) -> int:
        """The raw packed handle (index + generation)."""

        return self._h

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Node) and other._h == self._h

    def __hash__(self) -> int:
        return hash(self._h)

    def __repr__(self) -> str:
        return f"Node(0x{self._h:x})"

    def valid(self) -> bool:
        """Whether this handle still names a live node."""

        lib = get_library()
        out = ctypes.c_int(0)
        return lib.lumen_node_valid(self._h, ctypes.byref(out)) == _OK and out.value != 0

    # -- static query entry points --

    @staticmethod
    def query(selector: str) -> list["Node"]:
        return query(selector)

    @staticmethod
    def get_by_id(id: str) -> Optional["Node"]:
        return get_by_id(id)

    # -- traversal (design 4.1, 4.2) --

    def parent(self) -> Optional["Node"]:
        return self._out_node("lumen_node_parent")

    def first_child(self) -> Optional["Node"]:
        return self._out_node("lumen_node_first_child")

    def last_child(self) -> Optional["Node"]:
        return self._out_node("lumen_node_last_child")

    def next(self) -> Optional["Node"]:
        return self._out_node("lumen_node_next")

    def prev(self) -> Optional["Node"]:
        return self._out_node("lumen_node_prev")

    def children(self) -> list["Node"]:
        lib = get_library()
        out = _ffi.LumenNodeList()
        if lib.lumen_node_children(self._h, ctypes.byref(out)) != _OK:
            return []
        return _read_nodelist(lib, out)

    def closest(self, selector: str) -> Optional["Node"]:
        lib = get_library()
        out = ctypes.c_uint64(0)
        if (
            lib.lumen_node_closest(self._h, selector.encode("utf-8"), ctypes.byref(out))
            == _OK
            and out.value != 0
        ):
            return Node(out.value)
        return None

    # -- attributes / class / text (design 4.4) --

    def attrs(self) -> dict[str, str]:
        lib = get_library()
        return _read_kvlist(lib, lib.lumen_node_attrs, self._h)

    def attr(self, name: str) -> Optional[str]:
        """Read one attribute (convenience over :meth:`attrs`)."""

        return self.attrs().get(name)

    def set_attr(self, name: str, value: str) -> "Node":
        get_library().lumen_node_set_attr(
            self._h, name.encode("utf-8"), value.encode("utf-8")
        )
        return self

    def remove_attr(self, name: str) -> "Node":
        get_library().lumen_node_remove_attr(self._h, name.encode("utf-8"))
        return self

    def set_text(self, text: str) -> "Node":
        get_library().lumen_node_set_text(self._h, text.encode("utf-8"))
        return self

    def classes(self) -> list[str]:
        lib = get_library()
        return _read_strlist(lib, lib.lumen_node_classes, self._h)

    def add_class(self, cls: str) -> "Node":
        get_library().lumen_node_class_add(self._h, cls.encode("utf-8"))
        return self

    def remove_class(self, cls: str) -> "Node":
        get_library().lumen_node_class_remove(self._h, cls.encode("utf-8"))
        return self

    def toggle_class(self, cls: str) -> "Node":
        get_library().lumen_node_class_toggle(self._h, cls.encode("utf-8"))
        return self

    # -- markup (design 4.4, phase 6) --

    def inner_markup(self) -> str:
        """Serialize this node's children to ``.lmn``-ish text (``innerHTML``
        read)."""

        lib = get_library()
        out = ctypes.c_char_p()
        lib.lumen_node_inner_markup(self._h, ctypes.byref(out))
        return _take_owned_string(lib, out)

    def outer_markup(self) -> str:
        """Serialize this subtree to ``.lmn``-ish text (``outerHTML`` read)."""

        lib = get_library()
        out = ctypes.c_char_p()
        lib.lumen_node_outer_markup(self._h, ctypes.byref(out))
        return _take_owned_string(lib, out)

    def set_inner_markup(self, markup: str) -> "Node":
        """Replace this node's children with the subtree parsed from
        ``markup`` (``innerHTML`` write, chainable).

        Guarded: parsing needs the injected markup front-end, present on the
        from-source run path and a no-op on the precompiled-artifact path. Do
        NOT feed untrusted content -- this injects live markup
        (XSS-adjacent).
        """

        get_library().lumen_node_set_inner_markup(self._h, markup.encode("utf-8"))
        return self

    # -- inline style (design 4.5) --

    def inline_style(self) -> dict[str, str]:
        lib = get_library()
        return _read_kvlist(lib, lib.lumen_node_inline_style, self._h)

    def style(self, name: str) -> Optional[str]:
        """Read one inline style property (convenience over
        :meth:`inline_style`)."""

        return self.inline_style().get(name)

    def set_style(self, name: str, value: str) -> "Node":
        get_library().lumen_node_set_style(
            self._h, name.encode("utf-8"), value.encode("utf-8")
        )
        return self

    def remove_style(self, name: str) -> "Node":
        get_library().lumen_node_remove_style(self._h, name.encode("utf-8"))
        return self

    def computed_style(self) -> dict[str, str]:
        lib = get_library()
        return _read_kvlist(lib, lib.lumen_node_computed_style, self._h)

    # -- structure (design 4.3) --

    def append(self, child: "Node") -> "Node":
        get_library().lumen_node_append(self._h, child._h)
        return self

    def insert_before(self, child: "Node", reference: "Node") -> "Node":
        get_library().lumen_node_insert_before(self._h, child._h, reference._h)
        return self

    def set_parent(self, parent: "Node") -> "Node":
        get_library().lumen_node_set_parent(self._h, parent._h)
        return self

    def replace_with(self, other: "Node") -> "Node":
        """Replace this node with ``other`` in the parent, despawning this
        subtree. Returns ``other``."""

        get_library().lumen_node_replace_with(self._h, other._h)
        return other

    def remove(self) -> None:
        """Detach and despawn this node and its subtree (``remove``)."""

        get_library().lumen_node_remove(self._h)

    def clone_deep(self) -> "Node":
        """Deep-clone this subtree into a fresh detached node
        (``cloneNode(true)``)."""

        lib = get_library()
        out = ctypes.c_uint64(0)
        lib.lumen_node_clone(self._h, ctypes.byref(out))
        return Node(out.value)

    # -- introspection (design 4.7) --

    def rect(self) -> Optional[Rect]:
        return self._out_rect("lumen_node_rect")

    def content_rect(self) -> Optional[Rect]:
        return self._out_rect("lumen_node_content_rect")

    def scroll(self) -> Optional[Scroll]:
        lib = get_library()
        s = _ffi.LumenScroll()
        if lib.lumen_node_scroll(self._h, ctypes.byref(s)) != _OK:
            return None
        return Scroll(s.x, s.y, s.max_x, s.max_y)

    def is_visible(self) -> bool:
        lib = get_library()
        out = ctypes.c_int(0)
        return (
            lib.lumen_node_is_visible(self._h, ctypes.byref(out)) == _OK
            and out.value != 0
        )

    def z_index(self) -> int:
        lib = get_library()
        out = ctypes.c_int(0)
        lib.lumen_node_z_index(self._h, ctypes.byref(out))
        return out.value

    def entity_id(self) -> Optional[tuple[int, int]]:
        """The raw ``(index, generation)`` for debugging / handle
        round-trip."""

        lib = get_library()
        index = ctypes.c_uint32(0)
        gen = ctypes.c_uint32(0)
        if (
            lib.lumen_node_entity_id(self._h, ctypes.byref(index), ctypes.byref(gen))
            != _OK
        ):
            return None
        return (index.value, gen.value)

    def components(self) -> list[str]:
        lib = get_library()
        return _read_strlist(lib, lib.lumen_node_components, self._h)

    def component(self, name: str) -> dict[str, str]:
        """One component's public fields as a dict. Empty when the component
        is absent or not whitelisted."""

        lib = get_library()
        return _read_kvlist(lib, lib.lumen_node_component, self._h, name.encode("utf-8"))

    # -- events (design 4.6) --

    def on(self, event_type: str, handler: Callable[["Event"], None]) -> "Listener":
        """Bind ``handler`` for ``event_type`` (bubble / target phase).
        Returns a :class:`Listener`; call :meth:`Listener.off` to unbind. The
        callback is anchored for the program's duration (until ``off``)."""

        return self._bind(event_type, False, handler)

    def on_capture(
        self, event_type: str, handler: Callable[["Event"], None]
    ) -> "Listener":
        """Bind a capture-phase listener."""

        return self._bind(event_type, True, handler)

    # -- internals --

    def _out_node(self, fname: str) -> Optional["Node"]:
        lib = get_library()
        out = ctypes.c_uint64(0)
        if getattr(lib, fname)(self._h, ctypes.byref(out)) == _OK and out.value != 0:
            return Node(out.value)
        return None

    def _out_rect(self, fname: str) -> Optional[Rect]:
        lib = get_library()
        r = _ffi.LumenRect()
        if getattr(lib, fname)(self._h, ctypes.byref(r)) != _OK:
            return None
        return Rect(r.x, r.y, r.width, r.height, r.client_x, r.client_y)

    def _bind(
        self, event_type: str, capture: bool, handler: Callable[["Event"], None]
    ) -> "Listener":
        lib = get_library()

        def _dispatch(event_ptr) -> None:
            handler(Event(event_ptr))

        cb = _ffi.make_event_callback(_dispatch)
        token = lib.lumen_on(
            self._h,
            event_type.encode("utf-8"),
            1 if capture else 0,
            ctypes.cast(cb, ctypes.c_void_p),
            None,
        )
        if token != 0:
            _event_anchors[token] = cb
        return Listener(token)


class Listener:
    """A bound event listener. Call :meth:`off` to unbind
    (``removeEventListener``). Dropping a ``Listener`` does NOT unbind -- the
    handler stays anchored until ``off``."""

    __slots__ = ("_token",)

    def __init__(self, token: int) -> None:
        self._token = int(token)

    @property
    def token(self) -> int:
        """The raw off token."""

        return self._token

    def off(self) -> None:
        """Unbind the listener and drop its anchored callback."""

        if self._token == 0:
            return
        get_library().lumen_off(self._token)
        _event_anchors.pop(self._token, None)
        self._token = 0


class Event:
    """The event passed to a :meth:`Node.on` handler. Wraps the borrowed
    ``LumenEvent`` snapshot; valid only for the duration of the handler
    call. Scalar fields read the snapshot directly; the string fields
    (``type`` / ``key`` / ``value``) fetch through the current-event C
    getters."""

    __slots__ = ("_ptr",)

    def __init__(self, event_ptr) -> None:
        self._ptr = event_ptr

    @property
    def _ev(self):
        return self._ptr[0] if self._ptr else None

    def target(self) -> Node:
        """The node the event was dispatched to."""

        ev = self._ev
        return Node(ev.target if ev else get_library().lumen_event_target())

    def current_target(self) -> Node:
        """The node whose listener is currently running."""

        ev = self._ev
        return Node(
            ev.current_target if ev else get_library().lumen_event_current_target()
        )

    def type(self) -> str:
        """The event type (``"click"``, ``"keydown"``, ...)."""

        return _read_string_out(get_library().lumen_event_type)

    def key(self) -> str:
        """The key for key events."""

        return _read_string_out(get_library().lumen_event_key)

    def value(self) -> str:
        """The value for input / change events."""

        return _read_string_out(get_library().lumen_event_value)

    def position(self) -> tuple[float, float]:
        """Pointer position local to the target ``(x, y)``."""

        ev = self._ev
        return (ev.local_x, ev.local_y) if ev else (0.0, 0.0)

    def client_position(self) -> tuple[float, float]:
        """Pointer position in window coordinates ``(x, y)``."""

        ev = self._ev
        return (ev.client_x, ev.client_y) if ev else (0.0, 0.0)

    def delta(self) -> tuple[float, float]:
        """Wheel delta ``(dx, dy)``."""

        ev = self._ev
        return (ev.delta_x, ev.delta_y) if ev else (0.0, 0.0)

    def button(self) -> int:
        """The button for pointer events (0 primary, 1 middle, 2 secondary,
        -1 none)."""

        ev = self._ev
        return ev.button if ev else -1

    def modifiers(self) -> tuple[bool, bool, bool, bool]:
        """Modifier state ``(shift, ctrl, alt, super)``."""

        ev = self._ev
        if not ev:
            return (False, False, False, False)
        return (bool(ev.shift), bool(ev.ctrl), bool(ev.alt), bool(ev.super_))

    def prevent_default(self) -> None:
        """Cancel the event's default action."""

        get_library().lumen_event_prevent_default()

    def stop_propagation(self) -> None:
        """Stop propagation to the next node."""

        get_library().lumen_event_stop_propagation()

    def stop_immediate_propagation(self) -> None:
        """Stop the remaining handlers everywhere."""

        get_library().lumen_event_stop_immediate_propagation()


# ---- free entry points (design 4.1, 4.7) ----------------------------


def query(selector: str) -> list[Node]:
    """Run a CSS selector query over the whole tree."""

    lib = get_library()
    out = _ffi.LumenNodeList()
    if lib.lumen_query(selector.encode("utf-8"), ctypes.byref(out)) != _OK:
        return []
    return _read_nodelist(lib, out)


def query_single(selector: str) -> Optional[Node]:
    """The single match, or ``None`` for zero / many."""

    lib = get_library()
    out = ctypes.c_uint64(0)
    if (
        lib.lumen_query_single(selector.encode("utf-8"), ctypes.byref(out)) == _OK
        and out.value != 0
    ):
        return Node(out.value)
    return None


def get_by_id(id: str) -> Optional[Node]:
    """Fast id lookup (``getElementById``)."""

    lib = get_library()
    out = ctypes.c_uint64(0)
    if (
        lib.lumen_get_by_id(id.encode("utf-8"), ctypes.byref(out)) == _OK
        and out.value != 0
    ):
        return Node(out.value)
    return None


def document() -> Optional[Node]:
    """The root element (``document.documentElement``)."""

    lib = get_library()
    out = ctypes.c_uint64(0)
    if lib.lumen_document(ctypes.byref(out)) == _OK and out.value != 0:
        return Node(out.value)
    return None


def spawn(tag: str) -> Node:
    """Create a fresh detached element with markup ``tag``
    (``createElement``). Attach it with :meth:`Node.append` /
    :meth:`Node.set_parent`."""

    lib = get_library()
    out = ctypes.c_uint64(0)
    lib.lumen_node_spawn(tag.encode("utf-8"), ctypes.byref(out))
    return Node(out.value)


def dump_tree() -> str:
    """Whole-tree structural dump (id / tag / classes / rect). An inspection
    call."""

    lib = get_library()
    out = ctypes.c_char_p()
    lib.lumen_dump_tree(ctypes.byref(out))
    return _take_owned_string(lib, out)


def signals_all() -> dict[str, str]:
    """The whole signal set as ``name -> value`` pairs. An inspection
    call."""

    lib = get_library()
    return _read_kvlist(lib, lib.lumen_signals_all)


def pointer_state() -> Optional[PointerState]:
    """Current pointer state snapshot."""

    lib = get_library()
    s = _ffi.LumenPointerState()
    if lib.lumen_pointer_state(ctypes.byref(s)) != _OK:
        return None
    return PointerState(
        s.x, s.y, bool(s.inside), s.buttons, bool(s.shift), bool(s.ctrl), bool(s.alt),
        bool(s.super_),
    )


def frame_info() -> Optional[FrameInfo]:
    """Current per-frame counters."""

    lib = get_library()
    f = _ffi.LumenFrameInfo()
    if lib.lumen_frame_info(ctypes.byref(f)) != _OK:
        return None
    return FrameInfo(f.frame, f.dt_ms, f.dirty_count)
