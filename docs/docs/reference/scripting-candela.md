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
engine = "candela"   # "rhai" (default) | "lua" | "candela"
```

There is no automatic ".cdl file present" detection for a normal
`lumenc run`; `engine` must be set explicitly, as both example apps in this
repository do. (A static `--bundle` build separately infers which single
host to compile in from the app's script file extensions when `[script]
engine` is absent, but that inference only decides what gets linked into the
binary, not which host a `lumenc run` uses.)

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

The prelude also defines one plain candela helper function, `lm_append`,
described at the end of the DOM write-side section.

Without the import, and without a hand-written `host` block, the source
still compiles (candela resolves host functions lazily), but calling an
undeclared `lumen::...` function fails at runtime with "lumen is not a valid
namespace". Every builtin below is scalar-typed at the candela boundary: an
`int` is an i64, arrays are homogeneous (`int[]`, `string[]`), and a bare
`name(args);` call with no leading type returns `()` (null).

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

Use this when a value drives a `bind-text`/`bind-value`/`bind-visible`
attribute in markup, or needs to survive across handler calls (every event
handler is a fresh call into the program; signals are the state that
persists between calls).

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

candela reaches the live element tree the same way the C ABI and the other
script hosts do, through a process-global node-id table
(`lumen_core::node`); it does not go through the FFI crate's `lumen_*` C
functions at all; it calls the same underlying Rust query/mutation code
directly, as in-process host functions. A node is represented as a candela
`int`: `0` always means "no node". Handles from `node_spawn` /
`node_clone_deep` are valid for the rest of the current tick.

Selectors passed to `node_query` / `node_closest` / `document::query` use
the same CSS selector syntax as `main.css` (see
[CSS](../authoring/css.md)); the query walks the current per-tick DOM
snapshot and reuses the markup cascade's selector matcher.

### Read side: lookup and traversal

| Signature | Description |
|---|---|
| `int[] node_query(string selector)` | Run a CSS selector; returns matching node ids in document order. |
| `int node_get_by_id(string id)` | Fast id lookup; returns the node id or `0`. |
| `int node_document()` | The document root node id. |
| `int node_parent(int node)` | Parent node id, or `0`. |
| `int node_first_child(int node)` | First child node id, or `0`. |
| `int node_last_child(int node)` | Last child node id, or `0`. |
| `int node_next(int node)` | Next sibling node id, or `0`. |
| `int node_prev(int node)` | Previous sibling node id, or `0`. |
| `int[] node_children(int node)` | Child node ids, in document order. |
| `int node_closest(int node, string selector)` | Nearest ancestor-or-self matching `selector`; node id or `0`. |
| `bool node_valid(int node)` | Whether the id is present in the current snapshot. |

### Write side: build and mutate

There is no method-chaining sugar (`node.set_attr(...)`) on this dep of
candela; every mutation is a separate procedural call under `lumen::`.

| Signature | Description |
|---|---|
| `int node_spawn(string tag)` | Create a new element of `tag` (the same tag vocabulary as `.lmn` markup); returns its node id. |
| `int node_clone_deep(int node)` | Deep-clone a node and its subtree; returns the new root's id. |
| `node_set_attr(int node, string name, string value)` | Set an attribute. `"id"`, `"class"` (whitespace-split into the class list), `"text"`, and `"disabled"` are recognized specially; any other name goes into the generic attribute bag. |
| `node_remove_attr(int node, string name)` | Remove an attribute. |
| `node_set_id(int node, string id)` | Set the node's `id` attribute. |
| `node_set_text(int node, string text)` | Replace the node's text content. |
| `node_set_inner_markup(int node, string markup)` | Replace the node's children by parsing `markup` as `.lmn`-like source. Only meaningful when the app runs from source; a precompiled (AOT) app has no markup parser available, so this is a no-op there. Do not feed it untrusted content. |
| `node_class_add(int node, string class)` | Add one class. |
| `node_class_remove(int node, string class)` | Remove one class. |
| `node_class_toggle(int node, string class)` | Toggle one class. |
| `node_set_class(int node, string classes)` | Replace the whole class list (equivalent to `node_set_attr(node, "class", classes)`). |
| `node_set_style(int node, string name, string value)` | Set one inline style property. |
| `node_style_remove(int node, string name)` | Remove one inline style property. |
| `node_remove(int node)` | Remove the node (and its subtree). |
| `node_append(int parent, int child)` | Append `child` as `parent`'s last child. |
| `node_insert_before(int parent, int child, int reference)` | Insert `child` under `parent`, immediately before `reference`. |
| `node_set_parent(int node, int parent)` | Reparent `node` under `parent`, appended last. |
| `node_move_to(int node, int parent)` | Same as `node_set_parent`; reads better when the intent is "move", not "attach for the first time". |
| `node_replace_with(int old, int new)` | Replace `old` with `new` in the tree. |

Because a precompiled app has no source markup parser, the pattern used in
both example apps is to build lists element by element instead of relying
on `node_set_inner_markup`:

```candela
fn add_todo_row(container, idx, label, status) {
    let row = lumen::node_spawn("row");
    lumen::node_set_attr(row, "class", "list-row");
    lumen::node_append(container, row);
    add_cell(row, "li-idx", idx);
    add_cell(row, "li-label", label);
    add_cell(row, "pill", status);
}
```

The prelude ships a small helper for exactly this shape, so a caller does
not have to write the spawn/class/text/append sequence by hand every time:

```candela
fn lm_append(parent, tag, cls, text) { ... }   // in lumen.cdl

