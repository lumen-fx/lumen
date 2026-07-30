"""Python SDK for the Lumen UI framework, built on ``ctypes`` over the
``lumen-ffi`` C ABI. Stdlib-only -- no compiled extension, no ``cffi``,
no build step of its own; it just needs ``liblumen_ffi`` built once
(``cargo build -p lumen-ffi``) and loadable at import time.

Typical usage::

    import lumen

    class Counter(lumen.Model):     # fields ARE typed signals
        count: int = 0
        label: str = "0 clicks"

    app = lumen.App("path/to/app", title="Counter")
    state = Counter(app)

    @app.on_click("bump")
    def bump(_):
        state.count += 1            # typed, autocompleted, syncs to runtime
        state.label = f"{state.count} clicks"

    app.run()                       # blocks until the window closes

The surface, from most to least abstract:

* :class:`Model` / :class:`Field` - declarative reactive state.
* :class:`Signal` (typed handle), :func:`computed`, :class:`Color`.
* :mod:`lumen.raw` - the thin stringly ``lumen_signal_*`` layer under all
  of the above; reach for it only when you need a raw C call.

See ``lumen.errors`` for the exception hierarchy every ``lumen_*`` call
can raise, and the package README for install/run instructions.
"""

from . import dom, raw
from .api import App, get_library
from .dom import Event, Listener, Node
from .model import Field, Model
from .signals import Color, Signal, computed
from .errors import (
    LumenAbiVersionError,
    LumenAssetError,
    LumenBadArgError,
    LumenBadPathError,
    LumenCssError,
    LumenError,
    LumenInternalError,
    LumenInvalidHandleError,
    LumenInvalidValueError,
    LumenIoError,
    LumenLibraryNotFoundError,
    LumenPanicError,
    LumenParseError,
    LumenRuntimeError,
    LumenScriptError,
    LumenWindowError,
)

__all__ = [
    "App",
    "Model",
    "Field",
    "Signal",
    "Color",
    "computed",
    "raw",
    "dom",
    "Node",
    "Event",
    "Listener",
    "get_library",
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
    "LumenLibraryNotFoundError",
    "LumenAbiVersionError",
]

__version__ = "0.1.0.dev0"
