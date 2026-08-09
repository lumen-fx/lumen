# Candela scripting reference

candela is a script language embedded in Lumen. A candela script reads and
writes signals, builds and mutates the live element tree, and responds to
input, timers, file dialogs, hotkeys, and network replies. This page lists
every function a candela script can call into Lumen, the lifecycle functions
Lumen calls back into the script, and the dynamic DOM API.

## Attaching a script to an app

An app picks its script language with `[script] engine` in `lumen.toml`:

```toml
[script]
engine = "lua"   # "candela" (default) | "rhai" | "lua"
```

`engine` is optional. When it is absent, `lumenc run`/`build` and `lumenc
check` both infer the host from the app directory's script file extensions:
a `.cdl` file selects candela, a `.lua` file selects lua, a `.rhai` file
selects rhai. A directory carrying more than one extension resolves by a
fixed precedence, candela first, then lua, then rhai, so the answer never
depends on directory-listing order. A directory with no script file at all
(a script kept entirely inline in `<script>...</script>` markup) also
resolves to candela, since candela is the default language, not rhai; an
inline script written in lua or rhai still needs an explicit `engine` line,
since there is no file extension for inference to read. An explicit
`[script] engine` always wins over inference. A `lumenc bundle --static` build
resolves the host the same way; that choice also decides which single
script host gets compiled into the binary.

The script body itself is wired from markup, not from the engine setting:

```html
<script src="main.cdl" />
```

placed inside `main.lmn` (or an inline `<script>...</script>` body). The
compiled source is every inline `<script>` body concatenated in source
order, followed by every `<script src="...">` file's contents, also
concatenated in source order.

## Importing the Lumen host surface

candela reaches every Lumen-provided function through a typed
`host "<namespace>" { ... }` block that declares each function's name,
parameter types, and return type. A script only compiles a call to
`lumen::signal_set(...)` if `signal_set` was declared in a `host "lumen"`
block first, hand-written or otherwise.

Writing that block by hand is one option:

```candela
host "lumen" {
    string signal_get(string);
    signal_set(string, string);
    on(string, string, string);
}
```

The common path is the bundled prelude, which declares the entire surface in
one line:

```candela
import "lumen.cdl";
```

Lumen detects that exact statement (the bare form only; `import "lumen.cdl"
as x;` is not recognized) and splices in a full `host "lumen" { ... }` block
plus three more namespace blocks before the source reaches the compiler. The
import introduces four namespaces:

| Namespace | Reached as | Covers |
|---|---|---|
| `lumen` | `lumen::name(...)` | signals, timers, notifications, dialogs, hotkeys, the dynamic DOM, events, audio, classes, networking, filesystem |
| `window` | `window::name(...)` | window title/size, href, reload, dpr |
| `history` | `history::name(...)` | back/forward/go |
| `document` | `document::name(...)` | root/query/get_by_id/focused/hovered/spawn |

