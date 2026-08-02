# Scripting

Lumen apps script in candela. A `.cdl` file next to your markup holds the
handlers, the runtime calls them by name, and one import line gives the script
the whole Lumen surface: signals, timers, the dynamic DOM API, native dialogs,
hotkeys, and the rest.

The language itself (syntax, types, standard library) is documented at
<https://candela.lumenfx.dev/>. This page covers what a script can reach inside
Lumen.

## Attaching a script

Reference the file from your markup:

```xml
<script src="main.cdl" />
```

and select the host in `lumen.toml`:

```toml
[script]
engine = "candela"
```

Then open the script with the prelude import:

```candela
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "ready");
    lumen::on("click", "bump", "handle_bump");
}

fn handle_bump(id) {
    lumen::signal_set("greeting", "clicked");
}

fn main() {}
```

`import "lumen.cdl";` declares every builtin on this page in one line. Without
it a script gets no builtins, which is the point: the import is the opt-in.

Calls are namespaced. `lumen::` holds the app surface; `window::`, `document::`,
and `history::` mirror their web counterparts.

`main` runs once when the program loads, so keep it empty and do setup in
`on_start` or `on_ready`. `on_start` runs before the first tick, when no element
has been laid out yet; `on_ready` runs on the first tick, after the element tree
is mounted and queryable, which is where you build an initial DOM.

## Lifecycle callbacks

These are functions you write. The runtime calls them when the matching thing
happens; a script that omits one just does not get that event.

| Function | When it runs |
|---|---|
| `on_start()` | Once at load, before the first tick. Seed signals, register handlers. |
| `on_ready()` | Once on the first tick, after the tree is mounted and queryable. |
| `on_click(id)`, `on_double_click(id)`, `on_long_press(id)` | Pointer dispatchers. A per-id handler registered with `lumen::on` takes precedence for that pair. |
| `on_text_input(id, text)` | Text commit on `<input>` / `<textarea>`. |
| `on_toggle(id, checked)` | Toggle state change. |
| `on_slider(id, value)` | Slider drag commit; `value` is a float. |
| `on_drop(id, payload)` / `on_drag_start(id, payload)` | Drag-and-drop payload text. |
| `on_file_dropped(id, path)` | File dropped onto a `<... drop="true">` target. |
| `on_file_picked(tag, path)`, `on_files_picked(tag, paths)`, `on_folder_picked(tag, path)` | File dialog results. `on_files_picked` joins the paths with a pipe. An empty path means the dialog was cancelled. |
| `on_fetch(tag, body)` / `on_fetch_error(tag, message)` | Result of `lumen::fetch(url, tag)`. A non-2xx status arrives as an error. |
| `on_timer(name)` | A `set_timeout` / `set_interval` timer fired. |
| `on_menu(id)` | `<menu>` or `<menubar>` item clicked. |
| `on_hotkey(name)` | Registered global hotkey fired. |
| `on_tray(id)` | System tray icon clicked. |
| `on_dialog_accepted(id)` / `on_dialog_rejected(id)` | `<dialog>` closed. |
| `on_close()` | The window is about to close. Return `false` to keep it open. |

## Per-id handlers

`lumen::on(event, id, handler)` routes one element's event to a named function,
instead of the global `on_<event>(id)` dispatcher:

```candela
fn on_start() {
    lumen::on("click", "save", "handle_save");
    lumen::on("click", "cancel", "handle_cancel");
}

fn handle_save(id) { lumen::signal_set("status", "saved"); }
fn handle_cancel(id) { lumen::signal_set("status", ""); }
```

Template instances namespace their inner ids (`user-card:save`). A handler
registered for the bare suffix `save` matches every instance; register the
qualified id for per-instance routing.

The event names accepted here are the lifecycle names without the `on_` prefix:
`click`, `double_click`, `long_press`, `text_input`, `toggle`, `slider`, `drop`,
`file_dropped`, `file_picked`, `files_picked`, `folder_picked`, `fetch`,
`fetch_error`, `timer`, `menu`, `hotkey`, `tray`, `dialog_accepted`,
`dialog_rejected`.

## Signals

Signals are the named reactive values that `bind-text`, `bind-value`, `<if>`,
and `<for>` read. Scripts read and write them by name, in one of four scalar
types:

```candela
lumen::signal_set("greeting", "hello");
let text = lumen::signal_get("greeting");

lumen::signal_set_int("count", 0);
let n = lumen::signal_get_int("count");
lumen::signal_set_int("count", n + 1);
```

Reading a signal that was never written returns the empty string, `0`, `0.0`,
or `false`.

`lumen::derive(name, deps, f)` registers a computed signal. candela references
functions by symbol, so `f` is the *name* of a script function and `deps` is a
list of signal names. The function receives the dep values in order, and its
return value lands in `name`:

```candela
fn calc_label(n) { return n + " checks logged"; }

fn on_start() {
    lumen::derive("total_label", ["total"], "calc_label");
}
```

A derivation runs once after registration even when no dep has changed yet, so
the bound label is correct on the first frame.

## The dynamic DOM API

Beyond writing signals, a script can reach into the live element tree and change
it: find nodes with selectors, walk between them, spawn and move and remove
elements, edit classes and inline style, and bind event listeners with capture
and bubble phases. It is the DOM model, with the same vocabulary.

### Node handles

A node is an `int` handle. `0` means "no node", which is what every lookup
returns on a miss, so a guard is one comparison:

```candela
let list = lumen::node_get_by_id("note-list");
if list == 0 { return; }
```

`lumen::node_valid(node)` reports whether a handle is still present in the
current snapshot. Handles come from the tick's snapshot of the tree, so resolve
them inside the handler that uses them rather than caching them across ticks.

Handles minted by `lumen::node_spawn` and `lumen::node_clone_deep` are reserved
tokens: the element does not exist yet, and the runtime materializes it when it
drains the tick's commands. You can pass such a token straight into further
calls in the same handler (append it, set its text, add a class); it stops being
usable once the tick ends.

### Finding nodes

| Call | Returns |
|---|---|
| `lumen::node_get_by_id(id)` | The element with that `id`, or `0`. |
| `lumen::node_query(selector)` | Every match, in document order. |
| `lumen::node_closest(node, selector)` | Nearest ancestor-or-self match, or `0`. |
| `lumen::node_document()` | The root element. |
| `document::get_by_id(id)`, `document::query(selector)`, `document::root()` | The same three entry points under the `document` namespace. |
| `document::focused()`, `document::hovered()` | The focused / hovered element, or `0`. |

Selectors use the same grammar as your stylesheet: tag, `#id`, `.class`,
compounds, the descendant and `>` child combinators, and `:is()` / `:where()` /
`:not()` / `:nth-child()` with the other structural pseudo-classes. Queries
match against a snapshot of tag, id, class, and sibling position, so state
pseudo-classes (`:hover`, `:focus`, `:checked`) and the sibling combinators
(`+`, `~`) never match there. An unparseable selector yields an empty result.

```candela
fn on_ready() {
    let rows = lumen::node_query(".track");
    for r in rows {
        lumen::node_class_remove(r, "track-now");
    }
}
```

### Traversal

`lumen::node_parent`, `node_first_child`, `node_last_child`, `node_next`, and
`node_prev` each take a node and return a node or `0`.
`lumen::node_children(node)` returns the child list in document order.

```candela
fn clear_children(container) {
    let kids = lumen::node_children(container);
    for k in kids {
        lumen::node_remove(k);
    }
}
```

### Mutation

Build elements the way `createElement` and `appendChild` do: spawn a node, set
what it carries, attach it.

```candela
fn add_row(container, title, artist) {
    let row = lumen::node_spawn("row");
    lumen::node_set_attr(row, "class", "track");
    lumen::node_append(container, row);

    lm_append(row, "label", "td grow", title);
    lm_append(row, "label", "td artist", artist);
}
```

`lm_append(parent, tag, cls, text)` comes from the prelude. It spawns `tag`,
gives it a class and text, appends it under `parent`, and returns the new node,
so a row cell is one call instead of four. Pass `""` for `cls` or `text` to skip
that step.

Structure: `node_append(parent, child)`, `node_insert_before(parent, child,
reference)`, `node_set_parent(node, parent)`, `node_move_to(node, parent)`,
`node_replace_with(old, new)`, and `node_remove(node)`. Moving an
already-attached node reparents it; there is no detach step.