let row = lm_append(root, "row", "track", "Cipher");
```

`lm_append(parent, tag, cls, text)` spawns a `tag` element, adds `cls` as a
class (skip with `""`), sets `text` (skip with `""`), appends it under
`parent`, and returns the new node's id. It is a plain candela function
spliced in by the prelude, not a `lumen::`-namespaced host call.

### Read-backs on a single node

| Signature | Description |
|---|---|
| `string node_get_attr(int node, string name)` | Read an attribute; empty string if unset. |
| `string node_text(int node)` | The node's text content. |
| `string node_id(int node)` | The node's `id` attribute. |
| `bool node_class_contains(int node, string class)` | Whether the node has `class`. |
| `string node_style_get(int node, string name)` | Read one inline style property. |
| `string node_computed_style(int node, string name)` | Read one property from the resolved (cascade-applied) style. |

### Low-level introspection

| Signature | Description |
|---|---|
| `bool node_is_visible(int node)` | Whether the node is currently visible (not display:none, not clipped away). |
| `int node_z_index(int node)` | The node's resolved stacking z-index. |
| `string[] node_classes(int node)` | The node's class list. |
| `string[] node_components(int node)` | Names of the ECS components attached to the node's entity, for debugging. |
| `string node_outer_markup(int node)` | The node and its subtree, serialized as markup. |
| `string node_inner_markup(int node)` | The node's children, serialized as markup. |
| `string dump_tree()` | The whole document tree, serialized as markup, for debugging. |

A larger set of introspection readers is registered on the Rust host side
(node geometry, full computed style, raw attribute maps, entity id, pointer
state, frame timing, all signals) but is not reachable from a candela
script; see "Limitations" below.

### Events

Event bindings are procedural too: bind a handler by name, then read the
current event's fields through free `event_*` accessors keyed by the event
id the handler receives as its argument.

| Signature | Description |
|---|---|
| `int event_on(int node, string event_type, string handler)` | Bind `handler` to fire on `node` for `event_type` during the bubble phase; returns an off token. |
| `int event_on_capture(int node, string event_type, string handler)` | Same, but fires during the capture phase (root to target, before bubbling). |
| `event_off(int token)` | Unbind a handler previously bound with `event_on`/`event_on_capture`. |
| `int event_target(int ev)` | The node the event originally targeted. |
| `int event_current_target(int ev)` | The node whose handler is currently running. |
| `string event_type(int ev)` | The event type name (`"click"`, `"keydown"`, `"wheel"`, ...). |
| `string event_key(int ev)` | The logical key for a keyboard event (`"a"`, `"Enter"`, `"ArrowLeft"`). |
| `string event_value(int ev)` | The text value for an `input`/`change` event. |
| `int event_button(int ev)` | Pointer button: `0` primary, `1` secondary, `2` middle, `-1` none. |
| `float event_x(int ev)` | Pointer position relative to the target's top-left, logical pixels. |
| `float event_y(int ev)` | Pointer position relative to the target's top-left, logical pixels. |
| `float event_client_x(int ev)` | Pointer position in window (client) coordinates. |
| `float event_client_y(int ev)` | Pointer position in window (client) coordinates. |
| `float event_delta_x(int ev)` | Wheel scroll delta. |
| `float event_delta_y(int ev)` | Wheel scroll delta. |
| `bool event_shift(int ev)` | Shift held. |
| `bool event_ctrl(int ev)` | Control held. |
| `bool event_alt(int ev)` | Alt/Option held. |
| `bool event_super(int ev)` | Super/Cmd/Windows held. |
| `event_prevent_default(int ev)` | Suppress the event's default handling. |
| `event_stop_propagation(int ev)` | Stop the event from reaching the next node on its capture/bubble path. |
| `event_stop_immediate_propagation(int ev)` | Also stop any remaining handlers on the current node. |

Every `event_*` accessor takes the event id, but currently all of them read
one process-global "current event" cell rather than per-id state; the id
parameter exists for the web-idiomatic `event_target(ev)` call shape and to
leave room for nested dispatch later.

```candela
let off = lumen::event_on(btn, "click", "on_save");