On top of those namespaces the prelude defines a handful of plain candela
functions and two wrapper types, so the DOM reads as method calls. Those
are `Node` and `Event`, the constructors `node`, `event`, and `wrap_nodes`,
the entry points `spawn`, `get_by_id`, `document_node`, and `query`, and
the older list helper `lm_append`. Each is covered in
[The dynamic DOM API](#the-dynamic-dom-api) below.

Without the import, and without a hand-written `host` block, the source
still compiles (candela resolves host functions lazily), but calling an
undeclared `lumen::...` function fails at runtime with "lumen is not a valid
namespace". Most builtins below are scalar-typed at the candela boundary: an
`int` is an i64, arrays are homogeneous (`int[]`, `string[]`), and a bare
`name(args);` call with no leading type returns `()` (null). A handful of
low-level introspection readers return a homogeneous `{string: T}` map
instead; see "Map-returning introspection readers" further down. There is
still no mixed-type map or any-shaped return value, which is why `http`
(a request object) and `parse_json` (an any-shaped result) have no
candela builtin.

## `lumen` namespace

### App state and typed signals

Signals are the reactive store `bind-*` markup attributes read from.
`signal_get`/`signal_set` work on plain strings; the typed pairs parse and
format for you, defaulting to zero/false on a miss or unparseable value.

| Signature | Description |
|---|---|
| `add_clicks(int)` | Increment the app's click counter by the given amount. |
| `set_string(string key, string value)` | Set an app-side string key to `value`. |
| `set_text(string target_id, string text)` | Replace the text content of the element with id `target_id`. Mostly superseded by `bind-text` + `signal_set`. |
| `set_src(string target_id, string path)` | Swap the asset path of the `<image id=target_id>` at runtime (app-relative path). |
| `string signal_get(string name)` | Read a signal as a string; empty string if never written. |
| `signal_set(string name, string value)` | Write a signal to a string value. |
| `int signal_get_int(string name)` | Read a signal as i64; `0` on miss or non-numeric value. |
| `signal_set_int(string name, int value)` | Write a signal as i64. |
| `float signal_get_float(string name)` | Read a signal as f64; `0.0` on miss or non-numeric value. |
| `signal_set_float(string name, float value)` | Write a signal as f64. |
| `bool signal_get_bool(string name)` | Read a signal as bool; `false` on miss or unparseable value. |
| `signal_set_bool(string name, bool value)` | Write a signal as bool. |

Use this when a value drives a `bind-text` / `bind-value` / `bind-checked`
attribute in markup, or needs to survive across handler calls (every event
handler is a fresh call into the program; signals are the state that
persists between calls).

**Seed a signal with the setter whose type you intend to read.** A signal
remembers the type it was written with, and the per-tick sync from the
markup side only parses an incoming string back into that same type. Write
`lumen::signal_set_float("audio_position", 0.0)` in `on_start` and later
host writes parse back as floats; leave it unseeded and the first write
lands as a string, so `signal_get_float` reads it through a string parse
instead. The music example seeds its transport signals exactly this way.

### Classes

| Signature | Description |
|---|---|
| `set_class(string id, string classes)` | Replace the CSS classes on the element with id `id`. |
| `set_root_class(string classes)` | Replace the CSS classes on the `<root>` element; the usual way to drive a theme-token switch. |

```candela
lumen::set_root_class("app theme-dark");
```

### Timers

| Signature | Description |
|---|---|
| `set_timeout(string name, int ms)` | Schedule a one-shot timer; fires `on_timer(name)` after `ms` milliseconds. |
| `set_interval(string name, int ms)` | Schedule a repeating timer; fires `on_timer(name)` every `ms` milliseconds. |
| `cancel_timer(string name)` | Cancel a timer previously created with `set_timeout`/`set_interval`. |

### Notifications, clipboard, tray

| Signature | Description |
|---|---|
| `notify(string title, string body)` | Show an OS notification. |
| `copy_image(string path)` | Copy the image at `path` (app-relative) to the system clipboard. |
| `save_clipboard_image(string path)` | Write the current clipboard image to `path` as PNG. |
| `tray_icon(string id, string icon_path, string tooltip)` | Register or replace a system tray icon; clicks fire `on_tray(id)`. An empty tooltip disables the icon rather than clearing the tooltip text. |
| `unregister_tray(string id)` | Remove a previously registered tray icon. |

### Menus

| Signature | Description |
|---|---|
| `open_menu(string id)` | Open the menu `id` (sets the `__menu_open:<id>` signal to true). |
| `close_menu(string id)` | Close the menu `id` (sets the `__menu_open:<id>` signal to false). |

### File dialogs

Every dialog call returns immediately; the result arrives later through the
`on_file_picked` / `on_files_picked` / `on_folder_picked` lifecycle
functions, keyed by the `tag` you pass in.

| Signature | Description |
|---|---|
| `pick_file(string tag)` | Open a native open-file dialog. |
| `pick_files(string tag)` | Open a native multi-select dialog. |
| `pick_folder(string tag)` | Open a native folder-picker dialog. |
| `save_file(string tag, string default_name)` | Open a native save-file dialog seeded with `default_name`. |
| `pick_file_filtered(string tag, string spec)` | Open a filtered open-file dialog. `spec` is `"Label:ext1,ext2\|All:*"`. |

```candela
fn handle_pick(id) {
    lumen::pick_file("garden-open");
}

fn on_file_picked(tag, path) {
    if path == "" {
        lumen::signal_set("picked", "(cancelled)");
    } else {
        lumen::signal_set("picked", "picked: " + path);
    }
}
```

### Hotkeys

| Signature | Description |
|---|---|
| `register_hotkey(string name, string accelerator)` | Register a global OS hotkey (for example `"CommandOrControl+Shift+L"`); fires `on_hotkey(name)`. |
| `unregister_hotkey(string name)` | Remove a previously registered global hotkey. |

### Networking

| Signature | Description |
|---|---|
| `fetch(string url, string tag)` | Issue an HTTP GET. The reply fires `on_fetch(tag, body)` on a 2xx response or `on_fetch_error(tag, message)` otherwise. |

candela has no `http(request)` builtin (the request would need a map with
mixed-typed fields, which cannot cross the scalar host boundary), so
`on_http(tag, response)` never fires for a candela script; use `fetch` for
GET requests.

### Translation

| Signature | Description |
|---|---|
| `string t(string key)` | Translate `key` in the app's active locale; returns `key` itself when no catalogue carries it. See [Translation](../authoring/i18n.md). |
| `string tr(string key)` | Alias for `t(key)`, under Qt's name. |

### Filesystem

| Signature | Description |
|---|---|
| `string read_file(string path)` | Read a file to a string; empty string on error. |
| `bool write_file(string path, string contents)` | Write `contents` to `path`; returns true on success. |

### Per-id event routing

| Signature | Description |
|---|---|
| `on(string event, string id, string handler)` | Route `event` on element `id` to the script function named `handler`. |

`on` is how you attach a specific handler function to a specific element's
click, toggle, slider, drop, hotkey, tray, menu, or dialog event instead of
handling every occurrence in the matching global `on_*` function. See
"Lifecycle" below for the full event/fallback table.

```candela
lumen::on("click", "new-note", "new_note");
lumen::on("click", "theme-toggle", "toggle_theme");
```

A key also matches a handler registered for its suffix after the last `:`,
so a handler registered for `"save"` also matches a templated id like
`"user-card:save"`.

### Derived signals

| Signature | Description |
|---|---|
| `derive(string name, string[] deps, string f)` | Register a computed signal `name`, recomputed by the script function named `f` whenever any signal in `deps` changes. `f` receives the current value of each dep as a positional argument, in `deps` order. |

candela has no first-class closure value, so the recompute body is passed as
the *name* of a function rather than a closure literal.

```candela
fn calc_greeting(s) {
    if s == "" { return "(nobody yet)"; }
    return "hi, " + s + "!";
}

fn on_start() {
    lumen::signal_set("who", "");
    lumen::derive("greeting", ["who"], "calc_greeting");
}
```

A derivation with an empty `deps` array fires once, on the next tick, and
never again. A `derive` call also marks the signal pending-initial, so its
function runs on the first tick after registration even if no dep has
changed yet.

### Audio transport

| Signature | Description |
|---|---|
| `audio_play(string path)` | Load and play the audio track at `path` (app-relative wav/ogg); resets position to 0. |
| `audio_pause()` | Pause the transport, holding its position. |
| `audio_resume()` | Resume a paused transport. |
| `audio_stop()` | Stop the transport and rewind to 0. |
| `audio_seek(float secs)` | Seek to `secs` seconds, clamped to the track duration. |
| `audio_volume(float level)` | Set output volume, `0.0..=1.0`. |

Playback position, duration, and playing state are not builtins; read them
back through host-written signals bound with `bind-*` in markup.

## The dynamic DOM API

candela reaches the same live element tree the other script hosts and the
C ABI do. A node is a candela `int` handle, and `0` always means "no
node". Handles from `node_spawn` / `node_clone_deep` are valid for the
rest of the current tick, so you can append to a node you just spawned.

Selectors passed to `node_query` / `node_closest` / `document::query` use
the same CSS selector syntax as `main.css` (see
[CSS](../authoring/css.md)); the query walks the current per-tick DOM
snapshot and reuses the markup cascade's selector matcher.

### Node and Event: the method form

Every function below is declared in the `host "lumen"` block as a free
function over a raw `int` handle (`lumen::node_set_text(n, t)`), because a
`host` block can only declare functions, not methods. The bundled prelude
wraps that surface in two one-field structs, `Node { handle: int }` and
`Event { handle: int }`, plus `impl` blocks that unwrap `self.handle` and
call the matching free function, so a script written against the prelude
reads as method calls instead:

```candela
let col = spawn("column");
col.set_attr("class", cls);
title.set_text(text);
container.append(col);
```

The field is named `handle`, not `id`, on purpose: `.id()` stays free to
mean the element's `id` attribute (`node_id`), matching the DOM property of
the same name.

Mutator methods return nothing, so they do not chain:
`n.set_text("x").class_add("y")` does not compile. Write one call per
statement. The methods that hand back another node (`.parent()`,
`.next()`, `.closest(...)`, `.clone_deep()`, ...) do chain, and they stay
wrapped, so `container.first_child().next()` is still a `Node`.

Every free function that used to hand back a raw node id (`node_spawn`,
`node_get_by_id`, `node_document`, `node_parent`, ...) still works exactly
as declared, and the raw `lumen::node_*` / `lumen::event_*` calls are still
declared in the prelude; the wrapping is additive, not a replacement. Reach
for the entry points below to get a `Node` in the first place:

| Function | Description |
|---|---|
| `node(handle)` | Wraps a raw node handle (for example the return of `lumen::node_spawn(...)`) as a `Node`. |
| `event(handle)` | Wraps a raw event id as an `Event`. |
| `wrap_nodes(handles)` | Wraps every handle in an `int[]` as a `Node`, preserving order. |
| `spawn(tag)` | `node(lumen::node_spawn(tag))`: creates a detached element and returns it wrapped. |
| `get_by_id(id)` | `node(lumen::node_get_by_id(id))`: looks up an element by its `id` attribute. Returns a `Node` with handle `0` when nothing matches. |
| `document_node()` | The document root, wrapped. |
| `query(selector)` | Every element matching a CSS selector, wrapped. |

These are plain candela functions spliced in by the prelude, not
`lumen::`-namespaced host calls, so they compile with or without the
`import "lumen.cdl";` line reaching them (as long as something declares the
underlying `host "lumen"` block).

A `Node` is a struct, not an `int`, so it cannot be compared against a bare
integer the way the old raw-handle code did (`handle == 0`). Use `.exists()`
instead:

```candela
let container = get_by_id("todos-list");
if !container.exists() { return; }
```

`.valid()` differs from `.exists()`: `.exists()` only checks that the
handle is non-zero, while `.valid()` asks the host whether that handle is
still present in the current tick's snapshot (a handle can go stale if its
node was removed).