Content: `node_set_text(node, text)` for the text of a label or button,
`node_set_attr(node, name, value)` for anything markup can carry, and
`node_remove_attr`. `id`, `class`, `text`, and `disabled` route to the same
places the parser writes them, and a `class` value splits on whitespace, so
`node_set_attr(n, "class", "td num")` lands two classes where
`node_class_add(n, "td num")` would store one.

Classes and inline style have their own verbs: `node_class_add`,
`node_class_remove`, `node_class_toggle`, `node_set_class` (replace the whole
list), `node_set_style(node, prop, value)`, and `node_style_remove(node, prop)`.

`node_set_inner_markup(node, markup)` parses a markup fragment and replaces the
node's children with it. It needs the markup parser, which a precompiled app
does not carry, so it does nothing when the app runs from a compiled artifact.
Build lists element by element (`node_spawn` / `lm_append`) when the same code
has to work on both paths.

Layout attributes on a spawned element are not applied from the script side;
give the element a class and let CSS carry width, growth, and spacing.

### Events

`lumen::event_on(node, type, handler)` binds a listener and returns a token.
The handler is a named function that receives an event id, and the `event_*`
accessors read the current event through that id:

```candela
fn on_ready() {
    let btn = lumen::node_get_by_id("save");
    lumen::event_on(btn, "click", "clicked");
}

fn clicked(ev) {
    let target = lumen::event_target(ev);
    lumen::node_class_toggle(target, "pressed");
    lumen::event_prevent_default(ev);
}
```

`lumen::event_on_capture` binds the same listener in the capture phase, and
`lumen::event_off(token)` unbinds. Propagation follows the DOM contract: capture
from the root down to the target, dispatch at the target, then bubble back up.
`event_stop_propagation(ev)` stops the walk once the current node's handlers
finish, `event_stop_immediate_propagation(ev)` also skips the rest of that
node's handlers, and `event_prevent_default(ev)` suppresses the built-in action
(anchor navigation on a `click`).

Event types: `click`, `dblclick`, `pointerdown`, `pointerup`, `pointermove`,
`pointerenter`, `pointerleave`, `wheel`, `keydown`, `keyup`, `input`, `change`,
`focus`, `blur`, `submit`, and `scroll`. `focus`, `blur`, `pointerenter`,
`pointerleave`, and `scroll` do not bubble. `input` and `change` fire on commit
rather than per keystroke.

The accessors carry meaning for the event types that produce them:
`event_key(ev)` for keyboard events, `event_value(ev)` for `input` and `change`,
`event_delta_x` / `event_delta_y` for `wheel`, and the position and modifier
accessors for pointer events. `event_current_target(ev)` is the node whose
handler is running, which differs from `event_target(ev)` during capture and
bubble.

### Introspection

`node_is_visible(node)`, `node_z_index(node)`, `node_classes(node)`,
`node_components(node)` (the runtime components attached to the element),
`node_outer_markup(node)` and `node_inner_markup(node)`, and `dump_tree()` (the
whole tree as text) answer "what is on screen right now" questions, which is what
a debug overlay or a test assertion wants.

The geometry and full-style readers (`node_rect`, `node_content_rect`,
`node_scroll`, `node_computed_style_all`, `node_inline_style`, `node_attrs`)
return maps. candela host declarations cover scalar and homogeneous-array
returns, so those are not in the prelude; read one property at a time with
`lumen::node_computed_style(node, prop)`, or reach them from the Rhai, Lua, or
C hosts.

## Builtin reference

Every signature below is what `import "lumen.cdl";` declares. A call with no
return type returns null.

### Signals

| Signature | Meaning |
|---|---|
| `lumen::signal_get(name) -> string` | Read a signal as a string; empty when never written. |
| `lumen::signal_set(name, value)` | Write a string signal. |
| `lumen::signal_get_int(name) -> int` | Read a typed int signal; `0` on a miss or a non-numeric value. |
| `lumen::signal_set_int(name, value)` | Write a typed int signal. |
| `lumen::signal_get_float(name) -> float` | Read a typed float signal; `0.0` on a miss. |
| `lumen::signal_set_float(name, value)` | Write a typed float signal. |
| `lumen::signal_get_bool(name) -> bool` | Read a typed bool signal; `false` on a miss. |
| `lumen::signal_set_bool(name, value)` | Write a typed bool signal. |
| `lumen::derive(name, deps, f)` | Register a computed signal recomputed by the script function named `f` whenever one of the `deps` signals changes. |

