# candela scripting reference

Every builtin the candela script host registers, with its signature, parameters,
and behaviour. candela is Lumen's default script language; for the task-oriented
introduction see [Scripting](../guides/scripting.md). The candela language itself
is documented separately at [/candela/](https://docs.lumenfx.dev/candela/); this
page covers only the Lumen surface.

The same surface in the other hosts: [rhai](scripting-rhai.md),
[lua](scripting-lua.md).

## Selecting the host

An app runs one script host. Lumen picks it from `lumen.toml`:

```toml
[script]
engine = "candela"   # "candela" (default) | "rhai" | "lua"
```

With no `[script] engine` key, the host is inferred from the file extensions in
the app directory: a `.cdl` file selects candela, then `.lua` selects lua, then
`.rhai` selects rhai. With none of those present, candela is the default. Every
script file the markup references is concatenated into one program.

## Reaching the builtins

candela resolves host functions through a typed `host "lumen" { ... }` block. One
import line declares the whole Lumen surface:

```rust
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "hi");
}

fn main() {}
```

The import is replaced with the declarations before compilation, so no
`lumen.cdl` file is read from disk. Without the import (or a hand-written `host`
block), the source still compiles, and calling a builtin fails at run time with
`lumen is not a valid namespace`.

The prelude also declares the `window`, `document`, and `history` namespaces and
defines the `Node` / `Event` method wrappers described below.

Types in signatures are candela types: `int`, `float`, `bool`, `string`, arrays
(`int[]`), and maps (`{string: float}`). A builtin with no return type returns
null.

## Lifecycle hooks

Define these as free functions. Each is optional; a missing hook is a no-op.

| Hook | Fires |
| --- | --- |
| `on_start()` | Once at app construction, before the first tick. No element is queryable yet: `node_get_by_id` returns `0`. |
| `on_ready()` | Once per mount, on the first tick after the element tree is published. Queries resolve here. Re-armed after a hot reload, so it runs again on the fresh tree. |
| `on_close()` | On an OS close request, before teardown. Return `false` to veto the close and keep the window open. |
| `on_audio_end()` | When the audio transport reaches the end of a track. |
| `main()` | candela's module entry point, run once at compile time. Keep it empty unless the app needs module-level setup. |

Across a hot reload the signal values, the `on(...)` routing table, and live
event bindings are preserved; the recompiled program picks them up.

## Event handlers

Each event dispatches to a global handler function named below. Handlers are
optional.

| Handler | Arguments |
| --- | --- |
| `on_click(id)` | Element id. Suppressed when a double-click fires on the same element in the same tick. |
| `on_double_click(id)` | Element id. |
| `on_long_press(id)` | Element id. |
| `on_toggle(id, checked)` | Element id, `bool`. |
| `on_slider(id, value)` | Element id, `float`. |
| `on_text_input(id, text)` | Element id, current text. Fires on every edit that changes the text, and once more on Enter commit. |
| `on_drop(target_id, payload)` | Drop-zone id, the source's text payload. |
| `on_drag_start(source_id, payload)` | Drag-source id, its text payload. |
| `on_file_dropped(id, path)` | Element id, dropped file path. |
| `on_file_picked(tag, path)` | Dialog tag, chosen path. Empty path on cancel. |
| `on_files_picked(tag, paths)` | Dialog tag, paths joined with `\|`. |
| `on_folder_picked(tag, path)` | Dialog tag, chosen folder. |
| `on_hotkey(name)` | Registered hotkey name. |
| `on_menu(id)` | Menu item id. |
| `on_tray(id)` | Tray icon id. |
| `on_dialog_accepted(id)` | Dialog id. |
| `on_dialog_rejected(id)` | Dialog id. |
| `on_timer(name)` | Timer name. |
| `on_fetch(tag, body)` | Request tag, response body (2xx only). |
| `on_fetch_error(tag, message)` | Request tag, error text (transport failure or non-2xx). |

### Per-element routing

```rust
lumen::on(event: string, id: string, handler: string)
```

Routes one `(event, id)` pair to the script function named `handler`, bypassing
the global handler for that pair only. `event` is the name without the `on_`
prefix (`"click"`, `"toggle"`, `"timer"`, `"file_picked"`, ...). A handler
registered for `save` also matches template-instance ids ending in `:save`.

## Signals

Signals are the named reactive cells markup binds to with `bind-text` and
friends.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::signal_get(name: string)` | `string` | Read as a string. Empty when never written. |
| `lumen::signal_set(name: string, value: string)` | | Write a string. |
| `lumen::signal_get_int(name: string)` | `int` | Read as an integer. `0` on miss or a non-numeric value. |
| `lumen::signal_set_int(name: string, value: int)` | | Write an integer. |
| `lumen::signal_get_float(name: string)` | `float` | Read as a float. `0.0` on miss or a non-numeric value. |
| `lumen::signal_set_float(name: string, value: float)` | | Write a float. |
| `lumen::signal_get_bool(name: string)` | `bool` | Read as a boolean. `false` on miss or an unparseable value. |
| `lumen::signal_set_bool(name: string, value: bool)` | | Write a boolean. |
| `lumen::signals_all()` | `{string: string}` | The whole signal set as a name-to-value map. |

A getter converts across the scalar types: an integer cell read through
`signal_get_float` yields the same number as a float, and a string cell parses.

### Derived signals

```rust
lumen::derive(name: string, deps: string[], f: string)
```

Registers a computed signal `name`, recomputed by the script function named `f`
whenever any signal in `deps` changes. `f` receives the dependency values in
`deps` order and returns the new value. candela has no closure value, so the
recompute body is referenced by function name.

A derivation runs once after registration, then on every change to a
dependency. Derived-of-derived chains settle within the same tick. A derivation
that errors is retried on the next tick.

## Timers

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_timeout(name: string, ms: int)` | One-shot timer; fires `on_timer(name)` after `ms` milliseconds. |
| `lumen::set_interval(name: string, ms: int)` | Repeating timer; fires `on_timer(name)` every `ms` milliseconds. |
| `lumen::cancel_timer(name: string)` | Cancel the named timer. |

Timer names are unique: setting a timer with an existing name replaces it.
Negative delays clamp to zero. Cancelling from inside the timer's own handler
takes effect before the next fire.

## Query and traverse the element tree

Node handles are `int` ids; `0` means "no node". Reads resolve against the
snapshot rebuilt each tick, so a handle to a removed element reads as invalid.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_query(selector: string)` | `int[]` | Every element matching a CSS selector, document order. |
| `lumen::node_get_by_id(id: string)` | `int` | Element with that `id`, or `0`. |
| `lumen::node_document()` | `int` | The document root. |
| `lumen::node_parent(node: int)` | `int` | Parent, or `0`. |
| `lumen::node_first_child(node: int)` | `int` | First child, or `0`. |
| `lumen::node_last_child(node: int)` | `int` | Last child, or `0`. |
| `lumen::node_next(node: int)` | `int` | Next sibling, or `0`. |
| `lumen::node_prev(node: int)` | `int` | Previous sibling, or `0`. |
| `lumen::node_children(node: int)` | `int[]` | Children in document order. |
| `lumen::node_closest(node: int, selector: string)` | `int` | Nearest ancestor-or-self matching the selector, or `0`. |
| `lumen::node_valid(node: int)` | `bool` | Whether the handle is in the current snapshot. |

## Mutate the element tree

Mutations queue a command applied later in the same tick.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_spawn(tag: string)` | `int` | Create a detached element. The handle is valid for the rest of the tick; attach it before the tick ends. |
| `lumen::node_clone_deep(source: int)` | `int` | Deep-clone a subtree into a fresh detached element. |
| `lumen::node_set_attr(node: int, name: string, value: string)` | | Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map. |
| `lumen::node_remove_attr(node: int, name: string)` | | Remove an attribute. |
| `lumen::node_set_id(node: int, id: string)` | | Set the `id` attribute. |
| `lumen::node_set_text(node: int, text: string)` | | Replace the text content. |
| `lumen::node_set_inner_markup(node: int, markup: string)` | | Replace the children with a parsed markup fragment. Do not feed untrusted content. A no-op when the app runs from a precompiled artifact, which links no parser. |
| `lumen::node_class_add(node: int, class: string)` | | Add one class. |
| `lumen::node_class_remove(node: int, class: string)` | | Remove one class. |
| `lumen::node_class_toggle(node: int, class: string)` | | Toggle one class. |
| `lumen::node_set_class(node: int, classes: string)` | | Replace the whole class list. |
| `lumen::node_set_style(node: int, name: string, value: string)` | | Set one inline style property. |
| `lumen::node_style_remove(node: int, name: string)` | | Remove one inline style property. |
| `lumen::node_append(parent: int, child: int)` | | Append `child` under `parent`. |
| `lumen::node_insert_before(parent: int, child: int, reference: int)` | | Insert `child` before `reference` under `parent`. A `reference` of `0` appends. |
| `lumen::node_set_parent(node: int, parent: int)` | | Reparent `node` under `parent`. |
| `lumen::node_move_to(node: int, parent: int)` | | Same as `node_set_parent`. |
| `lumen::node_replace_with(old: int, new: int)` | | Replace `old` with `new`, despawning `old`'s subtree. |
| `lumen::node_remove(node: int)` | | Detach and despawn the element and its subtree. |

## Read element state

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_get_attr(node: int, name: string)` | `string` | One attribute value; empty when absent. |
| `lumen::node_text(node: int)` | `string` | Text content. |
| `lumen::node_id(node: int)` | `string` | The `id` attribute. |
| `lumen::node_class_contains(node: int, class: string)` | `bool` | Whether the class list contains `class`. |
| `lumen::node_style_get(node: int, prop: string)` | `string` | One inline style override. |
| `lumen::node_computed_style(node: int, prop: string)` | `string` | One resolved style property after the cascade. |
| `lumen::node_computed_style_all(node: int)` | `{string: string}` | Every resolved style property. |
| `lumen::node_inline_style(node: int)` | `{string: string}` | Every inline style override. |
| `lumen::node_attrs(node: int)` | `{string: string}` | Every attribute. |
| `lumen::node_classes(node: int)` | `string[]` | The class list. |

## Introspection

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_rect(node: int)` | `{string: float}` | Post-layout border box: `x`, `y`, `width`, `height`, `client_x`, `client_y`. Local `x` / `y` are relative to the parent; `client_*` are window coordinates. |
| `lumen::node_content_rect(node: int)` | `{string: float}` | Same keys, for the content box (padding and border removed). |
| `lumen::node_scroll(node: int)` | `{string: float}` | `x`, `y`, `max_x`, `max_y`. |
| `lumen::node_is_visible(node: int)` | `bool` | Effective visibility. |
| `lumen::node_z_index(node: int)` | `int` | Resolved stacking order. |
| `lumen::node_entity_id(node: int)` | `{string: int}` | `index` and `generation` of the backing entity. |
| `lumen::node_components(node: int)` | `string[]` | Names of the introspectable components on the element. |
| `lumen::node_component(node: int, name: string)` | `{string: string}` | Field map of one component. Empty for an absent or non-introspectable name. |
| `lumen::node_outer_markup(node: int)` | `string` | The subtree serialized to markup text. |
| `lumen::node_inner_markup(node: int)` | `string` | The children serialized to markup text. |
| `lumen::dump_tree()` | `string` | Whole-tree structural dump. |
| `lumen::pointer_state()` | `{string: string}` | `x`, `y`, `inside`, `buttons`, `shift`, `ctrl`, `alt`, `super`. Values are stringified. |
| `lumen::frame_info()` | `{string: float}` | `frame`, `dt_ms`, `dirty_count`. |

The introspectable components are `LayoutBox`, `Visuals`, `Opacity`, `ZIndex`,
`Visible`, `TextContent`, `LumenClasses`, `LumenAttributes`, `InlineStyle`, and
`Style`.

## Element events

Bind a handler to one element and one event type. Binding returns a token; pass
it to `event_off` to unbind.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::event_on(node: int, event_type: string, handler: string)` | `int` | Bind `handler` (a function name) for the bubble phase. Returns the off token, or `0` for an unknown node. |
| `lumen::event_on_capture(node: int, event_type: string, handler: string)` | `int` | Same, for the capture phase. |
| `lumen::event_off(token: int)` | | Unbind. |

The handler receives the event id as its only argument and reads the event
through the accessors below:

| Accessor | Returns | Value |
| --- | --- | --- |
| `lumen::event_target(ev: int)` | `int` | The element the event originated on. |
| `lumen::event_current_target(ev: int)` | `int` | The element whose handler is running. |
| `lumen::event_type(ev: int)` | `string` | Event type name. |
| `lumen::event_key(ev: int)` | `string` | Key name for keyboard events. |
| `lumen::event_value(ev: int)` | `string` | Text value for `input` / `change` / `submit`. |
| `lumen::event_button(ev: int)` | `int` | `0` primary, `1` middle, `2` secondary. |
| `lumen::event_x(ev: int)` / `lumen::event_y(ev: int)` | `float` | Pointer position relative to the target. |
| `lumen::event_client_x(ev: int)` / `lumen::event_client_y(ev: int)` | `float` | Pointer position in window coordinates. |
| `lumen::event_delta_x(ev: int)` / `lumen::event_delta_y(ev: int)` | `float` | Wheel delta. |
| `lumen::event_shift(ev: int)` / `_ctrl` / `_alt` / `_super` | `bool` | Modifier state. |
| `lumen::event_prevent_default(ev: int)` | | Cancel the default action. |
| `lumen::event_stop_propagation(ev: int)` | | Stop the event reaching further elements. |
| `lumen::event_stop_immediate_propagation(ev: int)` | | Stop the event entirely, including other handlers on this element. |

### Event types

`click`, `dblclick`, `pointerdown`, `pointerup`, `pointermove`, `pointerenter`,
`pointerleave`, `wheel`, `keydown`, `keyup`, `input`, `change`, `focus`, `blur`,
`submit`, `scroll`.

Dispatch runs capture (root down to the target), then the target, then bubble
(target up to the root). `focus`, `blur`, `pointerenter`, `pointerleave`, and
`scroll` do not bubble.

`input`, `change`, and `submit` all come from the text-commit signal, so they
fire on commit rather than per keystroke. Only `click` has a default action
(link navigation); `prevent_default` on a click skips it.

## Method sugar

The prelude wraps a raw handle in a `Node` or `Event` struct so calls read as
methods. The free functions above keep working on raw handles.

```rust
import "lumen.cdl";

fn on_ready() {
    let list = get_by_id("list");
    let row = spawn("row");
    row.class_add("item");
    row.set_text("hello");
    list.append(row);
    row.on("click", "handle_row");
}

fn handle_row(id) {
    let ev = event(id);
    ev.prevent_default();
}
```

Constructors: `node(handle)`, `event(handle)`, `wrap_nodes(handles)`,
`spawn(tag)`, `get_by_id(id)`, `document_node()`, `query(selector)`.

`Node` methods mirror the `node_*` builtins with the prefix dropped:
`exists`, `valid`, `parent`, `first_child`, `last_child`, `next`, `prev`,
`children`, `closest`, `clone_deep`, `set_attr`, `remove_attr`, `set_id`,
`set_text`, `set_inner_markup`, `class_add`, `class_remove`, `class_toggle`,
`set_class`, `set_style`, `style_remove`, `remove`, `append`, `insert_before`,
`set_parent`, `move_to`, `replace_with`, `get_attr`, `text`, `id`,
`class_contains`, `style_get`, `computed_style`, `is_visible`, `z_index`,
`classes`, `components`, `outer_markup`, `inner_markup`, `on`, `on_capture`.
`exists()` tests the handle against `0`; `valid()` tests it against the current
snapshot.

`Event` methods: `target`, `current_target`, `event_type`, `key`, `value`,
`button`, `x`, `y`, `client_x`, `client_y`, `delta_x`, `delta_y`, `shift`,
`ctrl`, `alt`, `super_key`, `prevent_default`, `stop_propagation`,
`stop_immediate_propagation`, `off`.

One more helper builds a list row in a single call:

```rust
lm_append(parent, tag, cls, text) -> int
```

It spawns `tag`, applies `cls` and `text` when they are non-empty, appends the
result under `parent`, and returns the new handle.

## window, document, history

Each is its own host namespace, declared by the same prelude import.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `window::set_href(path: string)` | | Navigate to a page path. |
| `window::href()` | `string` | The current page path. |
| `window::reload()` | | Re-navigate to the current page. |
| `window::title()` | `string` | Window title. |
| `window::set_title(title: string)` | | Set the window title. |
| `window::dpr()` | `float` | Device pixel ratio. |
| `window::set_size(width: float, height: float)` | | Resize the window, in logical pixels. |
| `window::location_path()` | `string` | The current page path. |
| `window::location_query()` | `string` | Always empty; query strings are not tracked. |
| `window::location_hash()` | `string` | Always empty; fragments are not tracked. |
| `history::back()` | | Step one entry back. |
| `history::forward()` | | Step one entry forward. |
| `history::go(delta: int)` | | Step `delta` entries; negative goes back. |
| `document::root()` | `int` | The document root. |
| `document::query(selector: string)` | `int[]` | Matching elements, document order. |
| `document::get_by_id(id: string)` | `int` | Element with that `id`, or `0`. |
| `document::focused()` | `int` | The focused element, or `0`. |
| `document::hovered()` | `int` | The hovered element, or `0`. |
| `document::spawn(tag: string)` | `int` | Create a detached element. |

## Page navigation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::page(path: string)` | | Navigate to a page path (`"settings"`, `"/user/7"`, `"/"`). |
| `lumen::page_current()` | `string` | The active page key. Spelled apart from `page` because a candela host function takes one arity per name. |
| `lumen::page_back()` | | Step one entry back in the page history. |
| `lumen::page_forward()` | | Step one entry forward. |

See [Pages](../guides/pages.md) for the file layout these paths resolve against.

## Dialogs

Each opens a native dialog and delivers the result to the matching handler,
keyed by `tag`. A cancelled dialog still fires once, with an empty path.

| Builtin | Behaviour |
| --- | --- |
| `lumen::pick_file(tag: string)` | Open-file dialog; fires `on_file_picked(tag, path)`. |
| `lumen::pick_files(tag: string)` | Multi-select dialog; fires `on_files_picked(tag, paths)` with paths joined by `\|`. |
| `lumen::pick_folder(tag: string)` | Folder picker; fires `on_folder_picked(tag, path)`. |
| `lumen::save_file(tag: string, default_name: string)` | Save dialog seeded with `default_name`; fires `on_file_picked(tag, path)`. |
| `lumen::pick_file_filtered(tag: string, spec: string)` | Open-file dialog with extension filters. `spec` is pipe-separated `Label:ext1,ext2` groups; a `*` extension means no filter. |

## OS integration

| Builtin | Behaviour |
| --- | --- |
| `lumen::notify(title: string, body: string)` | Show an OS notification. |
| `lumen::copy_image(path: string)` | Copy the image at `path` to the system clipboard. |
| `lumen::save_clipboard_image(path: string)` | Write the clipboard image to `path` as PNG. |
| `lumen::tray_icon(id: string, icon_path: string, tooltip: string)` | Register or replace a tray icon; clicks fire `on_tray(id)`. An empty tooltip disables it. |
| `lumen::unregister_tray(id: string)` | Remove a tray icon. |
| `lumen::register_hotkey(name: string, accelerator: string)` | Register a global hotkey (`"CommandOrControl+S"`, `"Alt+Space"`, `"F11"`); fires `on_hotkey(name)`. |
| `lumen::unregister_hotkey(name: string)` | Remove a global hotkey. |
| `lumen::open_menu(id: string)` | Open menu `id` by setting the `__menu_open:id` signal to true. |
| `lumen::close_menu(id: string)` | Close menu `id`. |

See [OS integration](../guides/os-integration.md) for the markup these pair with.

## Styling and theming

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_class(id: string, classes: string)` | Replace the class list on the element with that `id`. |
| `lumen::set_root_class(classes: string)` | Replace the class list on the root element, which drives theme-token selectors. |
| `lumen::set_color_scheme(name: string)` | Switch the color scheme: `"default"` (follow the OS), `"force-light"`, `"force-dark"`, `"prefer-light"`, `"prefer-dark"`. An unknown name is ignored with a warning. |

## Audio

| Builtin | Behaviour |
| --- | --- |
| `lumen::audio_play(path: string)` | Load and play the track at `path` (app-relative wav or ogg); resets position to zero. |
| `lumen::audio_pause()` | Pause, holding position. |
| `lumen::audio_resume()` | Resume a paused transport. |
| `lumen::audio_stop()` | Stop and rewind. |
| `lumen::audio_seek(secs: float)` | Seek to `secs`, clamped to the track duration. |
| `lumen::audio_volume(level: float)` | Set output volume in `0.0` to `1.0`. |

The transport writes the `audio_position`, `audio_duration`, and `audio_playing`
signals each tick, so markup binds to them directly.

## Networking

```rust
lumen::fetch(url: string, tag: string)
```

Issues an HTTP GET off the UI thread. A 2xx reply fires `on_fetch(tag, body)`;
a transport failure or non-2xx fires `on_fetch_error(tag, message)`. The reply
is delivered on the tick thread, so a handler may touch signals and the element
tree freely.

The general `http(request)` form and `parse_json` are not available to candela;
both need a value type this host cannot marshal. Use `fetch` plus candela's own
JSON support.

## Translation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::t(key: string)` | `string` | The active locale's string for `key`, or `key` itself when untranslated. |
| `lumen::tr(key: string)` | `string` | Alias for `t`. |

See [Translation](../guides/i18n.md) for the catalogue format.

## Filesystem

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::read_file(path: string)` | `string` | File contents; empty string on error. |
| `lumen::write_file(path: string, contents: string)` | `bool` | `true` on success. |

## Embedder commands

Two builtins emit a command with no built-in effect under `lumenc run`. They
exist for embedders that read the script command stream directly.

| Builtin | Behaviour |
| --- | --- |
| `lumen::add_clicks(n: int)` | Emit an add-clicks command carrying `n`. |
| `lumen::set_string(key: string, value: string)` | Emit a set-string command carrying `key` and `value`. |

Two more write directly to the element tree by id, without a node handle:

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_text(target_id: string, text: string)` | Replace the text content of the element with that `id`. |
| `lumen::set_src(target_id: string, path: string)` | Swap the asset path of an `<image>` at run time. Paths are app-relative. |