The tables below list every free function `lumen.cdl` declares, each with
the `Node`/`Event` method that calls it, where one exists. A handful of
functions, mostly ones with no natural receiver (`node_query`, `dump_tree`)
or the eleven map-returning introspection readers described further down,
have no method form yet and are still called as `lumen::function(...)`.

### Read side: lookup and traversal

| Signature | Method | Description |
|---|---|---|
| `int[] node_query(string selector)` | (see `query(selector)` above) | Run a CSS selector; returns matching node ids in document order. |
| `int node_get_by_id(string id)` | (see `get_by_id(id)` above) | Fast id lookup; returns the node id or `0`. |
| `int node_document()` | (see `document_node()` above) | The document root node id. |
| `int node_parent(int node)` | `.parent()` | Parent node id, or `0`. |
| `int node_first_child(int node)` | `.first_child()` | First child node id, or `0`. |
| `int node_last_child(int node)` | `.last_child()` | Last child node id, or `0`. |
| `int node_next(int node)` | `.next()` | Next sibling node id, or `0`. |
| `int node_prev(int node)` | `.prev()` | Previous sibling node id, or `0`. |
| `int[] node_children(int node)` | `.children()` | Child node ids, in document order. |
| `int node_closest(int node, string selector)` | `.closest(selector)` | Nearest ancestor-or-self matching `selector`; node id or `0`. |
| `bool node_valid(int node)` | `.valid()` | Whether the id is present in the current snapshot. |