### Timers

| Signature | Meaning |
|---|---|
| `lumen::set_timeout(name, ms)` | Fire `on_timer(name)` once after `ms` milliseconds. |
| `lumen::set_interval(name, ms)` | Fire `on_timer(name)` every `ms` milliseconds until cancelled. |
| `lumen::cancel_timer(name)` | Cancel a pending or repeating timer. |

### Notifications, clipboard, tray

| Signature | Meaning |
|---|---|
| `lumen::notify(title, body)` | Show an OS notification. |
| `lumen::copy_image(path)` | Put the PNG at `path` on the system clipboard. |
| `lumen::save_clipboard_image(path)` | Write the current clipboard image to `path` as PNG. |
| `lumen::tray_icon(id, icon_path, tooltip)` | Register or replace a tray icon; clicks fire `on_tray(id)`. An empty tooltip means no tooltip. |
| `lumen::unregister_tray(id)` | Remove a tray icon. |

### Menus

| Signature | Meaning |
|---|---|
| `lumen::open_menu(id)` | Open the `<menu id="...">` popup. |
| `lumen::close_menu(id)` | Close it. |

Both flip the `__menu_open:<id>` signal the popup binds to.

### File dialogs

| Signature | Meaning |
|---|---|
| `lumen::pick_file(tag)` | Native open dialog; fires `on_file_picked(tag, path)`. |
| `lumen::pick_files(tag)` | Multi-select open dialog; fires `on_files_picked(tag, paths)`. |
| `lumen::pick_folder(tag)` | Folder picker; fires `on_folder_picked(tag, path)`. |
| `lumen::save_file(tag, default_name)` | Save dialog seeded with `default_name`; fires `on_file_picked(tag, path)`. |
| `lumen::pick_file_filtered(tag, spec)` | Open dialog with a filter list. `spec` is pipe-separated `label:ext,ext` groups, and `*` means no filter. |

Cancelling fires the callback once with an empty path, so a script can clear its
modal state.

### Hotkeys

| Signature | Meaning |
|---|---|
| `lumen::register_hotkey(name, accelerator)` | Register an OS-level global hotkey (`"CommandOrControl+S"`); fires `on_hotkey(name)`. |
| `lumen::unregister_hotkey(name)` | Remove it. |

### DOM query and traversal

| Signature | Meaning |
|---|---|
| `lumen::node_query(selector) -> int[]` | Matching nodes in document order. |
| `lumen::node_get_by_id(id) -> int` | The node with that id, or `0`. |
| `lumen::node_document() -> int` | The document root. |
| `lumen::node_parent(node) -> int` | Parent, or `0`. |
| `lumen::node_first_child(node) -> int` | First child, or `0`. |
| `lumen::node_last_child(node) -> int` | Last child, or `0`. |
| `lumen::node_next(node) -> int` | Next sibling, or `0`. |
| `lumen::node_prev(node) -> int` | Previous sibling, or `0`. |
| `lumen::node_children(node) -> int[]` | Children in document order. |
| `lumen::node_closest(node, selector) -> int` | Nearest ancestor-or-self match, or `0`. |
| `lumen::node_valid(node) -> bool` | Whether the handle is in the current snapshot. |

### DOM mutation

