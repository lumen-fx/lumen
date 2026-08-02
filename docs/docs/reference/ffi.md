# C ABI

Lumen ships a C ABI so a program written in another language can build, drive,
and inspect a Lumen app: open the window, read and write signals, handle clicks
and DOM events, and mutate the live element tree. The header is
`lumen/ffi/include/lumen.h`, and the library it declares is `liblumen_ffi`
(cdylib and staticlib, with a `lumen.pc` for pkg-config).

Reach for it when the app's logic lives outside Lumen: an existing C++ or Python
program that wants a native UI, a language binding, or a tool that drives an app
under test. If your logic can live in the app itself, a script is the shorter
path; see [Scripting](../authoring/scripting.md).

## Quick start

```c
#include <lumen.h>
#include <stdio.h>

static void bump(const char *id, void *user_data) {
    int64_t *count = user_data;
    *count += 1;
    lumen_signal_set_int64(NULL, "count", *count);
}

int main(void) {
    /* Refuse a header / library mismatch in the major+minor pair. */
    if ((lumen_abi_version() >> 8) != (LUMEN_API_VERSION >> 8)) {
        fprintf(stderr, "lumen: ABI mismatch\n");
        return 1;
    }

    LumenApp *app = lumen_app_new("./myapp");
    if (!app) {
        fprintf(stderr, "lumen: %s\n", lumen_last_error());
        return 1;
    }

    int64_t count = 0;
    lumen_app_on_click(app, "bump", bump, &count);

    return lumen_app_run(app) == LUMEN_OK ? 0 : 1;   /* blocks; frees `app` */
}
```

`lumen_app_new(dir)` reads an app directory the way `lumenc run` does.
`lumen_app_new_from_lmna(data, len, base_dir)` takes a precompiled artifact
instead, for a launcher that compiles the source itself and hands over the
bytes.

`lumen_app_run_headless(app, ticks)` builds and ticks the app with no window and
no GPU surface, which is how the SDK examples and tests exercise the ABI.

## What the ABI covers

| Area | Entry points |
|---|---|
| Lifecycle | `lumen_app_new`, `lumen_app_new_from_lmna`, `lumen_app_set_title`, `lumen_app_set_size`, `lumen_app_run`, `lumen_app_run_headless`, `lumen_app_free` |
| Signals | Typed setters and getters (`lumen_signal_set_int64` / `_float64` / `_bool` / `_color` and their `get` counterparts), `lumen_signal_set_string` / `lumen_signal_get_string`, array signals (`lumen_signal_set_array`, `lumen_signal_array_len`, `lumen_signal_array_get_field`), and `lumen_signal_clear` |
| Change subscription | `lumen_signal_watch(name, cb, user_data)` fires once per tick in which the named signal's committed value changed |
| Native handlers | `lumen_app_on_click(app, id, cb, user_data)` for an id-scoped click, `lumen_app_on_close(app, cb, user_data)` for a vetoable close request |
| Exposed callbacks | `lumen_app_expose` / `lumen_app_expose_v2` publish a native function the app's script can call |
| Navigation | `lumen_navigate`, `lumen_navigate_back`, `lumen_navigate_forward`, `lumen_current_page`, plus `lumen_window_set_href` / `_reload` / `_set_title` / `_set_size` / `_dpr` and `lumen_history_go` |
| Dynamic DOM | Query, traversal, mutation, events, and introspection (below) |
| Errors | `lumen_last_error`, `lumen_last_error_global`, `lumen_status_message` |

## The dynamic DOM API from C

The DOM API scripts use is the same one the ABI exposes, against the same live
tree. A `LumenNode` is an opaque packed handle and `0` means "no node".

Find and traverse: `lumen_query`, `lumen_query_single`, `lumen_query_len`,
`lumen_get_by_id`, `lumen_document`, `lumen_node_parent`,
`lumen_node_first_child`, `lumen_node_last_child`, `lumen_node_next`,
`lumen_node_prev`, `lumen_node_children`, `lumen_node_closest`,
`lumen_node_valid`. The list-returning calls fill a `LumenNodeList` you index
with `lumen_nodelist_get` and release with `lumen_nodelist_free`.

Mutate: `lumen_node_spawn` and `lumen_node_clone` mint a handle valid for the
rest of the tick; `lumen_node_append`, `lumen_node_insert_before`,
`lumen_node_set_parent`, `lumen_node_replace_with`, and `lumen_node_remove`
place it. Content and styling go through `lumen_node_set_attr`,
`lumen_node_remove_attr`, `lumen_node_set_text`, `lumen_node_class_add` /
`_remove` / `_toggle`, `lumen_node_set_style`, and `lumen_node_remove_style`.
Mutations queue on a bus the runtime drains each tick, so they are safe to call
from any thread.