### Write side: build and mutate

| Signature | Method | Description |
|---|---|---|
| `int node_spawn(string tag)` | (see `spawn(tag)` above) | Create a new element of `tag` (the same tag vocabulary as `.lmn` markup); returns its node id. |
| `int node_clone_deep(int node)` | `.clone_deep()` | Deep-clone a node and its subtree; returns the new root's id. |
| `node_set_attr(int node, string name, string value)` | `.set_attr(name, value)` | Set an attribute. `"id"`, `"class"` (whitespace-split into the class list), `"text"`, and `"disabled"` are recognized specially; any other name goes into the generic attribute bag. |
| `node_remove_attr(int node, string name)` | `.remove_attr(name)` | Remove an attribute. |
| `node_set_id(int node, string id)` | `.set_id(id)` | Set the node's `id` attribute. |
| `node_set_text(int node, string text)` | `.set_text(text)` | Replace the node's text content. |
| `node_set_inner_markup(int node, string markup)` | `.set_inner_markup(markup)` | Replace the node's children by parsing `markup` as `.lmn`-like source. Only meaningful when the app runs from source; a precompiled (AOT) app has no markup parser available, so this is a no-op there. Do not feed it untrusted content. |
| `node_class_add(int node, string class)` | `.class_add(class)` | Add one class. |
| `node_class_remove(int node, string class)` | `.class_remove(class)` | Remove one class. |
| `node_class_toggle(int node, string class)` | `.class_toggle(class)` | Toggle one class. |
| `node_set_class(int node, string classes)` | `.set_class(classes)` | Replace the whole class list (equivalent to `node_set_attr(node, "class", classes)`). |
| `node_set_style(int node, string name, string value)` | `.set_style(name, value)` | Set one inline style property. |
| `node_style_remove(int node, string name)` | `.style_remove(name)` | Remove one inline style property. |
| `node_remove(int node)` | `.remove()` | Remove the node (and its subtree). |
| `node_append(int parent, int child)` | `.append(child)` | Append `child` as `parent`'s last child. The method form takes a `Node`, not a raw id (`parent.append(child)`). |
| `node_insert_before(int parent, int child, int reference)` | `.insert_before(new_node, ref_node)` | Insert `child` under `parent`, immediately before `reference`. The method form takes `Node`s for both arguments. |
| `node_set_parent(int node, int parent)` | `.set_parent(parent)` | Reparent `node` under `parent`, appended last. |
| `node_move_to(int node, int parent)` | `.move_to(new_parent)` | Same as `node_set_parent`; reads better when the intent is "move", not "attach for the first time". |
| `node_replace_with(int old, int new)` | `.replace_with(other)` | Replace `old` with `new` in the tree. |

