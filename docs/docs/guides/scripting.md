# Scripting

Markup describes the interface and CSS styles it. A script supplies the
behaviour: it reacts to events, writes signals, builds elements at runtime, and
talks to the OS.

## Choosing a host

Lumen runs one script host per app, and three are available.

**candela** is the default. It is Lumen's own language, statically checked, and
the one the scaffolds use. One import gives you the whole surface:

```rust
import "lumen.cdl";

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::on("click", "bump", "handle_bump");
}

fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn main() {}
```

Beyond `lumen::`, the prelude adds `window::`, `document::`, and `history::`,
plus `Node` and `Event` wrappers so tree work reads as method chains. `main()`
stays empty; a Lumen app does its work in the lifecycle handlers.

**Rhai** and **Lua** expose the same capabilities as plain globals:

```rhai
// Rhai
fn on_start() {
    let clicks = signal("clicks", 0);
    derive("counter_label", [clicks], |n| "clicks: " + n);
}
fn on_click(id) { let c = signal("clicks", 0); c.set(c.get() + 1); }
```

```lua
-- Lua
function on_start()
    local clicks = signal("clicks", 0)
    derive("counter_label", { clicks }, function(n) return "clicks: " .. n end)
end
function on_click(id)
    local c = signal("clicks", 0)
    c:set(c:get() + 1)
end
```

The host is picked from the file extensions in the app directory: a `.cdl` file
selects candela, a `.lua` file selects Lua, a `.rhai` file selects Rhai. A
directory holding more than one is resolved in that order, so the answer never
depends on directory listing order. Override it in `lumen.toml`:

```toml
[script]
engine = "rhai"
```

An app that keeps its script inline in `<script>` has no extension to read and
is treated as candela, so declare the engine if it is written in something
else. All `<script>` sources in an app are concatenated and handed to that one
host; do not mix languages in a single app.

## Lifecycle

Two entry points run automatically:

- `on_start()` runs once when the app is constructed, before the first frame.
  The element tree is not queryable yet, so a lookup here finds nothing. Use it
  to seed signals and register handlers.
- `on_ready()` runs on the first tick, once the tree is mounted and queryable.
  Build your initial dynamic content here.

`on_ready` is re-armed by hot reload, so a script that builds elements rebuilds
them after every edit.

## Handling events

There are three ways to receive an event, and they compose.

**Named callbacks** catch everything of one kind:

```rust
fn on_click(id) { ... }
fn on_double_click(id) { ... }
fn on_long_press(id) { ... }
fn on_timer(name) { ... }
```

Other callbacks in the same family cover text commits, toggles and sliders,
file drops and picks, in-app drag and drop, hotkeys, menus, the tray, dialog
results, HTTP replies, and window close.

**Per-id routing** sends one element's events straight to one function, which
reads better than an `if id == ...` chain:

```rust
lumen::on("click", "save", "handle_save");
```

Register it from `on_start`. Routing is available for `click`, `long_press`,
`drop`, `timer`, `hotkey`, `menu`, and `tray`.

**DOM-style bindings** attach a handler to a node you hold:

```rust
let btn = get_by_id("save");
btn.on("click", "handle_save");
btn.on_capture("keydown", "handle_key");
```

These follow the web propagation model: the event runs down the ancestor chain
in the capture phase, fires on the target, and bubbles back up. A handler
receives an event it can inspect and control, with `target`, `current_target`,
`key`, `value`, `button`, coordinates, wheel deltas, modifier flags,
`prevent_default`, `stop_propagation`, and `stop_immediate_propagation`.

The bindable types are `click`, `dblclick`, `pointerdown`, `pointerup`,
`pointermove`, `pointerenter`, `pointerleave`, `wheel`, `keydown`, `keyup`,
`input`, `change`, `focus`, `blur`, `submit`, and `scroll`. Of those, `input`,
`change`, and `submit` fire when a text field commits rather than on every
keystroke. `click` is the one type with a default action to prevent: it
suppresses the navigation an `<a href>` would otherwise perform.

## Reading and writing signals