fn on_save(ev) {
    let t = lumen::event_target(ev);
    lumen::event_prevent_default(ev);
}
// later: lumen::event_off(off);
```

Prefer `lumen::on("click", "save", "handle_save")` (per-id routing on the
static id from markup) for the common case; reach for `event_on` when you
need to bind against a node built at runtime, need the capture phase, or
need `prevent_default`/`stop_propagation`.

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
| `on_close()` | The window is about to close, before the backend tears anything down. Return `false` to veto the close and keep the window open; any other return value (or no `on_close` at all) lets it proceed. | none | - |

Every function with an `on(...)` event name above also supports per-id
routing: `lumen::on(event, key, handler)` calls `handler` instead of the
global function for that one key (the first argument: `id`, `tag`, `name`,
`source_id`, or `target_id`). A key with no matching route falls through to
the global function. `on_start`, `on_ready`, and `on_close` take no key
argument, so they are never routed.

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
    let row = lumen::node_spawn("label");
    lumen::node_set_text(row, text);
    lumen::node_append(container, row);
}

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::derive("click_label", ["clicks"], "calc_click_label");
    lumen::on("click", "bump", "handle_bump");
}

fn on_ready() {
    let log = lumen::node_get_by_id("log");
    if log != 0 {
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

A `host` block can only declare scalar and homogeneous-array return types,
not maps. A handful of Rust-side introspection readers return a
`{string: T}` map and are therefore registered on the host but have no
reachable candela declaration: `node_rect`, `node_content_rect`,
`node_scroll`, `node_computed_style_all`, `node_inline_style`,
`node_attrs`, `node_entity_id`, `node_component`, `pointer_state`,
`frame_info`, and `signals_all`. Use `node_is_visible`, `node_z_index`,
`node_computed_style` (single-property), `node_get_attr`, and
`node_classes` for the equivalent scalar-shaped reads.

The dynamic DOM write side has no method-chaining sugar (`node.set_attr(v)`
returning `node`); every mutation is its own `lumen::node_*` statement.
`node_set_inner_markup` only does anything on a from-source run; a
precompiled (AOT) app has no markup parser and treats it as a no-op, so
apps that ship precompiled build lists node by node instead (see
`lm_append`).