Because a precompiled app has no source markup parser, the pattern used in
both example apps is to build lists element by element instead of relying
on `node_set_inner_markup`:

```candela
fn add_cell(row, cls, text) {
    let cell = spawn("label");
    cell.set_attr("class", cls);
    cell.set_text(text);
    row.append(cell);
}

fn add_todo_row(container, idx, label, status) {
    let row = spawn("row");
    row.set_attr("class", "list-row");
    container.append(row);
    add_cell(row, "li-idx", idx);
    add_cell(row, "li-label", label);
    add_cell(row, "pill", status);
}
```

The prelude also ships `lm_append(parent, tag, cls, text)`, an older helper
for the same shape that predates the `Node` method sugar. It spawns a `tag`
element, adds `cls` as a class (skip with `""`), sets `text` (skip with
`""`), appends it under `parent`, and returns the new node's id:

```candela
fn lm_append(parent, tag, cls, text) { ... }   // in lumen.cdl

let row_id = lm_append(root_id, "row", "track", "Cipher");
```

Unlike `spawn`/`.append(...)`, `lm_append` takes and returns raw `int`
handles, not `Node`s; call it with `.handle` off a wrapped node
(`lm_append(root.handle, ...)`) if the rest of the script works in the
wrapped form, or wrap its return with `node(...)` to keep going in method
style. Neither example app calls it today; both build rows with
`spawn`/`.set_attr`/`.set_text`/`.append` instead.

### Read-backs on a single node

| Signature | Method | Description |
|---|---|---|
| `string node_get_attr(int node, string name)` | `.get_attr(name)` | Read an attribute; empty string if unset. |
| `string node_text(int node)` | `.text()` | The node's text content. |
| `string node_id(int node)` | `.id()` | The node's `id` attribute. |
| `bool node_class_contains(int node, string class)` | `.class_contains(class)` | Whether the node has `class`. |
| `string node_style_get(int node, string name)` | `.style_get(name)` | Read one inline style property. |
| `string node_computed_style(int node, string name)` | `.computed_style(name)` | Read one property from the resolved (cascade-applied) style. |

### Low-level introspection

| Signature | Method | Description |
|---|---|---|
| `bool node_is_visible(int node)` | `.is_visible()` | Whether the node is currently visible (not display:none, not clipped away). |
| `int node_z_index(int node)` | `.z_index()` | The node's resolved stacking z-index. |
| `string[] node_classes(int node)` | `.classes()` | The node's class list. |
| `string[] node_components(int node)` | `.components()` | Names of the ECS components attached to the node's entity, for debugging. |
| `string node_outer_markup(int node)` | `.outer_markup()` | The node and its subtree, serialized as markup. |
| `string node_inner_markup(int node)` | `.inner_markup()` | The node's children, serialized as markup. |
| `string dump_tree()` | (no receiver; call `lumen::dump_tree()`) | The whole document tree, serialized as markup, for debugging. |

### Map-returning introspection readers

A `host` block can declare a map return type (`{key_type: value_type}`)
the same as any other type position, so a further set of Rust-side
introspection readers reaches candela directly as maps, not just the
scalar and homogeneous-array readers above. None of these have a `Node`
method form yet; call them as `lumen::function(node.handle)` (or on a raw
handle directly). Read a value back out of the map with `.get("key")`, the
same method candela's map type uses everywhere:

```candela
let r = lumen::node_rect(n.handle);
let w = r.get("width");
```

