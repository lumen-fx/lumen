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
| `LumenApp *lumen_app_new(const char *dir)` | Build an app rooted at `dir`, which must exist and contain `main.lmn` and/or `lumen.toml`. Returns null on error. |
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

candela resolves every host call through a declared block, so a candela script
declares what it calls, once, and reaches it under the `native` namespace:

```rust
host "native" {
    any now_ms(...);
}

fn on_start() {
    let t = native::now_ms();
}
```

Declare each one variadic with the `any` return type, and declare only what the
embedder exposes: candela checks each declaration against a registered
implementation at compile time. See
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
`delta_y`, `button` (`0` primary, `1` middle, `2` secondary, `-1` none), and the
modifier flags `shift`, `ctrl`, `alt`, `super_`. The struct is borrowed for the
call; copy anything you keep.

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

The `lumenui` crate in `sdk/rust` is the Rust entry point. It does not go
through the C ABI: it links the runtime directly and exposes it in its native
ECS shape, so a handler is an ordinary `bevy_ecs` system scheduled beside the
framework's own.

`lumenui` has exactly one Lumen dependency - the engine - and re-exports
everything an app writes against. That is what lets a packaged Rust app link
the shared library beside it instead of compiling the engine into itself: one
copy of each Lumen type exists, the one inside the library, so a signal set
through the SDK and one set by a script are the same signal.

### Linking the engine

A Rust app does not open the C library. It links the engine as a Rust library,
which is a separate build of the same code: `crates/dylib`, whose output is
`liblumen_engine.{so,dylib}`. The C library exports the `extern "C"` surface
and nothing else, and one crate cannot produce both forms, so they are two
crates.

`lumenc build` and `lumenc package` compile a Rust app with `-C
prefer-dynamic`, which is what makes cargo take the shared form of that library
rather than the static one beside it. Building the app yourself with plain
`cargo build` is fine and links the engine in instead; the app behaves the same
and is simply bigger. The flag also applies to the Rust standard library, so an
app and the engine share one copy of that too, and a packaged app carries both
files. Since both come out of the same build as the executable, there is
nothing to keep in step by hand.

**Windows links the engine in.** A shared library there has to describe its
exports in an import library, and that format stops at 65535 entries, which a
graph this size is well past. So no linkable engine is built for Windows, a
Rust app carries the runtime inside its own executable, and a packaged one has
no library beside it. Everything else about writing the app is the same.

The surface is `App` for assembly, `LumenDefaultPlugins` for the standard stack
(decomposable, so you can disable parts or compose your own group), `Signals`
for typed signal access, the `signals!` macro for typed handles, run conditions
such as `on_click` for gating systems, `App::on_click` for the common
click-to-signal case, and `App::add_computed` for derived signals. `lumen_source!`
picks hot reload from disk in a debug build and embedded markup in a release
build from one line.

```rust
use lumenui::prelude::*;

fn main() -> lumenui::Result<()> {
    lumenui::App::new()
        .add_plugins(
            LumenDefaultPlugins
                .with_source(lumen_source!("examples/main.lmn", "examples/main.css")),
        )
        .insert_signal("count", 0i64)
        .add_systems(TickStage::Systems, (bump_counter, update_label).chain())
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

`rhai_extension` on the same builder takes a `rhai::Engine` and so reaches the
Rhai host alone. It is behind the crate's `host-rhai` feature, on by default;
turning defaults off drops the Rhai dependency and leaves `native_fn` as the
way to expose a function.

Runnable examples live in `sdk/rust/examples`.