Signals are the shared state bus between markup and script. Writing one updates
every element bound to it, and `derive` builds values that recompute
themselves. See [Reactivity](reactivity.md).

## Building elements at runtime

Static structure belongs in markup. When the shape depends on data, build it
with the DOM API.

```rust
fn on_ready() {
    let list = get_by_id("todos");
    for item in items {
        let row = spawn("row");
        row.set_attr("class", "list-row");
        row.set_text(item);
        list.append(row);
    }
}
```

The surface covers:

- **Finding nodes**: `document`, `get_by_id`, `query` with a CSS-style
  selector, and `closest`.
- **Walking the tree**: `parent`, `children`, `first_child`, `last_child`,
  `next`, `prev`.
- **Changing the tree**: `spawn`, `clone_deep`, `append`, `insert_before`,
  `move_to`, `set_parent`, `replace_with`, `remove`.
- **Changing a node**: `set_attr`, `remove_attr`, `set_id`, `set_text`,
  `class_add`, `class_remove`, `class_toggle`, `set_class`, `set_style`.
- **Reading a node back**: `get_attr`, `text`, `classes`, `style_get`,
  `computed_style`, `outer_markup`, `inner_markup`, and geometry.

Adding and removing classes is the preferred way to change appearance, because
the element re-resolves against the stylesheet and keeps every rule and custom
property it inherits. Reach for `set_style` only for a value that belongs to
one element alone, such as a computed position.

`set_inner_markup` parses a markup fragment, which needs the source parser. An
app compiled with `lumenc build` ships without it, so build those subtrees
element by element if you plan to precompile. See [Packaging](packaging.md).

## Timers

```rust
lumen::set_timeout("splash", 800);
lumen::set_interval("poll", 5000);
lumen::cancel_timer("poll");
```

Timers are named, and setting a timer with a name that is already in use
replaces it. Each firing calls `on_timer(name)`, or the function you routed
that name to with `on("timer", ...)`. A repeating timer cancelled from inside
its own handler stops immediately.

## Reaching the OS

Notifications, clipboard, file dialogs, menus, the tray, global hotkeys, audio,
and drag and drop are all script builtins. See
[OS integration](os-integration.md). HTTP lives here too: `fetch(url, tag)`
issues a GET off the UI thread and calls `on_fetch(tag, body)` or
`on_fetch_error(tag, message)` when it completes. For another method, headers,
or a request body, `http(request)` takes the whole request and answers on
`on_http(tag, response)`.

## Hot reload

`lumenc run` watches the app directory. Editing markup, CSS, an included
fragment, a stylesheet import, a script, or a translation catalogue updates the
running window without restarting it.

What survives an edit:

- Signal values, including array signals and anything derived from them.
- Per-element state of anything carrying an `id`: typed text and cursor,
  toggle and slider values, scroll positions. Elements without an `id` have no
  stable name to match on and start fresh.
- Handler and derivation registrations. A handler registered from `on_start`
  keeps working even though `on_start` does not run again; anything the new
  source registers replaces its match.

Elements a script created through the DOM API are rebuilt rather than
preserved: the tree is respawned from markup and `on_ready` fires again, so the
script recreates them the way it did at first mount.

An error keeps the app alive. A markup or CSS problem shows a banner across the
top of the window and leaves the last working version running. A script that
fails to compile leaves the previously loaded script in place and reports the
error.

One case needs a restart: renaming a handler function and the `on(...)` call
that binds it in the same edit. The old name stays registered because nothing
in the new source replaces it. Rename in two saves, or restart.

## Where to look things up

- Every builtin, per host: [candela](../reference/scripting-candela.md),
  [Rhai](../reference/scripting-rhai.md), [Lua](../reference/scripting-lua.md)
- Signals, `bind-*`, `<for>`, and `<if>`: [Reactivity](reactivity.md)
- `[script]` and the rest of the config:
  [lumen.toml](../reference/lumen-toml.md)
- Calling Lumen from Rust, C, C++, or Python: [FFI and SDKs](../reference/ffi.md)