| Signature | Return keys | Description |
|---|---|---|
| `{string: float} node_rect(int node)` | `x`, `y`, `width`, `height`, `client_x`, `client_y` | The node's border-box rect after layout. `x`/`y` are local to the parent's origin; `client_x`/`client_y` are window-space, the `getBoundingClientRect`-style read. |
| `{string: float} node_content_rect(int node)` | `x`, `y`, `width`, `height`, `client_x`, `client_y` | Same shape as `node_rect`, for the content box (inside padding and border). |
| `{string: float} node_scroll(int node)` | `x`, `y`, `max_x`, `max_y` | The scroll container's current offsets and their travel limits. |
| `{string: string} node_computed_style_all(int node)` | one entry per resolved CSS property | The full resolved (cascade-applied) style, the same values `node_computed_style` reads one property at a time. |
| `{string: string} node_inline_style(int node)` | one entry per inline property | Only the style properties set directly on the node (markup `style=` or `node_set_style`/`.set_style`), not the rest of the cascade. |
| `{string: string} node_attrs(int node)` | one entry per attribute | The node's raw attribute bag, including `id` and `class`, as plain strings. |
| `{string: int} node_entity_id(int node)` | `index`, `generation` | The ECS entity handle backing the node, for correlating with other introspection tools. |
| `{string: string} node_component(int node, string name)` | one entry per component field | The field map of the ECS component named `name` attached to the node's entity. `name` must be one of the names `.components()` lists for that node; an unrecognized or absent component returns an empty map. |
| `{string: string} pointer_state()` | `x`, `y`, `inside`, `buttons`, `shift`, `ctrl`, `alt`, `super` | Global pointer state: window-space position, whether the pointer is inside the window, the held-button bitmask, and the live modifier keys. Every value, including the numeric ones, comes back as a string. |
| `{string: float} frame_info()` | `frame`, `dt_ms`, `dirty_count` | Per-frame counters: a monotonic frame counter, milliseconds since the previous published frame, and the number of layout-dirty elements this tick. |
| `{string: string} signals_all()` | one entry per signal | Every signal's current name and string value, the same store `signal_get`/`signal_set` read and write individually. |

### Events

Event bindings are procedural too: bind a handler by name, then read the
current event's fields through `event_*` accessors keyed by the event id
the handler receives as its argument. Binding is a `Node` method
(`node.on(...)`); reading an event's fields is an `Event` method, on the
`Event` the handler gets by wrapping its argument with `event(ev)`.

| Signature | Method | Description |
|---|---|---|
| `int event_on(int node, string event_type, string handler)` | `Node.on(event_type, handler)` | Bind `handler` to fire on `node` for `event_type` during the bubble phase; returns an off token. |
| `int event_on_capture(int node, string event_type, string handler)` | `Node.on_capture(event_type, handler)` | Same, but fires during the capture phase (root to target, before bubbling). |
| `event_off(int token)` | `Event.off()` | Unbind a handler previously bound with `event_on`/`event_on_capture`. The token an `on`/`on_capture` bind returns is the same id the handler receives, so a handler can unbind itself. |
| `int event_target(int ev)` | `.target()` | The node the event originally targeted, wrapped. |
| `int event_current_target(int ev)` | `.current_target()` | The node whose handler is currently running, wrapped. |
| `string event_type(int ev)` | `.event_type()` | The event type name (`"click"`, `"keydown"`, `"wheel"`, ...). |
| `string event_key(int ev)` | `.key()` | The logical key for a keyboard event (`"a"`, `"Enter"`, `"ArrowLeft"`). |
| `string event_value(int ev)` | `.value()` | The text value for an `input`/`change` event. |
| `int event_button(int ev)` | `.button()` | Pointer button: `0` primary, `1` secondary, `2` middle, `-1` none. |
| `float event_x(int ev)` | `.x()` | Pointer position relative to the target's top-left, logical pixels. |
| `float event_y(int ev)` | `.y()` | Pointer position relative to the target's top-left, logical pixels. |
| `float event_client_x(int ev)` | `.client_x()` | Pointer position in window (client) coordinates. |
| `float event_client_y(int ev)` | `.client_y()` | Pointer position in window (client) coordinates. |
| `float event_delta_x(int ev)` | `.delta_x()` | Wheel scroll delta. |
| `float event_delta_y(int ev)` | `.delta_y()` | Wheel scroll delta. |
| `bool event_shift(int ev)` | `.shift()` | Shift held. |
| `bool event_ctrl(int ev)` | `.ctrl()` | Control held. |
| `bool event_alt(int ev)` | `.alt()` | Alt/Option held. |
| `bool event_super(int ev)` | `.super_key()` | Super/Cmd/Windows held. The method is named `super_key`, not `super`. |
| `event_prevent_default(int ev)` | `.prevent_default()` | Suppress the event's default handling. |
| `event_stop_propagation(int ev)` | `.stop_propagation()` | Stop the event from reaching the next node on its capture/bubble path. |
| `event_stop_immediate_propagation(int ev)` | `.stop_immediate_propagation()` | Also stop any remaining handlers on the current node. |

