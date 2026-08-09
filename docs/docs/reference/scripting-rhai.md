# Rhai scripting API

A Lumen app's behavior (event handlers, reactive state, timers, HTTP
calls, native dialogs, DOM mutation) lives in script, not markup.
Rhai is one of three script hosts Lumen ships (the others are Lua and
candela); this page is the exhaustive reference for what a Rhai script
can call.

## Attaching a script

A script attaches to an app with an explicit tag in `main.lmn`:

```html
<script src="main.rhai" />
```

An inline `<script>...</script>` body works too; both forms are
concatenated (inline body first, then every `src` file in document
order) into one combined source before it is compiled. A script-less
app has no `<script>` tag at all and skips the whole scripting plugin.

## Choosing the host

candela is the default host. `lumen.toml` selects a different one
explicitly:

```toml
[script]
engine = "rhai"   # "candela" (default) | "rhai" | "lua"
```

The match on `engine` is case-insensitive; an absent or unrecognized
value falls back to candela. With no `[script] engine` line at all, the
host comes from the script file extensions present in the app directory:
a `.cdl` file selects candela, otherwise a `.lua` file selects Lua,
otherwise a `.rhai` file selects Rhai, and a directory carrying no script
gets candela. That inference decides the host for `lumenc run` and, when
building with `lumenc bundle --static`, which single host gets compiled in. Rhai,
Lua, and candela expose the same set of lifecycle entry points and a
near-identical function surface; this page covers the Rhai bindings only.

## Lifecycle entry points

These are not builtins; they are functions you define that the
runtime calls by convention. A missing handler is silent success (the
runtime just doesn't call it). Commands a handler queues (`signal`
writes, `set_text`, `notify`, and so on) are applied on the same tick
the handler ran. `on_start` is the exception: it runs at app
construction, before any tick, and its commands are held and applied on
the first tick.

| Function | Fires when |
|---|---|
| `on_start()` | Once, immediately after the script loads, before the first tick. No element is queryable yet at this point. |
| `on_ready()` | Once, on the first tick after the static tree is mounted and queryable. Use this instead of `on_start()` to build DOM structure from a query. |
| `on_close()` | The window received a close request, before teardown. Return `false` to veto the close and keep the window open; any other return value (or no `on_close`) lets it proceed. |
| `on_click(id)` | A click landed on the `LumenId`-tagged element `id`. Suppressed for an element that also fired `on_double_click` this tick. |
| `on_double_click(id)` | A double-click landed on element `id`. |
| `on_long_press(id)` | A long-press landed on element `id`. |
| `on_toggle(id, checked)` | A `<toggle>`'s checked state changed. `checked` is bool. |
| `on_slider(id, value)` | A slider drag committed. `value` is f64. |
| `on_text_input(id, text)` | An IME/keyboard commit landed on `<input>` / `<textarea>` id. `text` is the committed string. |
| `on_file_dropped(id, path)` | A file was dropped onto an element with `drop="true"`. |
| `on_drop(target_id, payload)` | An in-app drag-and-drop payload was released over `target_id`. `payload` is the source's `drag-payload` attr (or its id). |
| `on_drag_start(source_id, payload)` | An in-app drag started from `source_id`. |
| `on_hotkey(name)` | A global OS hotkey registered via `register_hotkey` fired. |
| `on_menu(id)` | A `<menubar>` / `<menu>` item was clicked. |
| `on_tray(id)` | A system tray icon registered via `tray_icon` was clicked. |
| `on_dialog_accepted(id)` / `on_dialog_rejected(id)` | A dialog closed via its accept / reject action. |
| `on_file_picked(tag, path)` | `pick_file` / `pick_file_filtered` / `save_file` returned. `path` is empty on cancel. |
| `on_files_picked(tag, paths)` | `pick_files` returned; `paths` is the selection joined with `\|`. |
| `on_folder_picked(tag, path)` | `pick_folder` returned. `path` is empty on cancel. |
| `on_timer(name)` | A `set_timeout` / `set_interval` timer fired. |
| `on_fetch(tag, body)` / `on_fetch_error(tag, message)` | A `fetch(url, tag)` request completed (2xx) or failed (transport error or non-2xx). |
| `on_http(tag, response)` | An `http(request)` request completed. `response` is `#{ ok, status, headers, body, error }` regardless of status. |
| `on_audio_end()` | The active audio track finished playing. Only fires when the audio subsystem is compiled in. |