| Signature | Meaning |
|---|---|
| `lumen::node_spawn(tag) -> int` | Create an element; returns a handle valid for this tick. |
| `lumen::node_clone_deep(node) -> int` | Deep-copy a subtree; returns the clone root. |
| `lumen::node_append(parent, child)` | Append `child` as the last child of `parent`. |
| `lumen::node_insert_before(parent, child, reference)` | Insert `child` before `reference` under `parent`; a `0` reference appends. |
| `lumen::node_set_parent(node, parent)` | Reparent `node`. |
| `lumen::node_move_to(node, parent)` | Move `node` under `parent`. |
| `lumen::node_replace_with(old, new)` | Put `new` where `old` was and despawn `old`. |
| `lumen::node_remove(node)` | Despawn the node and its subtree. |
| `lumen::node_set_attr(node, name, value)` | Set an attribute. `id`, `class`, `text`, and `disabled` route to their components; a `class` value splits on whitespace. |
| `lumen::node_remove_attr(node, name)` | Remove an attribute. |
| `lumen::node_set_id(node, id)` | Set the element id. |
| `lumen::node_set_text(node, text)` | Replace the element's text content. |
| `lumen::node_set_inner_markup(node, markup)` | Parse a markup fragment and replace the children with it. No effect when the app runs from a compiled artifact. |
| `lumen::node_class_add(node, class)` | Add one class. |
| `lumen::node_class_remove(node, class)` | Remove one class. |
| `lumen::node_class_toggle(node, class)` | Toggle one class. |
| `lumen::node_set_class(node, classes)` | Replace the whole class list. |
| `lumen::node_set_style(node, prop, value)` | Set an inline style property. |
| `lumen::node_style_remove(node, prop)` | Remove an inline style property. |

### DOM read-back

| Signature | Meaning |
|---|---|
| `lumen::node_get_attr(node, name) -> string` | Attribute value; empty when absent. |
| `lumen::node_text(node) -> string` | Text content. |
| `lumen::node_id(node) -> string` | Element id. |
| `lumen::node_class_contains(node, class) -> bool` | Whether the class is present. |
| `lumen::node_style_get(node, prop) -> string` | Inline style value. |
| `lumen::node_computed_style(node, prop) -> string` | Value after the cascade. |

### Introspection

| Signature | Meaning |
|---|---|
| `lumen::node_is_visible(node) -> bool` | Whether the node renders. |
| `lumen::node_z_index(node) -> int` | Paint order index. |
| `lumen::node_classes(node) -> string[]` | Class list. |
| `lumen::node_components(node) -> string[]` | Runtime components on the element. |
| `lumen::node_outer_markup(node) -> string` | The node and its subtree as markup. |
| `lumen::node_inner_markup(node) -> string` | Its children as markup. |
| `lumen::dump_tree() -> string` | The whole tree as text. |

### Events

| Signature | Meaning |
|---|---|
| `lumen::event_on(node, type, handler) -> int` | Bind a bubble-phase listener; returns the off token. |
| `lumen::event_on_capture(node, type, handler) -> int` | Bind a capture-phase listener. |
| `lumen::event_off(token)` | Unbind. |
| `lumen::event_target(ev) -> int` | The node the event targeted. |
| `lumen::event_current_target(ev) -> int` | The node whose handler is running. |
| `lumen::event_type(ev) -> string` | Event type name. |
| `lumen::event_key(ev) -> string` | Logical key (`"a"`, `"Enter"`, `"ArrowLeft"`). |
| `lumen::event_value(ev) -> string` | Text value for `input` and `change`. |
| `lumen::event_button(ev) -> int` | `0` primary, `1` middle, `2` secondary, `-1` none. |
| `lumen::event_x(ev) -> float`, `lumen::event_y(ev) -> float` | Pointer position relative to the target. |
| `lumen::event_client_x(ev) -> float`, `lumen::event_client_y(ev) -> float` | Pointer position in window coordinates. |
| `lumen::event_delta_x(ev) -> float`, `lumen::event_delta_y(ev) -> float` | Wheel delta. |
| `lumen::event_shift(ev) -> bool`, `lumen::event_ctrl(ev) -> bool`, `lumen::event_alt(ev) -> bool`, `lumen::event_super(ev) -> bool` | Modifier state. |
| `lumen::event_prevent_default(ev)` | Suppress the built-in action. |
| `lumen::event_stop_propagation(ev)` | Stop the walk after this node. |
| `lumen::event_stop_immediate_propagation(ev)` | Also skip this node's remaining handlers. |

### Classes by id

| Signature | Meaning |
|---|---|
| `lumen::set_class(id, classes)` | Replace the class list on the element with that id. |
| `lumen::set_root_class(classes)` | Replace the class list on `<root>`. Drives theme switching. |

### Networking