Every `event_*` accessor takes an event id, but they all read the event
currently being dispatched rather than the one that id names. The
parameter is there for the web-idiomatic `event_target(ev)` call shape and
to leave room for nested dispatch later; read an event's fields inside its
own handler, and do not stash an id to read later.

```candela
let btn = get_by_id("save-btn");
let off = btn.on("click", "on_save");

fn on_save(ev_id) {
    let ev = event(ev_id);
    let t = ev.target();
    ev.prevent_default();
}
// later, from wherever `off` is in scope: lumen::event_off(off);
```

Prefer `lumen::on("click", "save", "handle_save")` (per-id routing on the
static id from markup) for the common case; reach for `node.on(...)` when
you need to bind against a node built at runtime, need the capture phase,
or need `.prevent_default()`/`.stop_propagation()`.

## `window` namespace

| Signature | Description |
|---|---|
| `window::set_href(string path)` | Navigate to `path`. |
| `string window::href()` | The current navigation path. |
| `window::reload()` | Re-navigate to the current path. |
| `string window::title()` | The current window title. |
| `float window::dpr()` | The window's device pixel ratio. |
| `window::set_title(string title)` | Set the window title. |
| `window::set_size(float width, float height)` | Resize the window, in logical pixels. |
| `string window::location_path()` | Same as `href()`. |
| `string window::location_query()` | Always empty; the query string is not tracked. |
| `string window::location_hash()` | Always empty; the hash is not tracked. |

candela has no nested-namespace value, so `window.location.path` becomes the
flat `window::location_path()` instead of a `location` sub-object.

## `history` namespace

| Signature | Description |
|---|---|
| `history::back()` | Navigate one step back. |
| `history::forward()` | Navigate one step forward. |
| `history::go(int delta)` | Navigate `\|delta\|` steps back (negative) or forward (positive). |

## `document` namespace

| Signature | Description |
|---|---|
| `int document::root()` | The document root node id. |
| `int[] document::query(string selector)` | Same as `lumen::node_query`. |
| `int document::get_by_id(string id)` | Same as `lumen::node_get_by_id`. |
| `int document::focused()` | The currently focused node, or `0`. |
| `int document::hovered()` | The currently hovered node, or `0`. |
| `int document::spawn(string tag)` | Same as `lumen::node_spawn`. |

## Lifecycle

Lifecycle functions are not builtins; they are **functions you write** that
the runtime calls by name, on the schedule below. None of them need a host
declaration, since the call direction is Lumen into the script, not the
script into a host function.

| Function | Fires when | Arguments | `on(...)` event name |
|---|---|---|---|
| `on_start()` | Once, at app construction, before the first tick and before the DOM is queryable. Register signals and `on(...)` routes here. | none | - |
| `on_ready()` | Once, on the first tick, after the DOM index is published. Build your initial dynamic-DOM tree here instead of `on_start`, since node queries only see the mounted static tree from this point on. | none | - |
| `on_click(id)` | A pointer click, unless a per-id route or a same-tick double-click claims it. | `id: string` | `"click"` |
| `on_double_click(id)` | A double-click. Suppresses the trailing `on_click` for the same element on the same tick. | `id: string` | `"double_click"` |
| `on_long_press(id)` | A long-press gesture. | `id: string` | `"long_press"` |
| `on_toggle(id, checked)` | A toggle control's state changes. | `id: string, checked: bool` | `"toggle"` |
| `on_slider(id, value)` | A slider drag commits. | `id: string, value: float` | `"slider"` |
| `on_text_input(id, text)` | An IME commit on an `<input>`/`<textarea>`. | `id: string, text: string` | `"text_input"` |
| `on_file_dropped(id, path)` | A file is dropped onto a `drop="true"` target. | `id: string, path: string` | `"file_dropped"` |
| `on_drop(target_id, payload)` | A drag-and-drop payload is accepted onto `target_id`. `payload` is the source's `drag-payload` attribute, or its id if no payload attribute was set. | `target_id: string, payload: string` | `"drop"` |
| `on_drag_start(source_id, payload)` | A drag gesture starts from `source_id`. | `source_id: string, payload: string` | `"drag_start"` |
| `on_file_picked(tag, path)` | An open/save file dialog closes. Empty `path` means cancelled. | `tag: string, path: string` | `"file_picked"` |
| `on_files_picked(tag, paths)` | A multi-select dialog closes. `paths` is the selected paths joined with `\|`. | `tag: string, paths: string` | `"files_picked"` |
| `on_folder_picked(tag, path)` | A folder-picker dialog closes. | `tag: string, path: string` | `"folder_picked"` |
| `on_fetch(tag, body)` | A `fetch(url, tag)` request completes with a 2xx status. | `tag: string, body: string` | `"fetch"` |
| `on_fetch_error(tag, message)` | A `fetch` request fails, transport error or non-2xx status. | `tag: string, message: string` | `"fetch_error"` |
| `on_timer(name)` | A `set_timeout`/`set_interval` timer fires. | `name: string` | `"timer"` |
| `on_menu(id)` | A `<menubar>`/`<menu>` item is clicked. | `id: string` | `"menu"` |
| `on_hotkey(name)` | A registered global hotkey fires. | `name: string` | `"hotkey"` |
| `on_tray(id)` | A system tray icon is clicked. | `id: string` | `"tray"` |
| `on_dialog_accepted(id)` | A dialog closes accepted. | `id: string` | `"dialog_accepted"` |
| `on_dialog_rejected(id)` | A dialog closes rejected. | `id: string` | `"dialog_rejected"` |
| `on_audio_end()` | The active audio track finishes playing. Fires only when the audio subsystem is compiled in. | none | - |
| `on_close()` | The window is about to close, before the backend tears anything down. Return `false` to veto the close and keep the window open; any other return value (or no `on_close` at all) lets it proceed. | none | - |