Every event with an `id`, `tag`, or `name` argument also supports **per-id
routing**: `on(event, id, fn_name)` (see [Routing](#routing)) registers a
handler called instead of the global fallback for that one `(event, id)`
pair. `event` is the bare name without the `on_` prefix and without the
trailing `(...)` (`"click"`, `"long_press"`, `"toggle"`, `"drop"`,
`"hotkey"`, `"file_picked"`, `"timer"`, `"fetch"`, `"http"`, and so on).
`on_start`, `on_ready`, `on_close`, and `on_audio_end` carry no key to route
on, so they are only ever the global function.

`on_ready` is wired by the runtime rather than by the Rhai host crate, the
same way the `page*` functions are; a bare embedding of `lumen-script-rhai`
that skips the standard plugin wiring never fires it.

## Signals (reactive state)

A signal is a named, reactive value. Writing one marks it dirty for
the current tick, which drives `bind-*` markup, `derive()` recompute,
and `signal_array`-backed `<for>` reconciliation.

### `signal(name, default) -> Signal`

Return a handle to a named scalar signal, initializing it to `default`
the first time the name is seen. Repeat calls with the same name are
cheap and share the same underlying value. A value already pushed into
the store from outside the script (the Rust SDK, the C ABI, another
thread) wins over `default`, so declaring a signal never clobbers what
an embedder seeded.

```rhai
let c = signal("clicks", 0);
c.set(c.get() + 1);

// Vue-style accessor sugar:
c.value = c.value + 1;
```

`Signal` methods: `.get() -> Dynamic`, `.set(v)`, and the `.value`
get/set property (an alias for `.get()` / `.set()`).

### `signal_array(name) -> ArraySignal`

Return a handle to a named reactive array. Backs `<for each="name">`
markup. The array is created lazily on the first `.set()` / `.push()`;
reads before that return `()` / `0`.

```rhai
let todos = signal_array("todos");
todos.set([
    #{ id: "1", label: "Task A" },
    #{ id: "2", label: "Task B" },
]);
todos.push(#{ id: "3", label: "Task C" });
let n = todos.len();
let row = todos.get(0);
let all = todos.all();   // owned snapshot; call .set(...) to write it back
```

`ArraySignal` methods: `.set(array)`, `.push(item)`, `.len() -> int`,
`.get(index) -> Dynamic` (`()` out of bounds), `.all() -> Array`
(owned snapshot; mutating it does not write back).

### `derive(name, deps, f) -> Signal`

Register a computed signal. `deps` is an array of `Signal` handles or
signal-name strings. `f` is called with the current dep values
whenever any dep is in this tick's dirty set (bounded to 32 cascade
passes per tick to catch a cyclic `derive`); its return value is
stringified into signal `name`. Returns a `Signal` handle for reading
the result.

```rhai
let clicks = signal("clicks", 0);
let label  = derive("counter_label", [clicks], |n| "Clicks: " + n);

derive("greeting", ["who"], |s|
    if s == "" { "(nobody yet)" } else { "hi, " + s + "!" }
);

// Empty deps still runs once, on the first tick after registration.
derive("status", [], || "ready");
```

### Typed signals: `signals.<path>`

`signals` is a constant available in every script without a factory
call. Chained property / index access (`signals.count`,
`signals.user.name`, `signals.users[0].name`) builds a dotted key;
the terminal method dispatches on the value's Rhai type and writes
straight into the typed property store, bypassing the string-typed
signal mirror entirely:

```rhai
signals.count.set(5);            // -> PropertyValue::I64
signals.ratio.set(0.5);          // -> PropertyValue::F64
signals.enabled.set(true);       // -> PropertyValue::Bool
signals.title.set("Hello");      // -> PropertyValue::Str
signals.bg.set_color("#ff8800"); // -> PropertyValue::Color
let v = signals.count.get();
```

`set(v)` overloads on i64 / f64 / bool / string; there is no automatic
hex-color detection from a string payload, so a color write always
goes through the explicit `set_color(hex)` method (`"#rrggbb"` or
`"#rrggbbaa"`, leading `#` optional; an invalid hex string is a
no-op). `get()` reads the cross-thread typed snapshot first, falling
back to the script-local mirror.

The following procedural functions predate the `signals.*` chained form
and are kept working; prefer the chained form in new code:

| Function | Chained form |
|---|---|
| `signal_set_int(name, value:int)` | `signals.name.set(value)` |
| `signal_get_int(name) -> int` | `signals.name.get()` (miss or wrong type: `()`) |
| `signal_set_float(name, value:float)` | `signals.name.set(value)` |
| `signal_get_float(name) -> float` | `signals.name.get()` |
| `signal_set_bool(name, value:bool)` | `signals.name.set(value)` |
| `signal_get_bool(name) -> bool` | `signals.name.get()` |
| `signal_set_color(name, hex:string)` | `signals.name.set_color(hex)` |
| `signal_get_color(name) -> map` | `signals.name.get()`; returns `#{ r, g, b, a }` (0..255 ints) |

The writes are interchangeable, the reads are not. `signals.name.get()`
consults the cross-thread typed snapshot before the script-local mirror,
so it sees a value written by the Rust SDK, the C ABI, or another thread;
`signal_get_int` and its siblings read the mirror alone and miss it. Use
the chained form whenever a value can arrive from outside the script.

### `is_valid(id:string) -> bool`

True when the element `id` currently passes validation (reads the
`valid:<id>` signal the runtime's validation pass writes each tick).
Defaults to `true` when the signal has never been set.

## DOM query and traversal

The dynamic DOM API reads and mutates the live element tree by
handle, independent of the `id`-addressed convenience mutators below.
A `Node` wraps a packed handle; reads resolve against the current
tick's snapshot.

### Global lookups

| Function | Returns | Behavior |
|---|---|---|
| `query(selector:string) -> NodeQuery` | result set | Runs a CSS selector against the live tree; errors on a bad selector. |
| `get_by_id(id:string) -> Node \| ()` | one node | Fast id lookup. |
| `document() -> Node \| ()` | root node | The document root. |

### `NodeQuery` methods

`.len() -> int`, `.is_empty() -> bool`, `.first() -> Node \| ()`,
`.nth(i:int) -> Node \| ()`, `.iter() -> Array`, `.collect() -> Array`,
`.single() -> Node` (errors unless exactly one match),
`.get_single() -> Node \| ()` (`()` for zero or many matches).

### `Node` traversal + liveness

`.parent()`, `.first_child()`, `.last_child()`, `.next()`, `.prev()`
(each `Node | ()`), `.children() -> Array`, `.closest(selector:string)
-> Node | ()` (errors on a bad selector), `.exists() -> bool` /
`.valid() -> bool` (identical; true when the handle is in the current
snapshot), `.handle() -> int` (the raw packed handle).

```rhai
let rows = query(".row");
if !rows.is_empty() {
    let first = rows.first();
    let card = first.closest(".card");
}
```

## DOM mutation

Every mutator below queues a command applied on the same tick and
returns the receiver `Node` so calls chain. Read-backs end the chain
and return a value instead.

| Method | Effect |
|---|---|
| `n.set_attr(name, value)` | Set an attribute. |
| `n.remove_attr(name)` | Remove an attribute. |
| `n.set_id(id)` | Set the `id` attribute. |
| `n.set_text(text)` | Replace the node's text content. |
| `n.set_inner_markup(markup)` | Parse `markup` and replace the node's children. Only available on the from-source dev path (a precompiled artifact has no markup parser linked in); do not feed untrusted content into it. |
| `n.add_class(class)` / `n.remove_class(class)` / `n.toggle_class(class)` | Incremental class-list edits. |
| `n.set_class(classes)` | Replace the whole class list (space-separated). |
| `n.set_style(name, value)` / `n.style_set(name, value)` | Set one inline style property (both names do the same thing). |
| `n.style_remove(name)` | Remove one inline style property. |
| `n.set_parent(parent)` / `n.move_to(parent)` | Attach the receiver under `parent`, appended. |
| `n.append(child)` | Attach `child` under the receiver, appended. |
| `n.insert_before(child, reference)` | Attach `child` under the receiver, before `reference`. |
| `n.replace_with(new) -> Node` | Swap the receiver for `new` in its parent slot, despawn the old subtree; returns `new`. |
| `n.remove()` | Detach and despawn the node and its subtree. Terminal. |
| `n.clone_deep() -> Node` | Deep-clone the subtree into a fresh detached node. |
| `n.get_attr(name) -> string \| ()` | Read an attribute. Read-back. |
| `n.id() -> string \| ()` | Read the `id`. Read-back. |
| `n.text() -> string \| ()` | Read the text content. Read-back. |
| `n.has_class(class) -> bool` | Read-back. |
| `n.style_get(name) -> string \| ()` | Read an inline style property. Read-back. |
| `n.computed_style(name) -> string \| ()` | Read a property after the full cascade (stylesheet + inherited + inline). Reflects the last committed tick. Read-back. |

`document.create(tag) -> Node` / `create(tag) -> Node` mint a fresh
detached element. `spawn` is registered under the hood for API-name
parity with the other hosts, but `spawn` is a reserved keyword in
Rhai's own tokenizer, so Rhai source must use `create` (or
`document.create`).

```rhai
let row = document.create("div");
row.set_class("card");
row.set_text("New row");
document.root().append(row);
```

## Introspection (read-only)

Low-level reads over the same per-tick snapshot; `computed_style()`,
`matched_rules()`, `dump_tree()`, and `signals_all()` walk the whole
tree or re-run the cascade, so treat them as inspection calls, not a
per-frame hot path.

| Call | Returns | Notes |
|---|---|---|
| `n.rect()` | map `#{x,y,width,height,client_x,client_y}` or `()` | Post-layout border box. `x`/`y` are parent-local; `client_x`/`client_y` are window coordinates. |
| `n.content_rect()` | same shape | Inner box minus padding + border. |
| `n.scroll()` | map `#{x,y,max_x,max_y}` or `()` | Scroll offsets and their travel limits. |
| `n.is_visible() -> bool` | | `false` for a despawned handle. |
| `n.z_index() -> int` | | `0` when unset. |
| `n.computed_style() -> map` | (0-arg form) | Every modeled CSS property after cascade, as a name/value map. |
| `n.inline_style() -> map` | | The `element.style` override map. |
| `n.attrs() -> map` | | Full attribute map (`id`, `class`, `text`, plus generic attrs). |
| `n.classes() -> Array` | | The class list. |
| `n.matched_rules() -> Array` | array of maps | Each entry: `#{selector, specificity:[a,b,c], source, source_order, declarations}`, ascending cascade order. |
| `n.entity_id() -> map \| ()` | `#{index, generation}` | Raw ECS entity handle, for debugging. |
| `n.components() -> Array` | | Names of whitelisted Lumen components present on the node. |
| `n.component(name) -> map \| ()` | | Errors if `name` is not a whitelisted component. `()` when whitelisted but absent. |
| `n.outer_markup() -> string` | | Serializes the node and its subtree to markup text. |
| `n.inner_markup() -> string` | | Serializes just the node's children. |
| `dump_tree() -> string` | | Whole-tree structural dump (id / tag / classes / rect), for debugging. |
| `pointer_state() -> map` | `#{x,y,inside,buttons,modifiers}` | `modifiers` is `#{shift,ctrl,alt,super}`. |
| `frame_info() -> map` | `#{frame,dt_ms,dirty_count}` | Per-tick counters. |
| `signals_all() -> map` | | The whole signal set as name -> value. |

## DOM events

`n.on(type, handler)` binds a handler function to a node for an event
type and returns an off token; call the token as `off.call()` to
unbind (Rhai calls a stored `FnPtr` value through `.call()`, so a bare
`off()` does not work). `n.on_capture(type, handler)` and the 3-arg
`n.on(type, handler, capture)` bind a capture-phase listener.

Event types: `click`, `dblclick`, `pointerdown`, `pointerup`,
`pointermove`, `pointerenter`, `pointerleave`, `wheel`, `keydown`,
`keyup`, `input`, `change`, `submit`, `focus`, `blur`, `scroll`.
`focus`, `blur`, `pointerenter`, `pointerleave`, and `scroll` do not
bubble; every other type does. Dispatch follows the DOM contract:
capture root-to-target, then target, then bubble target-to-root.

```rhai
let off = get_by_id("save").on("click", |ev| {
    print("clicked " + ev.target().id());
    ev.stop_propagation();
});
// later: off.call();
```

The handler receives one `Event` argument. `Event` methods:

| Method | Returns | Applies to |
|---|---|---|
| `.target()` / `.current_target()` | `Node` | Original target / node whose handler is currently running. |
| `.event_type() -> string` | | The event type name. |
| `.key() -> string` | | `keydown` / `keyup`. |
| `.value() -> string` | | `input` / `change` / `submit`. |
| `.button() -> int` | | Pointer events: 0 primary, 1 middle, 2 secondary. |
| `.x()` / `.y()` | `float` | Position local to the target. |
| `.client_x()` / `.client_y()` | `float` | Position in window coordinates. |
| `.position() -> map` | `#{x,y,client_x,client_y}` | Both position pairs at once. |
| `.delta_x()` / `.delta_y()` | `float` | `wheel` scroll delta. |
| `.modifiers() -> map` | `#{shift,ctrl,alt,super}` | |
| `.prevent_default()` | | Cancels the type's default action (currently: `click` link navigation). |
| `.stop_propagation()` | | Halts delivery to further nodes on the propagation path. |
| `.stop_immediate_propagation()` | | Also halts remaining handlers on the current node. |

## Window, document, and history namespaces

`window`, `document`, and `history` are constants available in every
script, mirroring the web's global objects.

| Call | Returns | Behavior |
|---|---|---|
| `window.set_href(path)` | | Navigate (same bus as `page(path)` below). |
| `window.href() -> string` | | The current resolved path. |
| `window.reload()` | | Re-navigate to the current path. |
| `window.title() -> string` | | The current window title. |
| `window.set_title(title)` | | Set the OS window title. |
| `window.dpr() -> float` | | Device pixel ratio. |
| `window.size() -> [w, h]` | | Logical window size. |
| `window.set_size(w, h)` | | Resize the OS window (logical pixels). |
| `window.location` | `Location` | Property (not a call). |
| `location.path() -> string` | | Same as `window.href()`. |
| `location.query() -> string` | | Always `""`; Lumen does not track a query string. |
| `location.hash() -> string` | | Always `""`; Lumen does not track a hash. |
| `document.root() -> Node \| ()` | | Same as the global `document()`. |
| `document.query(selector) -> NodeQuery` | | Same as the global `query(...)`. |
| `document.get_by_id(id) -> Node \| ()` | | Same as the global `get_by_id(...)`. |
| `document.focused() -> Node \| ()` | | The currently focused element. |
| `document.hovered() -> Node \| ()` | | The currently hovered element. |
| `document.create(tag) -> Node` | | Same as the global `create(tag)`. |
| `history.back()` | | Step back in the in-memory navigation stack. |
| `history.forward()` | | Step forward. |
| `history.go(delta:int)` | | Negative steps back, positive steps forward, that many times. |

`document` the constant (a namespace object) and `document()`
the global function (returns the root `Node` directly) both exist and
do different things; Rhai keeps variables and function calls in
separate namespaces, so the two never collide, but the naming overlap
is easy to misread.

## Navigation and appearance (runtime extensions)

`page(path)`, `page()`, `page_back()`, `page_forward()`, and
`set_color_scheme(name)` are not registered by the Rhai host crate
itself; `lumenc run` / `lumenc build` register them as engine
extensions on top of it, so they are present in every app built the
normal way but would be absent from a bare embedding of
`lumen-script-rhai` that skips that step.

| Function | Behavior |
|---|---|
| `page(path:string)` | Navigate to a file-based page path. |
| `page() -> string` | Read the current page path. |
| `page_back()` / `page_forward()` | Step the in-memory page history. |
| `set_color_scheme(name:string)` | One of `"default"`, `"force-light"`, `"force-dark"`, `"prefer-light"`, `"prefer-dark"`. An unrecognized name logs a warning and is ignored. |

## Per-element convenience mutators

These predate the dynamic DOM API above and address elements by their
markup `id` (a `LumenId`-tagged entity) rather than by `Node` handle.
They remain the simplest option for a static, `id`-addressed element.

| Function | Effect |
|---|---|
| `set_text(target_id, text)` | Replace the text content of the element with id `target_id`. Superseded in most apps by `bind-text` + a signal write. |
| `set_src(target_id, path)` | Swap the asset path of an `<image id=target_id>` at runtime (app-relative path); the old asset is dropped and a fresh decode enqueued. |
| `set_class(id, classes)` | Replace the CSS classes on the element with id `id`. |
| `set_root_class(classes)` | Replace the CSS classes on the `<root>` element; the usual way to drive a theme-token switch. |

```rhai
set_class("card-1", "card highlighted");
set_root_class("app theme-dark");
set_src("hero", "icons/sun.png");
```

## Routing

### `on(event, id, fn_name)`

Register a per-id handler for one of the lifecycle events. When
`event` fires targeting `id`, the named function runs instead of the
global `on_<event>(id)` fallback for that pair only.

```rhai
on("click", "save",   "handle_save");
on("click", "cancel", "handle_cancel");

fn handle_save(_id)   { /* ... */ }
fn handle_cancel(_id) { /* ... */ }
```

Templates auto-namespace inner ids as `<use-id>:<inner-id>`. A handler
registered under the bare suffix (`"save"`) still matches every
instance (`"user-card:save"`, `"team-card:save"`, ...) via a
last-`:`-segment fallback; register the fully qualified id instead for
per-instance routing.

### `local_id(source, suffix) -> string`

Return the sibling id `suffix` within the same template instance as
`source`. `source` without a `:` returns `suffix` unchanged.
Multi-level prefixes stack: `local_id("a:b:btn", "label")` returns
`"a:b:label"`.

```rhai
fn handle_save(id) {
    let label_id = local_id(id, "status");
    set_text(label_id, "Saved!");
}
```

## Timers

| Function | Effect |
|---|---|
| `set_timeout(name, ms:int)` | One-shot timer; fires `on_timer(name)` after `ms` milliseconds. |
| `set_interval(name, ms:int)` | Repeating timer; fires `on_timer(name)` every `ms` milliseconds until cancelled. Setting a timer with a name already in use replaces it. |
| `cancel_timer(name)` | Cancel a pending or repeating timer. No-op if unknown. |

## HTTP

### `fetch(url, tag)`

Issue an HTTP GET off-thread. `on_fetch(tag, body)` fires on a 2xx
reply; `on_fetch_error(tag, message)` fires on a transport failure or
a non-2xx status, where `message` is `HTTP status <code>`.

```rhai
fetch("https://api.example.com/weather?lat=40.7", "weather");

fn on_fetch(tag, body) {
    if tag == "weather" {
        let data = parse_json(body);
        signal("temp", "").set("" + data.current.temp);
    }
}
```

### `http(request:map)`

General HTTP request, off-thread. `request` is `#{ method, url,
headers, body, timeout_ms, tag }`; only `url` and `tag` are required
(`method` defaults to `"GET"`, `headers` to `#{}`, `body` and
`timeout_ms` are optional). `on_http(tag, response)` fires once the
reply lands, where `response` is `#{ ok, status, headers, body, error
}`; a 4xx/5xx is a completed reply (`ok = false`, a real `status`),
not an error - `error` is only populated on a transport failure (DNS,
connect, timeout). Response header names are lowercased.

```rhai
http(#{
    method: "POST",
    url: "https://api.example.com/items",
    headers: #{ "Content-Type": "application/json" },
    body: "{\"name\":\"x\"}",
    tag: "create_item",
});

fn on_http(tag, response) {
    if tag == "create_item" && response.ok {
        print("created: " + response.body);
    }
}
```

### `parse_json(json:string) -> Dynamic`

Parse a JSON string into a Rhai map / array / scalar. Returns `()` on
a parse error.

## Native shell

| Function | Effect |
|---|---|
| `notify(title, body)` | Show an OS notification. |
| `pick_file(tag)` | Native open-file dialog; fires `on_file_picked(tag, path)`. |
| `pick_files(tag)` | Native multi-select dialog; fires `on_files_picked(tag, paths)`. |
| `pick_folder(tag)` | Native folder picker; fires `on_folder_picked(tag, path)`. |
| `pick_file_filtered(tag, spec)` | Like `pick_file`, filtered. `spec` is `Label:ext1,ext2\|All:*` (pipe-separated groups; `*` means no filter). |
| `save_file(tag, default_name)` | Native save dialog seeded with `default_name`; fires `on_file_picked(tag, path)`. |
| `register_hotkey(name, accelerator)` | Register a global OS hotkey (`"CommandOrControl+S"`, `"Alt+Space"`, `"F11"`); fires `on_hotkey(name)`. |
| `unregister_hotkey(name)` | Remove a previously registered hotkey. |
| `tray_icon(id, icon_path, tooltip)` | Register or replace a system tray icon (macOS / Windows; Linux logs a warning and no-ops). Clicks fire `on_tray(id)`. An empty `tooltip` disables the tooltip. |
| `unregister_tray(id)` | Remove a tray icon. |
| `copy_image(path)` | Copy the PNG at `path` (app-relative) to the system clipboard. |
| `save_clipboard_image(path)` | Write the current clipboard image to `path` as PNG. |
| `open_menu(id)` / `close_menu(id)` | Set / clear the `__menu_open:<id>` signal a `<menu id="...">` popup binds to. |
| `t(key) -> string` | Translate `key` in the app's active locale. Returns `key` itself when no catalogue carries it. See [Translation](../authoring/i18n.md). |
| `tr(key) -> string` | Alias for `t(key)`, under Qt's name. |
| `read_file(path) -> string` | Read a file to a string. Empty string on error. |
| `write_file(path, contents) -> bool` | Write `contents` to `path`. `true` on success. |

Every native-dialog and picker call is fire-and-forget: the dialog
opens on the main thread and the result arrives later through its
matching lifecycle handler, never as a direct return value.

```rhai
on("click", "open", "do_open");
fn do_open(_id) { pick_file_filtered("open", "Text:txt,md|All:*"); }

fn on_file_picked(tag, path) {
    if tag == "open" && path != "" {
        signal("opened", "").set(path);
    }
}
```

## Audio

| Function | Effect |
|---|---|
| `audio_play(path)` | Load and play the track at `path` (app-relative wav/ogg); resets position to 0. |
| `audio_pause()` | Pause, holding position. |
| `audio_resume()` | Resume a paused track. |
| `audio_stop()` | Stop and rewind to 0. |
| `audio_seek(secs)` | Seek to `secs` seconds (accepts int or float; clamped to track duration). |
| `audio_volume(level)` | Set output volume, `0.0..=1.0` (accepts int or float). |

There is no `audio_get_*` builtin: the runtime writes `audio_position`
(float seconds), `audio_duration` (float seconds), and `audio_playing`
(bool) signals every tick while a track is loaded, so read them with
`signal_get_float` / `signal_get_bool` or through `bind-*` markup.
`on_audio_end()` fires once when the active track finishes.

## Markdown

### `parse_markdown(src:string) -> Array`

Parse markdown into a block list for driving a `<for>` block, one map
per block: `#{ id, kind, level, text, lang }`. `kind` is one of `"h"`
(heading; `level` 1-6), `"p"`, `"code"` (`lang` from a fenced block's
info string), `"li"`, or `"hr"`. Inline emphasis, strikethrough, and
inline code flatten to plain text with their markdown delimiters kept
around them (Lumen labels render one plain-text run, not rich text).

## Miscellaneous

| Function | Effect |
|---|---|
| `add_clicks(n:int)` | Push an `AddClicks` command. The default runtime does not interpret this token itself; it is forwarded on the command bus for an embedder that chooses to read it. |
| `set_string(key, value)` | Push a `SetString` command. Same no-op-by-default, forward-only behavior as `add_clicks`. |
| `print(...)` | Rhai's built-in `print`, redirected to capture into Lumen's diagnostic stream (visible in `lumenc`'s stderr) instead of writing to real stdout. |

Everything Rhai's own standard library provides (`String` methods,
`Array` methods, `type_of`, `parse_float`, control flow, and so on) is
available unmodified; this page covers only what Lumen adds.

## Example

A trimmed click counter with a derived label, a per-id handler, and a
theme toggle - the same shape the shipped example apps use:

```rhai
fn on_start() {
    let clicks = signal("clicks", 0);
    let dark   = signal("dark", true);

    derive("counter_label", [clicks], |n| "Clicks: " + n);

    on("click", "bump",  "handle_bump");
    on("click", "theme", "handle_theme");
}

fn handle_bump(_id) {
    let c = signal("clicks", 0);
    c.set(c.get() + 1);
}

fn handle_theme(_id) {
    let dark = signal("dark", true);
    if dark.get() {
        dark.set(false);
        set_root_class("app theme-light");
    } else {
        dark.set(true);
        set_root_class("app theme-dark");
    }
}
```

## Limitations

The engine's expression-depth and call-stack limits are raised well
above Rhai's release-build defaults so real app scripts (long
`if`/`else-if` chains, big literal maps) do not hit a false
"expression too complex" failure; `lumenc check` compiles with the
same limits `lumenc run` does. There is no native variadic function
registration: `register_command_fn`, the escape hatch for embedders
that want to add a host-native function from outside Lumen itself,
tops out at 4 positional arguments (the Lua host has no such cap).
`n.set_inner_markup(...)` needs the markup parser linked in, so it does
nothing when the app runs from a precompiled artifact built without it;
`document.create` and the rest of DOM spawning work either way.
`set_color_scheme`, the `page*` navigation functions, and `on_ready`
come from the runtime rather than the Rhai host crate, so a custom
embedding of `lumen-script-rhai` that skips the standard `lumenc`
plugin wiring will not have them.