| Signature | Meaning |
|---|---|
| `lumen::fetch(url, tag)` | HTTP GET on a worker thread; fires `on_fetch(tag, body)` or `on_fetch_error(tag, message)`. |

Parse a response with candela's own `json_parse`, which reads runtime strings
and hands back a value you unwrap with `as_map` / `as_list` / `as_str`.

### Filesystem

| Signature | Meaning |
|---|---|
| `lumen::read_file(path) -> string` | Read a file; empty string on error. |
| `lumen::write_file(path, contents) -> bool` | Write a file; `true` on success. |

### Handler routing

| Signature | Meaning |
|---|---|
| `lumen::on(event, id, handler)` | Route `event` on element `id` to the script function named `handler`. |

### Audio

| Signature | Meaning |
|---|---|
| `lumen::audio_play(path)` | Load and play a track (app-relative wav / ogg) from position 0. |
| `lumen::audio_pause()` | Pause, holding position. |
| `lumen::audio_resume()` | Resume a paused track. |
| `lumen::audio_stop()` | Stop and rewind. |
| `lumen::audio_seek(secs)` | Seek, clamped to the track duration. |
| `lumen::audio_volume(level)` | Output volume from `0.0` to `1.0`. |

The transport writes its position, duration, and playing state back as signals,
which markup reads with `bind-*`.

### Window, history, document

| Signature | Meaning |
|---|---|
| `window::set_href(path)` | Navigate to a page. |
| `window::href() -> string` | Current page path. |
| `window::reload()` | Reload the current page. |
| `window::title() -> string` | Window title. |
| `window::set_title(title)` | Set the window title. |
| `window::set_size(width, height)` | Resize the window. |
| `window::dpr() -> float` | Device pixel ratio. |
| `window::location_path() -> string` | Path part of the current location. |
| `window::location_query() -> string`, `window::location_hash() -> string` | Query and hash. Only the path is tracked today, so both return an empty string. |
| `history::back()`, `history::forward()` | Move through the in-memory history stack. |
| `history::go(delta)` | Move `delta` entries; a negative delta goes back. |
| `document::root() -> int` | The root element. |
| `document::query(selector) -> int[]` | Selector query. |
| `document::get_by_id(id) -> int` | Id lookup. |
| `document::focused() -> int`, `document::hovered() -> int` | Focused / hovered element, or `0`. |
| `document::spawn(tag) -> int` | Create an element, like `lumen::node_spawn`. |

### Prelude helper

| Signature | Meaning |
|---|---|
| `lm_append(parent, tag, cls, text) -> int` | Spawn `tag`, apply `cls` and `text` when non-empty, append under `parent`, and return the new node. |

### Id-addressed commands

| Signature | Meaning |
|---|---|
| `lumen::set_text(id, text)` | Replace the text content of the element with that id. |
| `lumen::set_src(id, path)` | Swap an `<image>` asset at runtime. The path resolves against the app dir and any configured asset root. |
| `lumen::add_clicks(n)`, `lumen::set_string(key, value)` | Emit an app command the runtime does not interpret. Useful only with a host extension that consumes it. |

## The Rhai and Lua hosts

The same surface is available to Rhai (`.rhai`) and Lua (`.lua`) scripts, which
stay supported for existing apps. The difference is spelling: those hosts expose
the builtins as flat global functions, so candela's `lumen::signal_set` is
`signal_set` there. Both also carry a few conveniences candela reaches
differently: the `signal(name, default)` and `signal_array(name)` handle objects
(candela uses the typed `signal_get` / `signal_set` pairs, and builds lists by
spawning nodes), `parse_json` (candela has `json_parse` in the language),
`local_id`, and the `page(path)` navigation call (`window::set_href` in
candela).

Pick the host in `lumen.toml`:

```toml
[script]
engine = "rhai"   # "candela" | "rhai" | "lua"
```

When `[script] engine` is absent the runtime uses Rhai. See
[Per-app config](./lumen-toml.md#script) for the details, including how a
bundled build infers the host from the app's script files.

Rhai editors treat the injected builtins as unknown functions; pointing your
LSP at
[`docs/lumen-rhai-builtins.rhai`](https://github.com/lumen-fx/lumen/blob/main/docs/lumen-rhai-builtins.rhai)
silences the warnings.