Every function with an `on(...)` event name above also supports per-id
routing: `lumen::on(event, key, handler)` calls `handler` instead of the
global function for that one key (the first argument: `id`, `tag`, `name`,
`source_id`, or `target_id`). A key with no matching route falls through to
the global function. `on_start`, `on_ready`, `on_audio_end`, and `on_close`
take no key argument, so they are never routed.

`on_ready` is wired by the runtime rather than by the candela host crate,
so a bare embedding of `lumen-script-candela` that skips the standard
plugin wiring never fires it.

## Example

A minimal counter with a click handler, a derived label, and one dynamic-DOM
row, in the shape both example apps use:

```candela
import "lumen.cdl";

fn calc_click_label(n) {
    return "clicks: " + str(n);
}

fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn add_log_row(container, text) {
    let row = spawn("label");
    row.set_text(text);
    container.append(row);
}

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::derive("click_label", ["clicks"], "calc_click_label");
    lumen::on("click", "bump", "handle_bump");
}

fn on_ready() {
    let log = get_by_id("log");
    if log.exists() {
        add_log_row(log, "ready");
    }
}

fn main() {}
```

## Limitations

candela's embedding has no first-class closure or user-defined object
value, so a few things read differently than they do from Rhai. `derive`
and `event_on`/`on` reference the recompute or handler function by its
*name* (a string) rather than accepting a closure literal. There is no
`signal(name, default) -> Signal` handle object and no `signal_array`; use
the string/typed `signal_get`/`signal_set` pair and build lists as real
DOM elements instead. `http(request)` and `parse_json(s)` are absent for
the same reason: a mixed-type request map and an any-shaped return value
both fall outside what a scalar host-fn boundary can declare.

The `Node`/`Event` method sugar is a compile-time wrapper over the same
`lumen::node_*`/`lumen::event_*` host calls; it adds no new capability
and no per-node object identity beyond the handle. A `Node` value does not
survive past the tick it was read or spawned in any more than a raw handle
does. Methods exist for most of the node/event surface, but the eleven
map-returning introspection readers (`node_rect` and the rest, listed
above) and a few receiver-less functions (`node_query`, `dump_tree`) are
still called as free `lumen::` functions.

`node_set_inner_markup`/`.set_inner_markup(...)` only does anything on a
from-source run; a precompiled (AOT) app has no markup parser and treats
it as a no-op, so apps that ship precompiled build lists node by node
instead, as both example apps do.

Two surfaces Rhai and Lua get from the runtime are not reachable here.
`lumen::page(...)`, `lumen::page_current()`, `lumen::page_back()`,
`lumen::page_forward()`, and `lumen::set_color_scheme(...)` are registered
on the candela engine but are not declared in `lumen.cdl`, so calling one
fails at runtime with "lumen is not a valid namespace". Navigate with
`window::set_href(path)` and `window::href()` instead, which the prelude
does declare; there is no candela equivalent for `set_color_scheme` today.

**Do not nest one host call inside another's argument list.** Writing
`lumen::signal_set_int("n", lumen::signal_get_int("n") + 1)` aborts the
process. Bind the inner call to a local first:

```candela
let n = lumen::signal_get_int("n");
lumen::signal_set_int("n", n + 1);
```

This is a defect in the candela virtual machine's host-call bridge, not a
deliberate restriction. It applies to every `lumen::`, `window::`,
`history::`, and `document::` function; calls nested inside a plain
candela function, including the prelude's `Node` and `Event` methods, are
unaffected.
