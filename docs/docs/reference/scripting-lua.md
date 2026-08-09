# Lua scripting API

Lua is one of the three script hosts Lumen ships, alongside candela (the
default) and Rhai. It runs the same event handlers, signals, and dynamic DOM
API on Lua 5.4, via `mlua`. Reach for it when you or your team already think
in Lua, or when you want Lua idioms (colon-call methods, 1-indexed arrays,
`pcall`) for app logic.

Attach the script from markup the way every host does:

```html
<script src="main.lua" />
```

A `main.lua` in the app directory selects the Lua host on its own, as long as
no `.cdl` file sits beside it. Pin the choice with `[script] engine` when you
want it to hold regardless of which files are present:

```toml
[app]
entry = "main.lmn"

[script]
engine = "lua"
```

An inline `<script>...</script>` body carries no extension for that inference
to read, so an app that keeps its Lua entirely inline needs the explicit
`engine = "lua"`. See [Choosing a host](../authoring/scripting.md#choosing-a-host)
for the full selection order.

Every inline body and every `src` file concatenate, in document order, into
one program compiled by that one host. There is no way to mix hosts within
one app.

## Reaching the API from Lua

There is no `require` and no module table to import. Every Lumen-provided
capability is installed as a plain Lua global (a function, or a table of
functions) before your script's top level runs, alongside Lua's normal
standard library (`string`, `table`, `math`, `os`, `io`, `coroutine`, ...) -
the host does not sandbox the interpreter beyond what `mlua`'s default state
loads. Four shapes show up:

- **Global functions** - most of the surface: `signal(...)`, `set_text(...)`,
  `fetch(...)`, `query(...)`, and so on. Call them directly.
- **Namespace tables** - `document`, `window`, `history`, and the chained
  `signals` root group related functionality: `document.query(sel)`,
  `window.set_title(t)`, `history.back()`, `signals.count.get()`.
- **Handle objects (userdata)** - `signal(...)`, `signal_array(...)`,
  `query(...)`, `get_by_id(...)`, and the DOM traversal methods return
  Lua userdata with **colon-call** methods: `local c = signal("n", 0);
  c:set(c:get() + 1)`. This is the one place Lua's idiom diverges from
  Rhai's dot-call - the receiver is the object before the colon, matching
  ordinary Lua OOP.
- **Lifecycle callbacks** - functions **you** define at the top level
  (`function on_click(id) ... end`) that the runtime calls by name at
  well-defined points. See [Lifecycle callbacks](#lifecycle-callbacks)
  below.

Lua's native 1-indexing is preserved where it fits: `ArraySignal:get(i)`
and a `signals.list[i]` subscript are 1-based. Two exceptions inherited
from the underlying Rust query result type are called out at their entry
below (`NodeQuery:nth`, `window.size()`).

## Lifecycle callbacks

These are not builtins - they are functions you write at the script's top
level. The runtime looks each one up by name and calls it when the matching
event happens; a script that omits one just never receives that event
(a missing function is silent, not an error).

| Function | Fires when |
|---|---|
| `on_start()` | Once, at app construction, before the first tick and before any element is queryable. Register handlers and signals here. |
| `on_ready()` | Once, on the first tick after the DOM index is first published - the first point a `query`/`get_by_id` call inside a handler sees the mounted tree. Optional; a script without it is unaffected. |
| `on_close()` | Before the window backend tears anything down. Returning `false` vetoes the close and keeps the window open; any other return value (or no `on_close` at all) lets it proceed. |
| `on_click(id)` / `on_double_click(id)` | Pointer click / double-click on the element with id `id`. When both fire on the same element in one tick, only `on_double_click` runs. |
| `on_long_press(id)` | Long-press gesture on `id`. |
| `on_text_input(id, text)` | IME commit on an `<input>` / `<textarea>`; `text` is the committed substring. |
| `on_toggle(id, checked)` | A toggle control changed; `checked` is a bool. |
| `on_slider(id, value)` | A slider commit; `value` is a float. |
| `on_file_dropped(id, path)` | An OS file drop landed on an element with `drop="true"`. |
| `on_drop(target_id, payload)` | In-app drag-and-drop: a drag ended over `target_id`. `payload` is the source's `drag-payload` attribute (or its id). |
| `on_drag_start(source_id, payload)` | In-app drag-and-drop: a drag started from `source_id`. |
| `on_file_picked(tag, path)` | `pick_file` / `save_file` / `pick_file_filtered` dialog closed. Empty `path` means the user cancelled. |
| `on_files_picked(tag, paths)` | `pick_files` dialog closed; `paths` is the selected paths joined with `\|`. |
| `on_folder_picked(tag, path)` | `pick_folder` dialog closed. |
| `on_hotkey(name)` | A global OS hotkey registered with `register_hotkey` fired. |
| `on_menu(id)` | A `<menubar>` / `<menu>` item was clicked. |
| `on_dialog_accepted(id)` / `on_dialog_rejected(id)` | A native dialog closed via its accept / reject action. |
| `on_tray(id)` | A system tray icon registered with `tray_icon` was clicked. |
| `on_timer(name)` | A `set_timeout` / `set_interval` timer fired. |
| `on_fetch(tag, body)` / `on_fetch_error(tag, message)` | `fetch(url, tag)` completed. A non-2xx response fires `on_fetch_error` with `"HTTP status <code>"` as the message - `fetch` treats anything outside 2xx as a failure. |
| `on_http(tag, response)` | `http({...})` completed (2xx or not). `response` is a table `{ ok, status, headers, body, error }`; `ok` is only true for a 2xx status, but the call always reaches `on_http` rather than `on_http_error`. |
| `on_audio_end()` | The active audio track finished playing. Fires only when the audio subsystem is compiled in. |

Every event above except `on_start`, `on_ready`, `on_close`, and
`on_audio_end` also supports **per-id routing**: `on(event, key, "fn_name")`
sends that one event/key pair to `fn_name` instead of the global
`on_<event>` handler. Those four carry no key to route on. The event name
passed to `on(...)` is the lowercased word from the table above
(`"click"`, `"double_click"`, `"long_press"`, `"text_input"`, `"toggle"`,
`"slider"`, `"file_dropped"`, `"drop"`, `"drag_start"`, `"file_picked"`,
`"files_picked"`, `"folder_picked"`, `"hotkey"`, `"menu"`,
`"dialog_accepted"`, `"dialog_rejected"`, `"tray"`, `"timer"`, `"fetch"`,
`"fetch_error"`, `"http"`). A handler registered against the bare suffix
of a templated id also matches every instance of that template (see
`on` / `local_id` below).

## Reactive state

### `signal(name, default) -> Signal`

Return a handle to a named scalar signal, seeding it with `default` the
first time the name is seen (an existing SDK/FFI-pushed value wins over
`default`). Every call for the same `name` shares the same underlying
value.

```lua
local c = signal("clicks", 0)
c:set(c:get() + 1)
```

`Signal` methods: `sig:get()`, `sig:set(v)`.

### `signal_array(name) -> ArraySignal`

Return a handle to a named reactive array backing `<for each="name">`
markup. The array is created lazily on first `:set` / `:push`.

```lua
local todos = signal_array("todos")
todos:set({
    { id = "1", label = "Task A" },
    { id = "2", label = "Task B" },
})
todos:push({ id = "3", label = "Task C" })
local n = todos:len()
local row = todos:get(1)   -- 1-indexed: first item, or nil if empty
```

`ArraySignal` methods: `:set(arr)` (a Lua array/sequence table; a
non-sequence table is stored as a single-item array), `:push(item)`,
`:len()`, `:get(i)` (1-based), `:all()` (a snapshot array of every item -
mutating it does not write back; follow up with `:set(...)`).

### `signals` (chained accessor)

A global `signals` table gives dot- or index-chained access to the same
global property store `signal()` reads, without minting a named handle
first:

```lua
signals.count.set(5)
local n = signals.count.get()
signals.user.name.set("Alice")     -- dotted path -> key "user.name"
signals.users[1].name.set("Bo")    -- 1-based index -> key "users[1].name"
signals.bg.set_color("#ff8800")    -- writes a typed Color
```

`.get()` / `.set(v)` / `.set_color(hex)` work as both dotted
(`signals.count.get()`) and colon (`signals.count:get()`) calls; the value
is always the trailing argument. `set` takes an integer, number, boolean,
or string; any other Lua type is ignored. `get()` reads the cross-thread
typed snapshot before the script-local mirror, so it sees a value written
by an embedder or another thread.

An index segment becomes part of the key verbatim, so Lua's 1-based
`signals.users[1].name` addresses the key `users[1].name`. Rhai's 0-based
`signals.users[0].name` is a different key for the same logical row; keep
that in mind when porting a script between the two hosts.

`signals` has no entity-scoped form from Lua; the keys are global.

### `derive(name, deps, fn) -> Signal`

Register a computed signal. `deps` is a Lua array whose entries are either
`Signal` handles or plain strings naming a signal. `fn` re-runs whenever
any dependency's value changes, and its return value is stringified into
`name`. Every derivation also runs once on the first tick after
registration, whether or not a dep has changed, so a bound value is
correct on the first frame.

```lua
local clicks = signal("clicks", 0)
local label = derive("counter_label", { clicks }, function(n)
    return "Clicks: " .. n
end)

derive("status", {}, function() return "ready" end)  -- fires once at startup
```

### Deprecated typed signal builtins

`signal_set_int(name, value)` / `signal_get_int(name)`,
`signal_set_float(name, value)` / `signal_get_float(name)`,
`signal_set_bool(name, value)` / `signal_get_bool(name)`, and
`signal_set_color(name, hex)` / `signal_get_color(name)` write and read a
single typed global signal directly (same store as `signals`). They
predate the chained `signals.name.set(v)` / `.get()` form and are kept for
back-compat; prefer the chained form in new code. Every getter returns
`nil` on a miss or a type mismatch.

## Element and text mutators

These take the target's `id="..."` string and mutate it in one call - no
`Node` handle needed. They are the lowest-friction way to update markup
that isn't wired to a `bind-*` attribute.

- `set_text(target_id, text)` - replace the element's text content.
- `set_src(target_id, path)` - swap an `<image>`'s asset path
  (app-relative); the old decoded asset is dropped and re-decoded.
- `set_class(id, classes)` - replace the element's whitespace-separated
  class list.
- `set_root_class(classes)` - same, targeting `<root>` directly (no id
  needed); the usual way to drive a theme switch.
- `is_valid(id) -> bool` - whether the element currently passes validation
  (true when no validity signal has been recorded for it).

```lua
set_text("hero-temp", "21C")
set_src("hero-icon", "icons/sun.png")
set_root_class("app theme-dark")
```

## Routing and ids

### `on(event, id, handler_fn_name)`

Route one `event` on element `id` to the script function named
`handler_fn_name`, instead of the global `on_<event>(id)` fallback. See
[Lifecycle callbacks](#lifecycle-callbacks) for the full event-name list.

```lua
on("click", "save", "handle_save")
on("click", "cancel", "handle_cancel")

function handle_save(_id) end
function handle_cancel(_id) end
```

A handler registered for a bare suffix (`"save"`) also matches every
templated instance's qualified id (`"user-card:save"`,
`"team-card:save"`) via a last-`:` suffix fallback; register the full
qualified id instead for per-instance routing.

### `local_id(source, suffix) -> string`

Return the sibling id `suffix` inside the same template instance as
`source`. `local_id("user-card:btn", "label")` returns
`"user-card:label"`; a `source` with no colon returns `suffix` unchanged.

```lua
function handle_save(id)
    set_text(local_id(id, "status"), "Saved!")
end
```

## Timers

- `set_timeout(name, ms)` - one-shot; fires `on_timer(name)` after `ms`
  milliseconds.
- `set_interval(name, ms)` - repeating; fires `on_timer(name)` every `ms`
  milliseconds until cancelled.
- `cancel_timer(name)` - cancel a pending or repeating timer by name;
  no-op if unknown. Safe to call from inside the timer's own `on_timer`.

```lua
set_timeout("hide-toast", 3000)

function on_timer(name)
    if name == "hide-toast" then
        signals.toast.set("")
    end
end
```

## HTTP

### `fetch(url, tag)`

Issue an HTTP GET off-thread. On a 2xx response, fires
`on_fetch(tag, body)`; anything else (transport failure or non-2xx) fires
`on_fetch_error(tag, message)`.

```lua
fetch("https://api.example.com/weather?lat=40.7", "weather")

function on_fetch(tag, body)
    if tag == "weather" then
        local data = parse_json(body)
        signals.temp.set(data.current.temp)
    end
end

function on_fetch_error(tag, msg)
    signals.status.set("error: " .. msg)
end
```

### `http(request)`

General HTTP request. `request` is a table:
`{ method, url, headers, body, timeout_ms, tag }` (`method` defaults to
`"GET"`; `headers` is a string-keyed table; everything else is optional).
Fires `on_http(tag, response)` once the reply lands, where `response` is
`{ ok, status, headers, body, error }` - `ok` reflects a 2xx status, but
the callback fires for any completed request (transport failure sets
`status = 0` and fills `error`).

```lua
http({ method = "POST", url = "https://api.example.com/todos",
       headers = { ["Content-Type"] = "application/json" },
       body = "{\"title\":\"test\"}", tag = "create" })

function on_http(tag, response)
    if tag == "create" and response.ok then
        set_text("status", "created, status " .. response.status)
    end
end
```

### `parse_json(json) -> any`

Parse a JSON string into a Lua table/array/scalar. Returns `nil` on a
parse error.

## Dialogs

- `pick_file(tag)` - native open-file dialog; fires
  `on_file_picked(tag, path)`.
- `pick_files(tag)` - native multi-select dialog; fires
  `on_files_picked(tag, paths)` (paths joined with `|`).
- `pick_folder(tag)` - native folder picker; fires
  `on_folder_picked(tag, path)`.
- `save_file(tag, default_name)` - native save dialog seeded with
  `default_name`; fires `on_file_picked(tag, path)`.
- `pick_file_filtered(tag, spec)` - `pick_file` with a filter list.
  `spec` is pipe-separated `Label:ext1,ext2` groups; a literal `*`
  extension means "no filter" (`"Images:png,jpg|All:*"`).

Every dialog fires its handler once, even on cancel (with an empty path),
so scripts can clean up modal state unconditionally.

```lua
on("click", "open", "do_open")
function do_open(_id) pick_file_filtered("open", "Text:txt,md|All:*") end

function on_file_picked(tag, path)
    if tag == "open" and path ~= "" then
        signals.opened.set(path)
    end
end
```

## Native shell (OS integration)

- `notify(title, body)` - OS notification (libnotify / NSUserNotification
  / Toast). Fire-and-forget; a missing notification daemon logs to
  stderr rather than erroring the script.
- `copy_image(path)` - read the PNG at `path` (app-relative) and put it
  on the system clipboard.
- `save_clipboard_image(path)` - write the current clipboard image to
  `path` as PNG.
- `tray_icon(id, icon_path, tooltip)` - register or replace a system
  tray icon (macOS/Windows; Linux logs a warning and no-ops). Empty
  `tooltip` disables the tooltip. Clicks fire `on_tray(id)`.
- `unregister_tray(id)` - remove a previously registered tray icon.
- `register_hotkey(name, accelerator)` - register a global OS hotkey
  (Electron/`global-hotkey` accelerator syntax, e.g.
  `"CommandOrControl+S"`); fires `on_hotkey(name)` regardless of window
  focus.
- `unregister_hotkey(name)` - remove a previously registered hotkey.
- `open_menu(id)` / `close_menu(id)` - flip the `__menu_open:<id>` signal
  driving a `<menu id="...">` popup.

```lua
register_hotkey("save", "CommandOrControl+S")
function on_hotkey(name)
    if name == "save" then notify("Lumen", "Saved.") end
end
```

## Audio

Thin transport controls over the app's single audio player
(`lumen-audio`). `audio_seek` / `audio_volume` take a Lua number, so an
integer literal (`audio_seek(30)`) works the same as a float
(`audio_seek(30.5)`).

- `audio_play(path)` - load and play the app-relative track (wav/ogg),
  replacing any current track and resetting position to 0.
- `audio_pause()` - pause, holding position.
- `audio_resume()` - resume a paused transport.
- `audio_stop()` - stop and rewind to 0.
- `audio_seek(secs)` - seek to `secs` seconds (clamped to track duration).
- `audio_volume(level)` - set output volume, `0.0..=1.0`.

Playback position / duration / playing state are host-written signals,
not builtins: `audio_position`, `audio_duration`, `audio_playing`. Read
them with `signals.audio_position.get()` / `bind-*` markup /
`derive(...)`, not a getter function.

## Files and misc

- `read_file(path) -> string` - read a file to a string; empty string on
  error (a warning is logged).
- `write_file(path, contents) -> bool` - write `contents` to `path`;
  `true` on success.
- `parse_markdown(src) -> array` - parse markdown into a block list for
  `<for>`: each entry is `{ id, kind, level, text, lang }`, where `kind`
  is `"h"` / `"p"` / `"code"` / `"li"` / `"hr"` and `level` is the heading
  depth (0 otherwise).
- `add_clicks(n)` - legacy host-interpreted token; the runtime forwards it
  without attaching semantics. Kept for early-alpha app compatibility.
- `set_string(key, value)` - free-form key/value token, forwarded without
  semantics; only meaningful to a custom host extension.
- `print(...)` - Lua's `print` is overridden to capture its arguments
  (tab-joined, Lua's own `tostring` convention for numbers) into the host
  diagnostic stream instead of stdout.

## Global introspection

Read-only snapshots of runtime state, useful for debugging and devtools-style
scripts.

- `pointer_state() -> table` - `{ x, y, inside, buttons, modifiers =
  { shift, ctrl, alt, super } }` for the current pointer.
- `frame_info() -> table` - `{ frame, dt_ms, dirty_count }` for the current
  tick.
- `signals_all() -> table` - every signal's current stringified value, as
  a string-keyed table.
- `dump_tree() -> string` - a serialized dump of the live element tree.

## Dynamic DOM API

Lua reaches the same host-neutral dynamic-DOM surface the C ABI and the
other script hosts use (`lumen_script::node_query` /
`lumen_script::introspect` / `lumen_script::event` in the Rust source) -
it calls those Rust functions directly rather than going through the C
ABI. A `Node` is a lightweight handle wrapping a packed integer, valid for
the current tick's snapshot; nodes come from a query, a traversal step, or
a mutation call.

### Finding nodes

- `query(selector) -> NodeQuery` - run a CSS selector (the same selector
  engine the stylesheet cascade uses, including sibling combinators)
  against the live tree; returns a result set in document order.
- `get_by_id(id) -> Node | nil` - fast id lookup.
- `document() -> Node | nil` / `document.root() -> Node | nil` - the
  document root. `document` is both callable and a namespace table (see
  below), so `document()` and `document.root()` are equivalent.
- `document.query(selector)` / `document.get_by_id(id)` - the namespaced
  spellings of the two globals above.
- `document.focused() -> Node | nil` / `document.hovered() -> Node | nil` -
  the currently focused and hovered elements. There is no global form of
  these two.
- `spawn(tag) -> Node` / `document.spawn(tag) -> Node` - create a fresh
  detached element with markup tag `tag` (e.g. `"div"`); attach it with
  `parent:append(node)` or `node:set_parent(parent)`.

`NodeQuery` methods: `:len()`, `:is_empty()`, `:first() -> Node|nil`,
`:nth(i) -> Node|nil`, `:iter()` / `:collect()` (both return a 1-indexed
Lua array of every matched `Node`), `:single() -> Node` (errors unless
exactly one match), `:get_single() -> Node|nil` (`nil` for zero or more
than one match). **`:nth(i)` is 0-indexed** (`nth(0)` is the first
match) - this is the one place the DOM API does not follow Lua's usual
1-based convention, inherited from the underlying Rust index.

```lua
local cards = query(".card.selected")
if not cards:is_empty() then
    cards:first():add_class("highlight")
end
```

### Node traversal

`node:parent()`, `node:first_child()`, `node:last_child()`, `node:next()`,
`node:prev()` each return a `Node` or `nil`. `node:children()` returns a
1-indexed Lua array of every child `Node`. `node:closest(selector)` walks
up from `node` (inclusive) and returns the first ancestor matching
`selector`, or `nil`; a malformed selector raises a Lua error.
`node:exists()` / `node:valid()` (identical) report whether the handle
still resolves to a live element. `node:handle()` returns the packed
handle as an integer.

### Node mutation

Every mutator enqueues its change and returns the receiver `Node` (except
where noted), so chains compose:
`node:add_class("a"):add_class("b"):set_text("done")`.

- `node:set_attr(name, value)` / `node:remove_attr(name)`
- `node:set_id(id)` - sugar for `set_attr("id", id)`
- `node:set_text(text)` - replace text content
- `node:set_inner_markup(markup)` - parse `markup` and replace the
  node's children with it. Only available on the dev / from-source run
  path (the injected markup parser is absent from a precompiled
  artifact, where this is a no-op). Do not feed it untrusted content -
  it injects live markup.
- `node:add_class(class)` / `node:remove_class(class)` /
  `node:toggle_class(class)`
- `node:set_class(classes)` - replace the whole class list
- `node:set_style(name, value)` (alias `node:style_set(name, value)`) /
  `node:style_remove(name)` - inline style property, the highest
  cascade tier
- `node:set_parent(parent)` (alias `node:move_to(parent)`) - append under
  a new parent, detaching from the current one
- `node:append(child)` - append `child` under `node`
- `node:insert_before(child, reference)` - insert `child` under `node`
  ahead of `reference`
- `node:replace_with(new)` - replace `node` with `new` in its parent,
  then remove `node`'s subtree; **returns `new`**, not the receiver
- `node:remove()` - detach and remove `node` and its subtree; returns
  nothing
- `node:clone_deep()` - deep-clone `node`'s subtree into a fresh detached
  node; **returns the clone**, not the receiver

### Node read-backs

These end a chain and return a value instead of the receiver:

- `node:get_attr(name) -> string|nil`, `node:id() -> string|nil`,
  `node:text() -> string|nil`
- `node:has_class(class) -> bool`
- `node:style_get(name) -> string|nil` - inline style only
- `node:computed_style(name) -> string|nil` - the resolved cascade value
  for one property; `node:computed_style()` with no argument returns the
  full resolved property map as a table

### Node introspection

Lower-level, post-layout state:

- `node:rect() -> table|nil` / `node:content_rect() -> table|nil` -
  `{ x, y, width, height, client_x, client_y }`, or `nil` before first
  layout
- `node:scroll() -> table|nil` - `{ x, y, max_x, max_y }`
- `node:is_visible() -> bool`
- `node:z_index() -> integer`
- `node:inline_style() -> table` - property name -> value
- `node:attrs() -> table` - attribute name -> value
- `node:classes() -> table` - 1-indexed array of class names
- `node:matched_rules() -> table` - 1-indexed array of every stylesheet
  rule matching `node`, each `{ selector, specificity = {a,b,c}, source,
  source_order, declarations }` (`source` is `"user-agent"` or
  `"author"`; `source_order` and `specificity` are what the cascade uses
  to break ties between entries)
- `node:entity_id() -> table|nil` - `{ index, generation }` of the
  backing ECS entity
- `node:components() -> table` - 1-indexed array of component type names
  present on the entity
- `node:component(name) -> table|nil` - one component's fields as a
  string-keyed table, or `nil` if absent; raises a Lua error for an
  unknown component name
- `node:outer_markup() -> string` / `node:inner_markup() -> string`

### Events

`node:on(event_type, handler)` and `node:on(event_type, handler, capture)`
bind a Lua function to `node`; `node:on_capture(event_type, handler)` is
the capture-phase shorthand for `on(type, handler, true)`. Both return an
**off function**: calling it unbinds the handler.

```lua
local off = get_by_id("save-btn"):on("click", function(ev)
    ev:prevent_default()
    set_text("status", "clicked at " .. ev:x() .. "," .. ev:y())
end)

-- later: off()
```

The handler receives one argument, an `Event` userdata, with:
`ev:target()` / `ev:current_target()` (`Node`), `ev:event_type()`,
`ev:key()`, `ev:value()`, `ev:button()`, `ev:x()` / `ev:y()` (local
coordinates), `ev:client_x()` / `ev:client_y()` (window coordinates),
`ev:delta_x()` / `ev:delta_y()` (wheel), `ev:position()` (a table with
`x`, `y`, `client_x`, `client_y`), `ev:modifiers()` (a table with `shift`,
`ctrl`, `alt`, `super`), `ev:prevent_default()`, `ev:stop_propagation()`,
`ev:stop_immediate_propagation()`.

Propagation follows the standard DOM contract: capture phase root-to-target,
then the target's own handlers, then bubble phase target-to-root for event
types that bubble (`focus`, `blur`, `pointerenter`, `pointerleave`, and
`scroll` do not bubble; everything else does).

### Window, location, and history

`window` and `history` are namespace tables, not per-node methods:

- `window.set_href(path)` / `window.href() -> string` - navigate / read
  the current page path (file-based pages)
- `window.reload()` - re-navigate to the current path
- `window.title() -> string` / `window.set_title(title)`
- `window.size() -> table` - a 1-indexed pair `{ width, height }` (not a
  named `{w, h}` table - index it as `window.size()[1]`)
- `window.set_size(width, height)`
- `window.dpr() -> number` - device pixel ratio
- `window.location.path() -> string` - same as `window.href()`.
  `window.location.query()` and `window.location.hash()` always return
  `""`; those parts of the URL are not tracked yet.
- `history.back() -> boolean` / `history.forward() -> boolean` (each
  reports whether a step was available) / `history.go(delta)`

The runtime adds four more navigation globals on top of the host crate:
`page(path)` navigates, `page()` with no argument reads the current page
path, and `page_back()` / `page_forward()` step the history stack.
`set_color_scheme(name)` forces the resolved color scheme, taking one of
`"default"`, `"force-light"`, `"force-dark"`, `"prefer-light"`, or
`"prefer-dark"`. These come from the runtime rather than
`lumen-script-lua`, so a custom embedding that skips the standard plugin
wiring will not have them; the same applies to `on_ready`. See
[Pages](../authoring/pages.md).

## A worked example

Based on the shipped weather app (`apps/weather/main.lua`), which sets
`engine = "lua"` and loads via `<script src="main.lua" />`:

```lua
local function fmt_temp(c)
    local unit = signal("unit", "C"):get()
    if unit == "F" then
        return math.floor(c * 9.0 / 5.0 + 32.0) .. "F"
    end
    return math.floor(c) .. "C"
end

local function render_hero()
    local city = signal("city", ""):get()
    if city == "" then
        set_text("hero-city", "Pick a city")
        return
    end
    set_text("hero-city", city)
    set_text("hero-temp", fmt_temp(signal("hero_temp", 0.0):get()))
end

function on_start()
    signal("unit", "C"):set("C")
    render_hero()
end

function on_text_input(id, text)
    if id == "search" then
        set_text("status", "Fetching " .. text .. "...")
        local url = "https://geocoding-api.open-meteo.com/v1/search?count=1&name=" .. text
        fetch(url, "geo")
    end
end

function on_fetch(tag, body)
    local data = parse_json(body)
    if tag == "geo" then
        local r = data.results[1]
        signal("city", ""):set(r.name)
        render_hero()
    end
end

function on_fetch_error(tag, msg)
    set_text("status", "Fetch " .. tag .. " failed: " .. msg)
end
```

The full app additionally shows per-day forecast rendering with a `for`
loop over a fetched array, unit toggling on click, and a drop-target
handler - see `apps/weather/main.lua` and its markup
`apps/weather/main.lmn` for the complete picture.

## Limitations

The Lua host covers the same ground as the Rhai host, including the full
dynamic DOM API (query, traversal, mutation, introspection, events), but a
few names and shapes differ:

- Lua creates an element with `spawn(tag)` / `document.spawn(tag)`. Rhai
  reserves `spawn` as a keyword in its own tokenizer, so Rhai source spells
  it `create(tag)` / `document.create(tag)`.
- Rhai's `Signal` carries a `.value` get/set property; Lua's `Signal` has
  only `:get()` and `:set(v)`.
- Rhai overloads `audio_seek` / `audio_volume` on int and float; Lua takes
  a single number for each, which covers both.
- `register_command_fn` caps at four arguments on Rhai. Lua is natively
  variadic and has no cap.

`window.size()` returns a positional pair rather than a
`{width=,height=}` table, and `NodeQuery:nth` is 0-indexed while every
other indexed accessor in the API (`ArraySignal:get`, `signals.list[i]`)
is 1-indexed. Both are called out at their entries above since they break
the otherwise-consistent 1-based convention.

Four global introspection functions - `pointer_state`, `frame_info`,
`signals_all`, and `dump_tree` - are callable exactly like the rest of the
surface, but the Lua host's builtin metadata table, which the language
server draws completion and hover from, does not list them. Editor
completion for those four is missing even though the calls work.

A handler that raises clears the host's pending command queue, so the
signal writes it made are discarded. DOM mutations are not: Lua queues
them on a separate bus that a handler error does not roll back, so a
partially built subtree survives. Build a subtree in one pass, or check
your inputs before you start mutating.

candela, the third host and Lumen's default, carries DOM nodes and events
as integer handles. Its prelude wraps them in `Node` and `Event` types
with the same method shape Lua and Rhai expose (`get_by_id(id).parent()`,
`event(ev).target()`), and the prefixed free functions
(`lumen::node_parent(h)`, `lumen::event_target(ev)`) stay reachable
underneath. See [candela scripting](./scripting-candela.md).
