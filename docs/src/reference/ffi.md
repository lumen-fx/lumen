# FFI and SDKs

The C ABI that drives a Lumen app from another language, and the C++, Python,
and Rust SDKs built on top of it.

## What embedding means

A normal Lumen app is a directory of markup, CSS, and a script, run by
`lumenc`. Embedding turns that around: your program owns `main`, builds the app
from a directory (or from a precompiled artifact), registers native callbacks,
and hands control to Lumen's event loop. Markup and CSS still describe the UI;
your language holds the state and the logic.

The seam is a C ABI exported by the `lumen` library. Two headers describe it,
both committed to the repository so there is no generation step:

- `include/lumen.h` - the header you include. It defines the status enum, the
  tagged `LumenValue` type, and the callback signatures.
- `include/lumen_simple.h` - generated from the Rust source, included by
  `lumen.h`. It declares the lifecycle, signal, DOM, and event entry points.

Build the library from the workspace root:

```sh
cargo build -p lumen             # target/debug/liblumen.{so,dylib,dll,a}
cargo build -p lumen --release
```

This builds the C library every C, C++, and Python caller opens. The Rust SDK
links a second library instead of opening this one; see
[the Rust SDK](#rust-sdk) for what that is and why they are separate.

The build also renders a `lumen.pc` file into the crate's output directory.
Install it where pkg-config looks and `pkg-config --cflags --libs lumen`
resolves the include path and `-llumen`.

## Conventions

**Return codes.** Every function returns `LumenStatus`, except constructors,
which return `NULL` on failure, and a few accessors that return a handle. The
codes:

| Constant | Value | Meaning |
| --- | --- | --- |
| `LUMEN_OK` | 0 | Success. |
| `LUMEN_ERR_BAD_PATH` | 1 | App directory missing or unusable. |
| `LUMEN_ERR_BAD_ARG` | 2 | Null, non-UTF-8, or out-of-range argument. |
| `LUMEN_ERR_RUNTIME` | 3 | Runtime failure. |
| `LUMEN_ERR_INTERNAL` | 4 | Internal failure. |
| `LUMEN_ERR_PARSE` | 5 | Markup parse failure. |
| `LUMEN_ERR_CSS` | 6 | CSS or selector parse failure. |
| `LUMEN_ERR_ASSET` | 7 | Asset load failure. |
| `LUMEN_ERR_WINDOW` | 8 | Windowing failure. |
| `LUMEN_ERR_SCRIPT` | 9 | Script failure. |
| `LUMEN_ERR_IO` | 10 | I/O failure. |
| `LUMEN_ERR_INVALID_HANDLE` | 11 | Handle does not resolve. |
| `LUMEN_ERR_INVALID_VALUE` | 12 | Value could not be converted. |
| `LUMEN_ERR_PANIC` | 13 | A Rust panic was caught at the boundary. |
| `LUMEN_ERR_BUFFER_TOO_SMALL` | 14 | Output buffer too small; `*out_len` holds the required capacity. |

`lumen_status_message(status)` returns a static description of a code. On a
non-OK return, `lumen_last_error()` returns a thread-local message; when that
slot is empty it falls back to the most recent error from any thread.
`lumen_last_error_global()` reads the global slot without the fallback. Both
pointers stay valid until the next `lumen_*` call that records an error.

**Threading.** Every `lumen_*` function is safe to call from any thread.
Callbacks fire on Lumen's tick thread, which is generally not the thread that
built the app. Guard any shared state they touch, and keep them fast: a
blocking callback stalls rendering, input, and timers.

**Ownership.** Pointers passed into a callback are borrowed for the duration of
the call; Lumen copies immediately, so you may free your buffers as soon as the
call returns. Lumen reads the value a callback writes through its out-pointer
once the callback returns, so pointers that value carries must outlive the call
rather than point into the callback's own frame. Lists and strings Lumen returns
are owned by you and released with the matching free function.

**`user_data`.** The opaque pointer you register is stored verbatim and handed
back to your callback. Lumen never dereferences it. Keeping the referent alive,
and safe to read from the tick thread, is yours to do.

**ABI version.** `LUMEN_API_VERSION` is the header's view, packed as
`(major << 16) | (minor << 8) | patch`. `lumen_abi_version()` returns the
loaded library's view. Compare at startup and refuse a mismatch in the
major-minor pair.

**String-out convention.** Functions that copy a string into a caller buffer
take `(char *buf, size_t buf_len, size_t *out_len)`. On success the bytes are
written with a trailing NUL and `*out_len` is the length excluding it. When
`buf` is null or too small, nothing is written, `*out_len` is set to the
required capacity, and `LUMEN_ERR_BUFFER_TOO_SMALL` is returned: call once to
size, then again to fill.

## App lifecycle

| Function | Behaviour |
| --- | --- |
| `LumenApp *lumen_app_new(const char *dir)` | Build an app rooted at `dir`, which must exist and contain `lumen.toml` and/or `src/main.lmn`. Returns null on error. |
| `LumenApp *lumen_app_new_from_lmna(const uint8_t *data, size_t len, const char *base_dir)` | Build an app from precompiled artifact bytes, with no parser involved. The bytes are copied immediately. `base_dir` is what relative asset paths resolve against; null means the current directory. |
| `LumenStatus lumen_app_set_title(LumenApp *app, const char *title)` | Override the window title. |
| `LumenStatus lumen_app_set_size(LumenApp *app, uint32_t w, uint32_t h)` | Override the initial window size, in logical pixels. |
| `LumenStatus lumen_app_run(LumenApp *app)` | Consume the handle and enter the event loop. Blocks until the window closes; the handle is freed on return. |
| `LumenStatus lumen_app_run_headless(LumenApp *app, uint32_t ticks)` | Consume the handle and drive `ticks` ticks with no window or GPU surface. Scripts, bindings, reconciliation, and typed-property draining all run. `ticks = 0` builds and drops, validating that the app loads. |
| `void lumen_app_free(LumenApp *app)` | Drop a handle without running. Safe on null. Do not call after `run` or `run_headless`. |
| `uint32_t lumen_abi_version(void)` | The library's packed ABI version. |

## Native callbacks

| Function | Behaviour |
| --- | --- |
| `LumenStatus lumen_app_expose(LumenApp *app, const char *name, uint32_t arg_count, LumenFn func, void *user_data)` | Expose a native function to the app's script under `name`, with the given arity. |
| `LumenStatus lumen_app_on_click(LumenApp *app, const char *id, LumenClickFn cb, void *user_data)` | Route clicks on the element with that `id` to a native callback. A second registration for the same id replaces the first. Register before `lumen_app_run`. Not delivered under `lumen_app_run_headless`, which injects no input. |
| `LumenStatus lumen_app_on_close(LumenApp *app, LumenCloseFn cb, void *user_data)` | Register a close hook. It fires on an OS close request before teardown; return non-zero to allow the close, `0` to veto. A second registration replaces the first. On Unix a second SIGINT or SIGTERM bypasses the hook. Does not fire under `lumen_app_run_headless`. |
| `LumenStatus lumen_signal_watch(const char *name, LumenWatchFn cb, void *user_data)` | Subscribe to changes of a global signal. Fires once per tick in which the value commits, plus once with the current value on the first tick after registration. Registration is global, independent of any app handle, and additive: a second watch on the same name adds a second watcher. Fires only while the app runs. |

Callback types:

```c
typedef void (*LumenFn)(LumenValue *out, int argc, const LumenValue *argv, void *user_data);
typedef void (*LumenClickFn)(const char *id, void *user_data);
typedef void (*LumenWatchFn)(const char *name, const LumenValue *value, void *user_data);
typedef int  (*LumenCloseFn)(void *user_data);
typedef void (*LumenEventFn)(const LumenEvent *event, void *user_data);
```

`lumen_app_expose` registers into every script host the app runs, so an exposed
function is callable whatever language the app is written in. Arguments and
results cross as `LumenValue`.

A `LumenFn` writes its result through the `out` pointer it receives first,
rather than returning a `LumenValue`. `out` is never null and points to a
writable value Lumen has pre-set to `LUMEN_NIL`, so a callback with nothing to
return may leave it alone. The out-parameter is what keeps the callback
expressible from bindings that cannot form an aggregate (`sret`) return, such
as ctypes and libffi.

Rhai and Lua scripts call an exposed `now_ms` as a plain global, `now_ms()`.
Rhai dispatches on the declared `arg_count`; Lua binds the call variadically, so
the count is not enforced there.

A candela script reaches the same function under the `native` namespace:

```rust
fn on_start() {
    let t = native::now_ms();
}
```

candela resolves a host call through a `host "native" { .. }` block, and the host
writes that block from what was exposed, so the script declares nothing. A script
that carries its own block keeps it, and one compiled ahead of time to a `.cdlb`
needs it: that build sees the script alone. See
[candela scripting](scripting-candela.md#native-functions-from-the-embedder).

Array and map arguments reach the callback as `LUMEN_NIL`; scalars and strings
cross in full. The value a callback writes carries every kind, arrays and maps
included.

## Values

`LumenValue` is a tagged union: a `LumenKind` discriminant plus a payload.

| `LumenKind` | Payload field | Type |
| --- | --- | --- |
| `LUMEN_NIL` | | none |
| `LUMEN_BOOL` | `as_.boolean` | `int`, `0` is false |
| `LUMEN_INT` | `as_.integer` | `int64_t` |
| `LUMEN_FLOAT` | `as_.float_` | `double` |
| `LUMEN_STRING` | `as_.string` | `const char *`, UTF-8, NUL-terminated |
| `LUMEN_ARRAY` | `as_.array` | `LumenArrayView { const LumenValue *items; size_t len; }` |
| `LUMEN_MAP` | `as_.map` | `LumenMapView { const LumenMapEntry *entries; size_t len; }` |

`LumenMapEntry` is `{ const char *key; LumenValue value; }`. Both views are
borrowed.

## Signals

Scalar signals are one typed family: a set/get pair per type, keyed by signal
name. Each setter stores the value typed, and each getter reads it back without
a string round-trip.

- `lumen_signal_set_str(const char *name, const char *value)` /
  `lumen_signal_get_str(const char *name, char *buf, size_t buf_len, size_t *out_len)`
- `lumen_signal_set_int64` / `lumen_signal_get_int64`
- `lumen_signal_set_float64` / `lumen_signal_get_float64`
- `lumen_signal_set_bool` / `lumen_signal_get_bool`
- `lumen_signal_set_color` / `lumen_signal_get_color` - RGBA bytes, four
  channels of `0` to `255`
- `lumen_signal_clear(const char *name)` - empty string, or empty array

A getter returns `LUMEN_ERR_BAD_ARG` when the signal holds no value of that
type. Markup bound with `bind-text` observes any of these writes: the binding
pass stringifies scalar cells on read.

Array signals are a separate family, because a row is a record rather than a
scalar and the property store has no array cell:

| Function | Behaviour |
| --- | --- |
| `lumen_signal_set_array(const char *name, const LumenValue *value)` | Replace an array signal. `value->kind` must be `LUMEN_ARRAY`; each item should be a `LUMEN_MAP` whose entries become one row of the bound `<for>` block. Values are stringified into the row. |
| `lumen_signal_array_len(const char *name, size_t *out_len)` | Row count of an array signal. |
| `lumen_signal_array_get_field(const char *name, size_t row, const char *field, char *buf, size_t buf_len, size_t *out_len)` | One field of one row, string-out convention. |
| `lumen_signals_all(LumenKVList *out)` | The whole signal set. Free with `lumen_kvlist_free`. |

`lumen_signal_get_str` and the two array getters return what the embedder last
pushed through the FFI. A write originating inside the running app, from a
script or a two-way input binding, is not visible through them; the number,
bool, and color getters do see such writes.

## Navigation

| Function | Behaviour |
| --- | --- |
| `lumen_navigate(const char *path)` | Navigate to a page path (`"settings"`, `"/user/7"`, `"/"`). Resolved by longest existing page prefix, not as a URL. |
| `lumen_navigate_back(void)` | Step one entry back. No-op at the start of history. |
| `lumen_navigate_forward(void)` | Step one entry forward. No-op at the end. |
| `lumen_current_page(char *buf, size_t buf_len, size_t *out_len)` | The active page key, string-out convention. Empty before the first page mounts; lags a resolved navigation by at most one tick. |
| `lumen_window_set_href(const char *path)` | Same as `lumen_navigate`. |
| `lumen_window_reload(void)` | Re-navigate to the current page. |
| `lumen_history_go(int delta)` | Step `delta` entries; negative goes back. |

## Window

| Function | Behaviour |
| --- | --- |
| `lumen_window_set_title(const char *title)` | Set the window title. |
| `lumen_window_set_size(float width, float height)` | Resize the window, in logical pixels. |
| `lumen_window_dpr(float *out)` | Current device pixel ratio. |

## Query and traverse

`LumenNode` is an opaque packed handle; `0` means "no node". Reads resolve
against a snapshot rebuilt each tick, so a handle to a removed element reads as
invalid.

| Function | Behaviour |
| --- | --- |
| `lumen_query(const char *selector, LumenNodeList *out_list)` | Every element matching a CSS selector, document order. A bad selector returns `LUMEN_ERR_CSS`. Free the list. |
| `lumen_query_len(const char *selector, size_t *out_len)` | Match count. |
| `lumen_query_single(const char *selector, LumenNode *out)` | Succeeds only on exactly one match; any other count returns `LUMEN_ERR_BAD_ARG` and writes `0`. |
| `lumen_get_by_id(const char *id, LumenNode *out)` | Element with that `id`, or `0`. |
| `lumen_document(LumenNode *out)` | The document root, or `0` before the first tick. |
| `lumen_node_parent` / `_first_child` / `_last_child` / `_next` / `_prev` | Traversal; `0` when absent. |
| `lumen_node_children(LumenNode node, LumenNodeList *out_list)` | Children in document order. Free the list. |
| `lumen_node_closest(LumenNode node, const char *selector, LumenNode *out)` | Nearest ancestor-or-self matching the selector, or `0`. |
| `lumen_node_valid(LumenNode node, int *out)` | Whether the handle is in the current snapshot. |
| `lumen_nodelist_get(LumenNodeList list, size_t index, LumenNode *out)` | Read one handle. Walk `0..list.len`. |
| `lumen_nodelist_free(LumenNodeList list)` | Release a list. Call once. |

## Mutate

| Function | Behaviour |
| --- | --- |
| `lumen_node_spawn(const char *tag, LumenNode *out)` | Create a detached element. The handle is valid for the rest of the tick; attach it before the tick ends. |
| `lumen_document_spawn(const char *tag, LumenNode *out)` | Document-scoped create verb. |
| `lumen_node_clone(LumenNode source, LumenNode *out)` | Deep-clone a subtree into a fresh detached element. |
| `lumen_node_set_attr(LumenNode node, const char *name, const char *value)` | Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map. |
| `lumen_node_remove_attr(LumenNode node, const char *name)` | Remove an attribute. |
| `lumen_node_set_text(LumenNode node, const char *text)` | Replace the text content. |
| `lumen_node_set_inner_markup(LumenNode node, const char *markup)` | Replace the children from a markup fragment. Do not feed untrusted content. A no-op when the app runs from a precompiled artifact, which links no parser. |
| `lumen_node_class_add` / `_class_remove` / `_class_toggle` | Class-list edits. |
| `lumen_node_set_style(LumenNode node, const char *name, const char *value)` | Set one inline style property. |
| `lumen_node_remove_style(LumenNode node, const char *name)` | Remove one inline style property. |
| `lumen_node_append(LumenNode parent, LumenNode child)` | Append `child` under `parent`. |
| `lumen_node_insert_before(LumenNode parent, LumenNode child, LumenNode reference)` | Insert before `reference`; a `reference` of `0` appends. |
| `lumen_node_set_parent(LumenNode node, LumenNode parent)` | Reparent. |
| `lumen_node_replace_with(LumenNode old, LumenNode new_)` | Replace `old`, despawning its subtree. |
| `lumen_node_remove(LumenNode node)` | Detach and despawn the element and its subtree. |

Mutations queue on a bus the runtime drains each tick.

## Introspection

| Function | Behaviour |
| --- | --- |
| `lumen_node_rect(LumenNode node, LumenRect *out)` | Post-layout border box. `LumenRect` carries `x`, `y`, `width`, `height`, `client_x`, `client_y`; local `x` / `y` are relative to the parent, `client_*` are window coordinates. |
| `lumen_node_content_rect(LumenNode node, LumenRect *out)` | Content box, with padding and border removed. |
| `lumen_node_scroll(LumenNode node, LumenScroll *out)` | `x`, `y`, `max_x`, `max_y`. |
| `lumen_node_is_visible(LumenNode node, int *out)` | Effective visibility. |
| `lumen_node_z_index(LumenNode node, int *out)` | Resolved stacking order. |
| `lumen_node_entity_id(LumenNode node, uint32_t *out_index, uint32_t *out_gen)` | Raw entity index and generation. |
| `lumen_node_computed_style(LumenNode node, LumenKVList *out)` | Full computed style. |
| `lumen_node_attrs(LumenNode node, LumenKVList *out)` | Full attribute map. |
| `lumen_node_inline_style(LumenNode node, LumenKVList *out)` | Inline style overrides. |
| `lumen_node_component(LumenNode node, const char *name, LumenKVList *out)` | Field map of one component. A name outside the introspectable set returns `LUMEN_ERR_BAD_ARG`. |
| `lumen_node_classes(LumenNode node, LumenStrList *out)` | Class list. |
| `lumen_node_components(LumenNode node, LumenStrList *out)` | Names of the introspectable components on the element. |
| `lumen_node_outer_markup(LumenNode node, char **out)` | The subtree serialised to markup text. |
| `lumen_node_inner_markup(LumenNode node, char **out)` | The children serialised to markup text. |
| `lumen_dump_tree(char **out)` | Whole-tree structural dump. |
| `lumen_pointer_state(LumenPointerState *out)` | Pointer position, `inside` flag, button bits, and modifier flags. |
| `lumen_frame_info(LumenFrameInfo *out)` | `frame`, `dt_ms`, `dirty_count`. |

Releasers: `lumen_kvlist_free(LumenKVList)`, `lumen_strlist_free(LumenStrList)`,
`lumen_string_free(char *)`.

The introspectable components are `LayoutBox`, `Visuals`, `Opacity`, `ZIndex`,
`Visible`, `TextContent`, `LumenClasses`, `LumenAttributes`, `InlineStyle`, and
`Style`.

## Element events

| Function | Behaviour |
| --- | --- |
| `LumenEventToken lumen_on(LumenNode node, const char *event_type, int capture, LumenEventFn callback, void *user_data)` | Bind a callback to one element and one event type. A non-zero `capture` registers a capture-phase listener. Returns an off token, or `0` on a bad argument. |
| `LumenStatus lumen_off(LumenEventToken token)` | Unbind. No-op for an unknown token. |

The callback receives a `LumenEvent` with the scalar fields `target`,
`current_target`, `local_x`, `local_y`, `client_x`, `client_y`, `delta_x`,
`delta_y`, `button` (the web code: `0` primary, `1` middle, `2` secondary, `3`
back, `4` forward, `5` pen eraser, `-1` none), the modifier flags `shift`,
`ctrl`, `alt`, `super_`, and the pointer's readings: `pointer_id`, `pressure`,
`tangential_pressure`, `tilt_x`, `tilt_y`, `twist`, `width`, `height`,
`buttons` (a mask: `1` primary, `2` secondary, `4` middle, `8` back, `16`
forward, `32` eraser), `pointer_type` (`LUMEN_POINTER_MOUSE`,
`LUMEN_POINTER_PEN`, `LUMEN_POINTER_TOUCH`), and `is_primary`. The struct is
borrowed for the call; copy anything you keep.

| Function | Behaviour |
| --- | --- |
| `LumenStatus lumen_node_set_pointer_capture(LumenNode node, uint64_t pointer_id)` | Send that pointer's events to `node` until its last button comes up. Does nothing while no button is held. |
| `LumenStatus lumen_node_release_pointer_capture(LumenNode node, uint64_t pointer_id)` | Give the pointer back to hit testing, when `node` holds it. |
| `LumenStatus lumen_node_has_pointer_capture(LumenNode node, uint64_t pointer_id, int *out)` | Whether `node` holds the pointer. |

The C++, Python, and Rust SDKs expose the same readings on their `Event` and
the three capture calls on their `Node`.

String fields and propagation controls are separate calls, valid only inside the
callback:

- `lumen_event_type` / `lumen_event_key` / `lumen_event_value` - string-out
  convention
- `lumen_event_target()` / `lumen_event_current_target()` - packed handles, `0`
  outside a callback
- `lumen_event_prevent_default()` - cancel the default action, which is link
  navigation for `click`
- `lumen_event_stop_propagation()` - stop the event reaching further elements
- `lumen_event_stop_immediate_propagation()` - stop the event entirely

Event types and propagation match the script hosts; see
[Scripting](../guides/scripting.md).

## C++ SDK

Header-only C++17 bindings over the C ABI, in `sdk/cpp`. One include,
`<lumen.hpp>`, and no third-party dependencies.

The surface is `lumen::App` for lifecycle and callbacks, `lumen::Signal<T>` for
typed reactive handles (`int64_t`, `double`, `bool`, `std::string`,
`lumen::Color`), `lumen::dom` for the element tree, `lumen::Value` and
`lumen::Args` for exposed callbacks, and `lumen::raw` for the unwrapped C calls.
Lifecycle failures throw `lumen::Error`, carrying the status code and the
last-error text; the signal surface does not throw and returns
`std::optional<T>` where a value may be absent. No C++ exception crosses back
into the C boundary.

```cpp
#include <lumen.hpp>

int main() {
    lumen::App app("counter_app", {.title = "Counter"});

    lumen::Signal<int64_t>     count{"count", 0};
    lumen::Signal<std::string> label{"label", "0 clicks"};

    app.on_click("bump", [&] {
        count += 1;
        label = std::to_string(*count) + " clicks";
    });

    return app.run();
}
```

Link with CMake (`add_subdirectory(sdk/cpp)` provides the `lumen::cpp`
interface target), with `find_package(lumen-cpp CONFIG REQUIRED)`, or with
pkg-config. `liblumen` must be findable at run time. Runnable examples
covering the whole surface live in `sdk/cpp/examples`; see `sdk/cpp/README.md`.

## Python SDK

Stdlib-only `ctypes` bindings, in `sdk/python`, distributed as `lumenui` and
imported as `lumen`. No compiled extension and no build step of its own; the
Rust library is the only thing to build.

The surface is `lumen.Model` and `lumen.Field` for declarative reactive state,
`lumen.Signal[T]` for a single typed signal, `lumen.computed` for derived
values, `signal.watch` for change subscriptions, `lumen.dom` for the element
tree, and `lumen.raw` for the unwrapped C calls. A non-OK status raises a
dedicated exception under `lumen.LumenError`. The package ships a `py.typed`
marker, so the annotations drive type checkers and completion.

```python
import lumen

class Counter(lumen.Model):
    count: int = 0
    label: str = "0 clicks"

app = lumen.App("counter_app", title="Counter")
state = Counter(app)

@app.on_click("bump")
def bump():
    state.count += 1
    state.label = f"{state.count} clicks"

app.run()
```

Install with `pip install -e sdk/python`, which registers the package but does
not build the library. The distribution is named `lumenui` and is pure Python,
so it carries no runtime of its own and loads whichever one the machine has.
`load_library()` looks for `liblumen` under `LUMEN_LIBRARY_PATH` (a file or a
directory), then `LUMEN_LIB_DIR`, then, in a packaged app, the directory
holding the executable, then `target/debug` and `target/release` relative to
the working directory and to the workspace root, then an installed toolchain
(the directory holding the `lumenc` on `PATH`, then `$LUMEN_PREFIX/bin`,
default `~/.lumen/bin`), then the system loader's own paths. Examples run
straight from a checkout:

```sh
cargo build -p lumen
LUMEN_LIBRARY_PATH=target/debug python sdk/python/examples/counter.py
```

Handlers and watches fire on Lumen's tick thread. See `sdk/python/README.md`.

## Rust SDK

The `lumenui` crate is the Rust entry point. It does not go through the C ABI:
it exposes the engine in its native ECS shape, so a handler is an ordinary
`bevy_ecs` system scheduled beside the framework's own.

### Installing

Add `lumenui` and pin the Rust toolchain the Lumen release was built with:

```toml
# Cargo.toml
[dependencies]
lumenui = "X.Y.Z"
```

```toml
# rust-toolchain.toml, beside Cargo.toml
[toolchain]
channel = "<the rustc that Lumen X.Y.Z names>"
```

Your build compiles your code and `lumenui`, and none of the engine. Each Lumen
release ships the engine's compiled crates for every desktop target, and
`lumenui`'s build script downloads the set for its own version: the release
tagged `vX.Y.Z`, checked against that release's `sha256sums.txt`. The set is
large, so it is downloaded once per version and kept in the user cache
directory (`~/.cache/lumen/lumenui-engine/` on Linux, under `XDG_CACHE_HOME`
when that is set; `~/Library/Caches/lumen/lumenui-engine/` on macOS;
`%LOCALAPPDATA%\lumen\lumenui-engine\` on Windows). Delete a version's
directory there to reclaim the space.

Three rules follow from linking compiled crates:

- **The compiler is pinned.** Compiled Rust crates are readable only by the
  exact `rustc` that produced them. When the toolchain in use differs, the
  build stops before compiling anything and names the version to put in
  `rust-toolchain.toml`. `lumenc new` writes that file for a Rust template.
- **Name the engine's crates through `lumenui`.** `bevy_ecs`, `glam`, and the
  `lumen-*` crates are already inside the engine. A dependency of your own on
  any of them compiles a second copy, whose types are not the engine's, so
  nothing typed against it fits. `lumenui::prelude::*` brings the everyday
  items, the ECS derives (`Component`, `Resource`, `Message`, `Bundle`) and
  `Widget` among them; `lumenui::bevy_ecs`, `lumenui::glam` and the other
  re-exports reach the rest. Import the prelude with a glob: the `bevy_ecs`
  derives find their crate through the `bevy_ecs` name it brings in.
- **Build on the machine you target.** The set carries the proc-macro
  libraries your derives run, and those are built for one kind of machine, so
  cross-compiling is refused. The targets are `x86_64` and `aarch64` Linux,
  macOS, and Windows. On Linux the final link needs the development packages
  of the system libraries the engine links (GTK 3, X11, xkbcommon, Wayland),
  as a source build does.

The engine set's features are fixed by the release: it carries the full
default stack. Set `LUMEN_ENGINE_ARTIFACTS` to the directory of an unpacked
set to build against it instead of the release's, which is how a Lumen
checkout tests an unreleased version: `tools/release/lumenui-engine.py build`
then `collect` makes one.

### Linking the engine

A Rust app links the engine into its own executable, on every platform:
`lumenui` points the compiler at the release's compiled crates, and those are
static libraries. `lumenc run`, `lumenc build` and `lumenc package` build the
app with plain cargo, the same build `cargo build` does, so the executable
needs no Lumen library beside it. `lumenc new <name> rust` scaffolds such an
app.

In a Lumen checkout the SDK's code is the `lumenui-engine` crate in `sdk/rust`,
which an app can depend on by path under the name `lumenui`
(`lumenui = { package = "lumenui-engine", path = "..." }`); that compiles the
engine from source and links it in the same way.

Because the engine is inside the executable, a Rust app cannot load a
[runtime module](lumen-toml.md#dependencies), which loads only into the shared
engine; `lumenc package` refuses one. A portable plugin loads into a Rust app
like any other.

### Running headless

`App::run_headless_rendered` runs the whole pipeline with no window: layout,
rendering on the app's `[render] backend` into an offscreen target, scripts,
and the introspection server when `[mcp] simulate` is on. It is the mode
`lumenc run --headless` runs a markup app in. `HeadlessOptions` sets the
target's device pixel ratio and an optional tick bound; without one the run
lasts until SIGINT or SIGTERM. The viewport is the window size. `App::run_headless`
is the cheaper mode that ticks the app and paints nothing.

`App::screenshot` runs the same way and returns the frame the closing tick
painted, as a `Frame` of straight-alpha RGBA8 pixels. `save_png` and
`open_png` read and write PNG files, and two frames compare equal when every
pixel does, so an app can screenshot itself or check a view against a golden
image in its own tests:

```rust
use lumenui::prelude::*;

#[test]
fn the_main_view_matches_its_golden() {
    let frame = app::app()
        .screenshot(HeadlessOptions { dpr: 1.0, ticks: Some(3) })
        .unwrap();
    assert!(frame == Frame::open_png("tests/golden/main.png").unwrap());
}
```

`lumenc run <app> --headless` works on a Rust app too. lumenc runs the app
with cargo and sets `LUMEN_HEADLESS` (with `LUMEN_HEADLESS_TICKS`,
`LUMEN_HEADLESS_DPR` and `LUMEN_HEADLESS_SIZE` for `--ticks`, `--dpr` and
`--size`) on its process, and `App::run` reads them and takes the rendered
headless path with those options. Setting the variables yourself does the
same for a `cargo run`. A malformed value stops the app with an error rather
than opening a window.

### Writing an app

The surface is `App` for assembly, `LumenDefaultPlugins` for the standard stack
(decomposable, so you can disable parts or compose your own group), `Signals`
for typed signal access, the `signals!` macro for typed handles, run conditions
such as `on_click` for gating systems, `App::on_click` for the common
click-to-signal case, and `App::add_computed` for derived signals. `lumen_source!`
picks hot reload from disk in a debug build and embedded markup in a release
build from one line. Give it paths under the app's `src/`: a markup file in a
directory named `src` makes the directory above it the app, so `lumen.toml`
beside `src/` is read, as `lumenc run` reads it.

A release build embeds the entry markup and stylesheet only. The files they
name by relative path (an `@import`, an `@font-face` font, a `url()` image) and
`lumen.toml` are read from the app directory: the executable's own directory
when it holds a `lumen.toml`, which is how `lumenc package` lays an app out,
else the directory `lumen_source!` embedded the sources from, and only when
neither is there the working directory. A release binary therefore runs from
any directory, packaged or on the machine that built it.

A system that reads `ClickEvent`, `DoubleClickEvent` or `LongPressEvent`, or is
gated on `on_click` / `on_toggle`, runs `.after(ClickSet::Producers)`: that set
holds every system that writes those messages (the pointer, Enter and Space on
a focused element, the double-click and long-press detectors), so the system
sees a click on the tick it lands and what it changes paints on that frame.
Without the edge it may run first and see the click a tick late. Closures
registered with `App::on_click` and its siblings are already ordered this way.
A system that writes a signal or edits the DOM needs no ordering to reach the
screen: one the schedule runs after the `bind-*` readers or the DOM applier
paints on the next tick, which runs without waiting for input.

Two markers say where the pointer is. `Hovered` sits on the innermost element
under the pointer, the one a click and the pointer events name. `HoverWithin`
sits on that element and on every ancestor of it, which is what CSS `:hover`
matches, so a system that asks whether a card is hovered, its children
included, queries `With<HoverWithin>`.

```rust
use lumenui::prelude::*;

fn main() -> lumenui::Result<()> {
    lumenui::App::new()
        .add_plugins(
            LumenDefaultPlugins
                .with_source(lumen_source!("examples/src/main.lmn", "examples/src/main.css")),
        )
        .insert_signal("count", 0i64)
        .add_systems(
            TickStage::Systems,
            (bump_counter, update_label).chain().after(ClickSet::Producers),
        )
        .run()
}

fn bump_counter(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
    let hits = clicks.read().count();
    if hits > 0 {
        let total = signals.get_or::<i64>("count", 0) + hits as i64;
        signals.set("count", total);
    }
}
```

For an app that is mostly markup and script with a little Rust behind it,
`lumenui::simple::App::builder()` assembles one from a directory and runs it.
`native_fn` on that builder is the Rust counterpart of `lumen_app_expose`: it
takes a name, an arity, and a closure over `ScriptValue`, and registers into
every host the app runs, so scripts call it whatever language they are written
in.

```rust
use lumenui::ScriptValue;
use lumenui::simple::App;

fn main() -> lumenui::Result<()> {
    App::builder()
        .dir("app")
        .native_fn("answer", 0, |_args| ScriptValue::I64(42))
        .run()
}
```

`script_fn` on the same builder takes a described function rather than a bare
closure: parameter types, a return type, a one-line doc, a namespace of your
choosing, and the languages that may see it. `ScriptFn::from_fn` reads the
types off a plain Rust closure, and `ScriptFn::new` spells them out.

```rust
use lumenui::{ScriptFn, ScriptTy, ScriptValue};

App::builder()
    .dir("app")
    .script_fn(ScriptFn::from_fn("set_pin", |pin: i64| pin > 0).param_names(["pin"]))
    .script_fn(
        ScriptFn::new("read_pin")
            .param("pin", ScriptTy::Int)
            .ret(ScriptTy::Int)
            .doc("Read a GPIO pin.")
            .build(|cx| match cx.int_arg(0) {
                pin @ 0..=27 => Ok(ScriptValue::I64(pin)),
                pin => Err(format!("pin {pin} is out of range")),
            }),
    )
    .run()
```

A `script_fn` body may fail: `Err(message)` raises `message` in the script that
called it, as a Rhai runtime error, a Lua error, or a candela `host_fn_error`.
A `native_fn` closure always succeeds, as an exposed `LumenFn` does.

Both land in the same registry as `lumen_app_expose` and as anything a plugin
registers. A plugin is the third way in, and it goes through `add_plugin`: that
installs it before the markup is parsed and before the script hosts load, which
is the window a registration has to arrive in, and the one a widget plugin's
tag has to arrive in for the markup to use it. See [Writing plugins](../contributing/plugins.md) for what a plugin
can expose and how each language spells a namespace.

The builder has no hook into one language's engine: `AppBuilder::rhai_extension`
and the crate's `host-rhai` feature were removed when the script hosts became
runtime modules, and `script_fn` and `native_fn` above replace them, reaching
every language the app runs.

### Dialogs and the clipboard

`FileDialog` opens the native file dialogs, the same ones the script
functions open. Pick the kind with `FileDialog::open()`, `open_multiple()`,
`save()` or `pick_folder()`, add `filter(label, &["png"])`, a starting
`directory(path)` and a suggested `file_name(name)`, then hand it a callback.
From a system, queue it with `commands.queue(dialog.then(callback))`; with the
world in hand, `dialog.show(world, callback)`. The callback runs on the main
thread on a tick after the dialog closes, with the world and the chosen paths,
and with no paths when the user cancelled.

```rust
use lumenui::prelude::*;

fn open_document(mut commands: Commands) {
    commands.queue(
        FileDialog::open()
            .filter("Images", &["png"])
            .then(|world: &mut World, paths| {
                if let Some(path) = paths.into_iter().next() {
                    world.insert_resource(OpenFile(path));
                }
            }),
    );
}
```

`Clipboard` is a system parameter over the system clipboard: `text()` and
`set_text(text)`, and `image()` and `set_image(&image)` for images as RGBA8
pixels in memory (`RgbaImage`, four bytes per pixel, rows top to bottom). A
system that takes it runs on the main thread. With no clipboard the reads find
nothing and the writes return `false`.

```rust
fn paste(clipboard: Clipboard) {
    if let Some(image) = clipboard.image() {
        println!("pasted {}x{}", image.width(), image.height());
    }
}
```

Runnable examples live in `sdk/rust/examples`.

### Frame timing and the renderer

`FrameTiming` is a resource holding when the last tick ran, phase by phase. A
system reads the tick before its own: `frame` is that tick's `Tick::frame`,
`start` its start, `update_end` the end of input, app systems, style and
layout, `extract_end` the moment the render world held its scene, `encoded`
when the renderer had built its draw commands, `submitted` when it handed them
to the GPU (or finished rasterizing, on the CPU renderer), `render_end` the end
of the render step, and `presented` when a window put the frame on screen. A
tick that painted nothing has every field from `extract_end` on `None`, and
`painted()` says so; `duration()` runs from `start` to the last instant
recorded. The instants are CPU clock readings: `submitted` on the GPU renderer
is when the work was queued, not when the GPU finished it, and an offscreen
(headless) frame is never `presented`.

Every pointer, wheel and key message carries the moment the event happened:
`PointerData::time_stamp` on `PointerMoved`, `PointerPressed`,
`PointerReleased` and `PointerCancelled`, and `time_stamp` on `MouseWheel`,
`KeyPressed` and `KeyReleased`. In a page it is the browser's `timeStamp`; a
desktop window stamps the event when it receives it. The time from an input to
the frame that shows it is the `presented` (or `submitted`) instant of that
frame's `FrameTiming` minus the event's `time_stamp`.

```rust
use std::time::Instant;
use lumenui::prelude::*;

/// Frame time, and how long the latest pointer move took to reach the screen.
fn measure(
    timing: Res<FrameTiming>,
    mut moves: MessageReader<PointerMoved>,
    mut pending: Local<Option<Instant>>,
) {
    // `timing` is the tick before this one, which drew what that tick's
    // moves changed.
    if timing.painted()
        && let Some(input) = pending.take()
    {
        let shown = timing.presented.unwrap_or(timing.end());
        println!("frame {:?}, latency {:?}", timing.duration(), shown - input);
    }
    if let Some(latest) = moves.read().last() {
        *pending = Some(latest.pointer.time_stamp);
    }
}
```

`RendererInfo` is a resource saying which renderer draws the app, present
once it has drawn a frame: `backend` is its `[render] backend` name (`gpu` or
`cpu`, so under `auto` it tells which one started), `device`, `device_kind`,
`vendor_id`, `device_id`, `api` and `driver` describe what it runs on, and
`max_texture_side` is the largest texture it makes. On the GPU renderer
`memory` holds the bytes its allocations use (`allocated`) and the bytes of the
blocks they come from (`reserved`), refreshed at most twice a second. No
portable graphics API reports how much memory the device has, so that is not
here.

### Canvas

The `lumenui-canvas` crate gives a Rust app the [`<canvas>`](tags.md#canvas)
element and draws on it from Rust: the same canvas module scripts use, so a
script and the app can draw on one canvas and share one buffer. Add it beside
`lumenui`, at the same version:

```toml
[dependencies]
lumenui = "X.Y.Z"
lumenui-canvas = "X.Y.Z"
```

It builds nothing of its own: the module is compiled into the same engine set
`lumenui` downloads, under the same pinned compiler. Install `CanvasPlugin`
with `add_plugin`, which registers the tag before the markup is parsed.
`lumenc check` reads the app's `Cargo.toml` for the same reason: a
`lumenui-<name>` (or `lumen-<name>`) dependency on a bundled module counts as a
dependency on that module, so the elements it brings check without a
`lumen.toml` entry, which a Rust app cannot carry.

`Context2d::new(id)` is the drawing context of the canvas whose element has
that `id`, the counterpart of the HTML canvas's `CanvasRenderingContext2D`:
paths (`begin_path`, `move_to`, `line_to`, `quad_to`, `bezier_to`, `arc`,
`rect`, `close_path`, `fill`, `fill_evenodd`, `stroke`), rectangles,
`clear_rect`, `clip` and `clip_evenodd`, colours, line style,
`set_global_alpha`, `set_global_composite_operation`, `save` / `restore`, the
transform, `set_font` and `fill_text`, `clear` and `resize`.
`Context2d::of(&canvas)` takes the `Canvas` component of an adopted element.
The calls that parse text (`set_fill_style`, `set_stroke_style`, `set_font`)
return a `CanvasError` where the script functions answer `false`;
`CompositeOperation`, `LineCap` and `LineJoin` parse from their HTML
spellings with `str::parse`.

`PixelBuffer::new(width, height)` is a straight RGBA8 buffer the app owns; it
is freed when dropped. Every pixel call lives on its `BufferId` handle, which
`raw()` hands to a script and `BufferId::from_raw` takes back:

| Call | Effect |
| --- | --- |
| `write_rgba8(x, y, w, h, &[u8])` | Write a rectangle from straight RGBA8 rows in one call. |
| `write_packed(x, y, w, h, &[u32])` | The same from `0xRRGGBBAA` values. |
| `read_rgba8(x, y, w, h, &mut [u8])` | Read a rectangle back; outside the buffer reads transparent. |
| `read_packed(x, y, w, h, &mut [u32])` | The same as `0xRRGGBBAA` values. |
| `fill_rect`, `pixel`, `set_pixel` | One colour over a rectangle, one pixel at a time. |
| `resize(w, h)` | Change the size, keeping the pixels both sizes share at the top left. |
| `load_png(path)`, `save_png(path)` | PNG in and out; a relative path resolves against the app. |

`ctx.draw_buffer(&buffer, x, y)` and `draw_buffer_scaled(&buffer, x, y, w,
h)` put a buffer on the canvas. `ctx.set_image_smoothing(false)` samples the
nearest pixel when a buffer is drawn at another size, so zoomed pixel art
stays sharp; smoothing is on by default, as `imageSmoothingEnabled` is on the
web, and `save` / `restore` carry it.

A draw takes the buffer's pixels at the call, as `drawImage` does on the web: a
write after it, even before the tick encodes, changes the buffer and not what
was drawn. So redraw after a write: `clear`, then draw the buffer again. That
is cheap whatever the buffer's size: drawing an unchanged buffer uploads
nothing, and drawing one after a write uploads only the area the write touched.
Drawing calls land in the frame of the tick that encodes them; a system ordered
`.before(CanvasSet::Encode)` draws into the same frame.

`CanvasReader` reads a canvas back, the counterpart of `getImageData`. It is a
system parameter, because a read rasterizes everything drawn so far and text
needs the app's shaper; calls not yet encoded are encoded first, so a read sees
every call made before it. `reader.read_rgba8(&ctx, x, y, w, h, &mut out)` fills
a slice with straight RGBA8 rows, one pixel per canvas unit, and
`read_packed` fills `0xRRGGBBAA` values. It answers `CanvasError::NoRenderer`
until the renderer has drawn its first frame. The read is a full rasterize of
the region on the app's renderer: vello_cpu on the CPU renderer, and on the
GPU renderer a render into a texture of the region's size followed by a copy
back that waits for the GPU, whose first read also builds a second set of
vello's pipelines. `ctx.get_image_data(x, y, w, h)` is the script form: it
returns a `PixelBuffer` that is filled when the canvas next encodes.

```rust
use lumenui::prelude::*;
use lumenui_canvas::{CanvasReader, Context2d};

fn eyedropper(mut reader: CanvasReader) {
    let mut pixel = [0u8; 4];
    if reader.read_rgba8(&Context2d::new("art"), 10, 10, 1, 1, &mut pixel).is_ok() {
        // `pixel` is what the canvas shows at canvas unit (10, 10).
    }
}
```

```rust
use lumenui::prelude::*;
use lumenui_canvas::{CanvasPlugin, CanvasSet, Context2d, PixelBuffer};

#[derive(Resource)]
struct Art(PixelBuffer);

fn paint(art: Res<Art>) {
    art.0.write_rgba8(16, 16, 2, 2, &[255, 0, 0, 255].repeat(4)).unwrap();
    let ctx = Context2d::new("art");
    ctx.clear();
    ctx.set_image_smoothing(false);
    ctx.draw_buffer_scaled(&art.0, 0.0, 0.0, 512.0, 512.0);
}

fn main() -> lumenui::Result<()> {
    lumenui::App::new()
        .add_plugins(LumenDefaultPlugins.with_markup(
            r#"<root><canvas id="art" width="512" height="512"/></root>"#,
        ))
        .add_plugin(CanvasPlugin::default())
        .insert_resource(Art(PixelBuffer::new(64, 64).unwrap()))
        .add_systems(TickStage::Systems, paint.before(CanvasSet::Encode))
        .run()
}
```

A small pixel painter lives in `std/canvas/sdk/examples/paint.rs`. The
buffer caps from the module's `config` table (see [candela's
Canvas section](scripting-candela.md#pixel-buffers)) apply to a Rust app too,
set with `CanvasPlugin::with_caps`; the region cap does not, since a slice
crosses no script boundary.