```c
LumenNode list, row;
if (lumen_get_by_id("list", &list) == LUMEN_OK && list != 0) {
    lumen_node_spawn("row", &row);
    lumen_node_class_add(row, "item");
    lumen_node_set_text(row, "hello");
    lumen_node_append(list, row);
}
```

Bind events with `lumen_on(node, event_type, capture, callback, user_data)`,
which returns a token you pass to `lumen_off`. The callback receives a
`LumenEvent` with the scalar fields (target, current target, positions, wheel
delta, button, modifiers); read the string fields with `lumen_event_type`,
`lumen_event_key`, and `lumen_event_value`, and control propagation with
`lumen_event_prevent_default`, `lumen_event_stop_propagation`, and
`lumen_event_stop_immediate_propagation`. Propagation is the DOM contract:
capture from the root to the target, then bubble back up.

Introspect: post-layout geometry (`lumen_node_rect`, `lumen_node_content_rect`,
`lumen_node_scroll`), visibility and paint order (`lumen_node_is_visible`,
`lumen_node_z_index`), computed style, attributes, inline style and components
as key-value buffers (`lumen_node_computed_style`, `lumen_node_attrs`,
`lumen_node_inline_style`, `lumen_node_component`), name lists
(`lumen_node_classes`, `lumen_node_components`), markup
(`lumen_node_outer_markup`, `lumen_node_inner_markup`, `lumen_dump_tree`), and
global state (`lumen_pointer_state`, `lumen_frame_info`, `lumen_signals_all`).
Release what they hand back with `lumen_kvlist_free`, `lumen_strlist_free`, or
`lumen_string_free`.

`lumen_node_set_inner_markup` replaces a node's children from a markup fragment.
It needs the markup parser, so it does nothing when the app runs from a
precompiled artifact, and it must not be fed untrusted content.

## Conventions

**Status codes.** Every function returns `LumenStatus` (or `NULL` for a
constructor). `LUMEN_OK` is 0. On any other value, `lumen_last_error()` returns
a thread-local UTF-8 message, falling back to the global slot when the calling
thread has none, and `lumen_status_message(status)` gives a static description
without that round-trip.

**No panic escapes.** Every entry point catches a Rust panic and reports
`LUMEN_ERR_PANIC` rather than unwinding across the boundary.

**Strings are UTF-8, NUL-terminated.** There is no wide-char surface.

**String out-parameters size themselves.** Call once with a null or short
buffer: nothing is written, `*out_len` receives the required capacity including
the trailing NUL, and the call returns `LUMEN_ERR_BUFFER_TOO_SMALL`. Call again
with a buffer that size.

**Ownership is explicit.** Anything the library allocates has a matching
`lumen_*_free`. Pointers handed to a callback are borrowed for the duration of
the call; copy what you keep.

**Threading.** Every `lumen_*` function is safe to call from any thread.
Callbacks fire on the Lumen tick thread, which may not be your main thread, so
the `user_data` you register must stay alive and be safe to read from there.

**Version check.** `LUMEN_API_VERSION` is the header's view of the ABI and
`lumen_abi_version()` is the linked library's. Compare the major and minor pair
at startup and refuse a mismatch; the patch component carries no API meaning.

## SDKs

The SDKs wrap the ABI in something idiomatic, and are the recommended entry
point over raw C:

| SDK | Shape |
|---|---|
| [C++](https://github.com/lumen-fx/lumen/tree/main/sdk/cpp) | Header-only C++17. Typed `lumen::Signal<T>` handles with natural operators, lambda click handlers, and a chainable `lumen::dom` namespace over the DOM API. |
| [Python](https://github.com/lumen-fx/lumen/tree/main/sdk/python) | Stdlib `ctypes`, no compiled extension. A dataclass-style `lumen.Model` whose fields are signals, decorator handlers, and `lumen.dom`. |
| [Rust](https://github.com/lumen-fx/lumen/tree/main/sdk/rust) | In-process embedding: build an app, add your own systems and plugins, and reach signals, events, and the DOM with Rust types. |

Each SDK ships runnable examples covering signals, handlers, exposed callbacks,
lists, and the DOM.

## Callbacks the script can call

`lumen_app_expose(app, name, arg_count, func, user_data)` publishes a native
function under `name` so the app's script can call it, and
`lumen_app_expose_v2` is the same registration with an out-parameter callback
signature, which suits bindings that cannot express an aggregate return
(`ctypes`, libffi). Arguments and return values cross as `LumenValue`, a tagged
union covering nil, bool, int, float, string, array, and map.

Exposed callbacks currently reach Rhai scripts. From a candela or Lua app, drive
the native side through signals, `lumen_app_on_click`, and DOM event callbacks
instead.
