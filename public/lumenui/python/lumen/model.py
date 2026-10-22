"""Declarative reactive state - dataclass-style models whose fields ARE
signals.

Subclass :class:`Model` and annotate fields; each becomes a typed
:class:`lumen.Signal` under the hood. Reading a field reads the signal;
writing it writes the signal (which syncs to the runtime and fires any
watchers). Full typing means IDEs autocomplete the fields.

    import lumen

    class Counter(lumen.Model):
        count: int = 0
        label: str = "0 clicks"

    app = lumen.App("counter_app")
    state = Counter(app)

    state.count += 1                      # typed, syncs to the runtime
    state.label = f"{state.count} clicks"

The signal a field binds to is the field name by default; override it with
``Field(name="other_name")`` when markup binds a different name. Mutable
defaults (lists) use ``Field(default_factory=list)``.

Signals are global by name, so one :class:`Model` instance per app is the
intended shape (the field names are the binding names markup reads).
"""

from __future__ import annotations

from typing import Any, Callable

from .signals import Signal

__all__ = ["Model", "Field"]

_MISSING = object()


class Field:
    """Per-field override for a :class:`Model` annotation.

    Args:
        default: Initial value (mutually exclusive with ``default_factory``).
        name: Signal name to bind to, when it differs from the field name.
        default_factory: Zero-arg callable producing the initial value -
            use for mutable defaults (``Field(default_factory=list)``).
        type: Explicit signal kind override (see :class:`lumen.Signal`).
    """

    __slots__ = ("default", "name", "default_factory", "type")

    def __init__(
        self,
        default: Any = _MISSING,
        *,
        name: str | None = None,
        default_factory: Callable[[], Any] | None = None,
        type: object = None,
    ) -> None:
        if default is not _MISSING and default_factory is not None:
            raise ValueError("Field: pass default OR default_factory, not both")
        self.default = default
        self.name = name
        self.default_factory = default_factory
        self.type = type


class _FieldDescriptor:
    """Descriptor mapping one annotated model field to a typed
    :class:`lumen.Signal`. Attribute read/write proxies to the signal, so
    ``model.count`` and ``model.count = 5`` are signal get/set.
    """

    __slots__ = ("field_name", "signal_name", "kind", "default", "default_factory", "signal")

    def __init__(
        self,
        signal_name: str | None,
        kind: object,
        default: Any,
        default_factory: Callable[[], Any] | None,
    ) -> None:
        self.field_name = ""  # filled by __set_name__
        self.signal_name = signal_name  # may be None -> defaults to field name
        self.kind = kind
        self.default = default
        self.default_factory = default_factory
        self.signal: Signal | None = None

    def __set_name__(self, owner: type, name: str) -> None:
        self.field_name = name
        if self.signal_name is None:
            self.signal_name = name
        # Construct the backing signal now (no runtime call until a value is
        # set); ``kind`` may be a type object or a string annotation.
        self.signal = Signal(self.signal_name, type=self.kind)

    def __get__(self, obj: object, objtype: type | None = None) -> Any:
        if obj is None:
            return self
        assert self.signal is not None
        return self.signal.get()

    def __set__(self, obj: object, value: Any) -> None:
        assert self.signal is not None
        self.signal.set(value)


class Model:
    """Base class for declarative reactive state. See the module docstring.

    Subclasses declare fields as annotations (with optional defaults or
    :class:`Field` overrides); :meth:`__init_subclass__` turns each into a
    :class:`_FieldDescriptor`. Constructing an instance applies the defaults
    (pushing them to the runtime).
    """

    __lumen_fields__: dict[str, _FieldDescriptor] = {}

    def __init_subclass__(cls, **kwargs: object) -> None:
        super().__init_subclass__(**kwargs)
        # Merge annotations across the MRO so subclasses inherit fields.
        annotations: dict[str, object] = {}
        for klass in reversed(cls.__mro__):
            annotations.update(getattr(klass, "__annotations__", {}))

        fields: dict[str, _FieldDescriptor] = {}
        for field_name, annotated_type in annotations.items():
            if field_name.startswith("_"):
                continue
            raw_default = cls.__dict__.get(field_name, _MISSING)
            if isinstance(raw_default, Field):
                signal_name = raw_default.name
                kind = raw_default.type if raw_default.type is not None else annotated_type
                default = raw_default.default
                default_factory = raw_default.default_factory
            else:
                signal_name = None
                kind = annotated_type
                default = raw_default
                default_factory = None

            desc = _FieldDescriptor(signal_name, kind, default, default_factory)
            setattr(cls, field_name, desc)
            # ``__set_name__`` only fires automatically for descriptors set
            # during class-body execution; a post-hoc ``setattr`` needs it
            # invoked by hand.
            desc.__set_name__(cls, field_name)
            fields[field_name] = desc

        cls.__lumen_fields__ = fields

    def __init__(self, app: object | None = None) -> None:
        # ``app`` is accepted for the idiomatic ``State(app)`` call and kept
        # for reference; signals are global by name, so no per-app wiring is
        # needed. Applying defaults pushes each field's initial value.
        self._app = app
        for desc in type(self).__lumen_fields__.values():
            if desc.default_factory is not None:
                setattr(self, desc.field_name, desc.default_factory())
            elif desc.default is not _MISSING:
                setattr(self, desc.field_name, desc.default)

    def signal(self, field_name: str) -> Signal:
        """Return the underlying :class:`lumen.Signal` for ``field_name`` -
        for when you need ``.watch()`` or to pass the handle to
        :func:`lumen.computed`."""

        desc = type(self).__lumen_fields__.get(field_name)
        if desc is None or desc.signal is None:
            raise AttributeError(f"{type(self).__name__} has no signal field {field_name!r}")
        return desc.signal
