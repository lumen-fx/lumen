# Lumen Python SDK

Effortless, typed Python bindings for the [Lumen](../../) UI framework,
over the C ABI (`lumen/ffi/include/lumen.h`). State is declarative and
reactive: you write a `Model` whose fields are typed signals, mutate them
with plain Python, and the runtime keeps the UI in sync.

Stdlib only - `ctypes`, no `cffi`, no compiled extension, no build step of
its own. The only thing to build is the Rust side, once.

## Quickstart

```python
import lumen

class Counter(lumen.Model):        # dataclass-style: fields ARE signals
    count: int = 0
    label: str = "0 clicks"

app = lumen.App("counter_app", title="Counter")
state = Counter(app)

@app.on_click("bump")              # decorator; handler takes no args
def bump():
    state.count += 1               # typed, autocompleted, syncs to the runtime
    state.label = f"{state.count} clicks"

app.run()                          # blocks until the window closes
```

Handlers may take the clicked `element_id` string **or no argument** -
`def bump():` and `def bump(element_id):` both work (mirrors the C++
SDK's `void()` / `void(std::string)` overloads). You can also register
without the decorator: `app.on_click("bump", bump)`.

`state.count` and `state.label` *are* signals: reading a field reads the
signal, writing it pushes the new value to the runtime, and markup bound
with `bind-text="label"` reflects it next tick. Field names are the
binding names; override with `Field(name="other")`.

## The surface, most to least abstract

| You want... | Reach for |
|---|---|
| Declarative reactive state | `lumen.Model` + `lumen.Field` |
| A single typed signal | `lumen.Signal[T]` |
| A derived value | `lumen.computed(name, fn, *deps)` |
| To react to a change | `signal.watch(fn)` |
| To query / mutate the live element tree | `lumen.dom` |
| A raw `lumen_signal_*` C call | `lumen.raw` (appendix) |

### Dynamic DOM (`lumen.dom`)

`lumen.dom` reaches the live element tree the way a browser script does:
query and traverse nodes, read and write attributes / classes / text /
inline style, build and rearrange subtrees, read post-layout geometry, and
bind event handlers. A `dom.Node` is a cheap handle; reads return `None` /
empty for a stale handle and mutations chain.

```python
from lumen import dom

row = dom.spawn("row").add_class("item").set_text("hello")
list_node = dom.get_by_id("list")
if list_node:
    list_node.append(row)

row.on("click", lambda e: print("clicked", e.target().handle, e.type()))
```

### `Model` / `Field`

```python
class TodoList(lumen.Model):
    items: list = lumen.Field(default_factory=list)   # array signal (<for>)
    summary: str = "0 items"

state = TodoList(app)
state.items = state.items + [{"id": "t1", "text": "Buy milk"}]  # re-renders <for>
state.summary = f"{len(state.items)} items"
```

- Annotated fields become typed signals (`int`->int64, `float`->float64,
  `bool`, `str`, `lumen.Color`, `list`->array).
- Defaults are applied on construction; use `Field(default_factory=...)`
  for mutable defaults (lists).
- Signals are global by name, so one `Model` instance per app is the
  intended shape (the field names are the binding names markup reads).
- `state.signal("count")` returns the underlying `Signal` handle - for
  `.watch()` or to pass into `computed()`.

### `Signal[T]` - standalone typed handles

```python
count = lumen.Signal("count", 0)          # kind inferred from the initial value
count += 1                                 # typed operators where they make sense
count.value = 10                           # .value / .get() / .set()
tint = lumen.Signal("tint", lumen.Color(255, 128, 0))

@count.watch                               # decorator or count.watch(fn)
def _(new_value):
    print("count is now", new_value)
```

`Signal(name, initial=None, *, type=...)` - pass `type=` when there is no
initial value, or to widen an `int` literal to `float`.

### `watch` and `computed`

`signal.watch(fn)` is a **real ABI subscription** (`lumen_signal_watch`):
`fn(new_value)` fires on the Lumen tick thread each time the value
commits - not a polling loop. A freshly-registered watch also fires once
with the current value on the first tick it is observed. Watches only fire
**while an app is running** (`run` / `run_headless`), because that is when
ticks happen.

`computed(name, fn, *deps)` derives a signal from `fn()`, recomputing
whenever any dependency commits. Direct or decorator form:

```python
count = lumen.Signal("count", 0)

# direct
lumen.computed("label", lambda: f"{count.value} clicks", count)

# decorator (deps first, then decorate the recompute fn)
@lumen.computed("label", count)
def label():
    return f"{count.value} clicks"
```

### Colors

`lumen.Color` is an RGBA tuple with named channels and CSS-style hex:

```python
lumen.Color(255, 128, 0)            # r, g, b (, a=255)
lumen.Color("#ff8000")             # hex string (also #f80 / #ff8000ff)
lumen.Color.from_hex("#ff8000ff")  # explicit
c = lumen.Color("#ff8000"); c.r, c.g, c.b, c.a
c.to_hex()                          # "#ff8000ff"
```

### Typing

The package ships a PEP 561 `py.typed` marker, so once installed the
inline annotations are visible to `mypy` / Pyright and drive IDE
completion for `Model` fields, `Signal[T]`, and the `App` methods.

## Requirements

- Python 3.9+ (stdlib `ctypes` only).
- The Lumen C library, `liblumen_ffi`, built as a `cdylib`.
- `App.run()` opens a real OS window/GPU surface. For CI or a no-display
  environment, `App.run_headless(ticks)` drives the full app (scripts,
  bindings, `<for>`/`<if>` reconciliation, **and `watch` firing**) for a
  fixed number of ticks with no window.

## Build the C library

From the Lumen workspace root:

```sh
cargo build -p lumen-ffi            # -> target/debug/liblumen_ffi.{so,dylib,dll}
cargo build -p lumen-ffi --release  # -> target/release/...
```

## Run the examples

```sh
LUMEN_LIBRARY_PATH=target/debug python sdk/python/examples/counter.py
LUMEN_LIBRARY_PATH=target/debug python sdk/python/examples/todo.py
```

Both scripts add their parent directory to `sys.path`, so they run
straight from a checkout - no `pip install` required. `counter.py` is the
quickstart above; `todo.py` exercises a `Model` with a `list` field
driving a `<for>` block (array signals).

## Installing the package

```sh
pip install -e sdk/python
```

Registers the `lumen` package (distribution name `lumen-ui`). It does
**not** build or bundle `liblumen_ffi` - you still need
`cargo build -p lumen-ffi` and a way for the loader to find it.

## Locating the library

`load_library()` (called on first use) searches, in order:

1. `LUMEN_LIBRARY_PATH` - a direct path to the library file, or a
   directory containing it.
2. `target/{debug,release}` relative to the current working directory.
3. `target/{debug,release}` relative to the workspace root (found by
   walking up for the workspace `Cargo.toml`).
4. The system loader's own search paths (`LD_LIBRARY_PATH`, `/usr/lib`, ...).

The loaded library's `lumen_abi_version()` is checked against the version
this SDK targets (ABI **0.4**); a major mismatch or an older minor raises
`LumenAbiVersionError`.

## Threading

`@app.on_click` handlers and `watch` callbacks fire on **Lumen's tick
thread**, not necessarily the thread that called `run`. `ctypes` acquires
the GIL for you, so touching Python state is safe - but a slow or blocking
handler stalls the whole event loop (rendering, input, timers). Keep
handlers fast; hand real work to a Python thread you spawn yourself.

State you mutate from your own threads is still your responsibility to
synchronise. `Signal` set/get themselves are thread-safe (each maps to one
thread-safe C call).

## Errors

Every `lumen_*` call maps a non-OK `LumenStatus` onto a dedicated
exception under `lumen.LumenError` (`LumenBadPathError`, `LumenCssError`,
`LumenScriptError`, ...), so you can `except LumenCssError` instead of
string-matching. `LumenLibraryNotFoundError` and `LumenAbiVersionError`
cover the two pre-call failures.

## Appendix - the raw layer (`lumen.raw`)

`lumen.raw.Signal` is the thin, stringly surface every abstraction above is
built on: each method is exactly one `lumen_signal_*` C call. Reach for it
only when you need the raw ABI - e.g. the array setter, or the stringly
text setters `bind-text` reads directly.

```python
from lumen import raw

raw.Signal.set_int64("count", 5)          # typed scalar setters/getters
raw.Signal.get_int64("count")             # -> 5
raw.Signal.set_string("label", "hi")      # stringly text signal
raw.Signal.set_array("rows", [{"id": "a", "name": "x"}])   # <for> array
raw.Signal.get_string("label")            # read back what the FFI last pushed
```

Read-back caveat: `get_string` / `array_len` / `array_field` return the
value the *embedder* last pushed through the FFI, not live in-app state (a
Rhai `signals.x.set(..)` or a two-way input binding is not visible here).
The typed layer above hides all of this.
