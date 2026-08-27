# Scripting

Markup describes the interface and CSS styles it. A script supplies the
behaviour: it reacts to events, writes signals, builds elements at runtime, and
talks to the OS.

## Choosing a host

Three hosts are available, and an app can use more than one.

**candela** is the default. It is Lumen's own language, statically checked, and
the one most scaffolds are written in. One import gives you the whole surface:

```rust
import "lumen.cdl";

fn on_ready() {
    lumen::signal_set_int("clicks", 0);
    get_by_id("bump").on("click", "on_bump");
}

fn on_bump(ev) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn main() {}
```

Beyond `lumen::`, the prelude adds `window::`, `document::`, and `history::`,
plus `Node` and `Event` wrappers so tree work reads as method chains. `main()`
stays empty; a Lumen app does its work in the lifecycle handlers.

candela's own standard library ships with the toolchain, so `import "std/time";`
and the array methods work in an app script the way they do anywhere else; see
[the candela reference](../reference/scripting-candela.md#the-candela-standard-library)
for where it does not reach.

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

Each script file picks its own host from its extension: `.cdl` runs under
candela, `.lua` under Lua, `.rhai` under Rhai.

```html
<script src="model.cdl"/>
<script src="report.lua"/>
```

An app written this way runs both hosts at once. Files of the same language
join into one program, so two `.cdl` files share their functions the way two
halves of one file would, and each still opens with its own
`import "lumen.cdl";`. Two different languages stay separate programs and
cannot call each other. What they do share is signals: they read and write the
same signal bus, and a value one host writes is visible to the other on the
same tick. Lifecycle and event callbacks reach every host, so `on_start`,
`on_ready`, `on_click` and the rest run in each language that defines them.

An inline `<script>` block has no extension to read. It joins the app's one
external language when there is exactly one, and candela otherwise.

To put the whole app on one engine regardless of extensions, declare it in
`lumen.toml`:

```toml
[script]
engine = "rhai"
```

That is also the answer for an app whose only script is an inline block written
in something other than candela.

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
`input`, `change`, `focus`, `blur`, `submit`, and `scroll`. Of those, `input`
fires on every edit to a text field, while `change` and `submit` wait for the
field to commit. `click` is the one type with a default action to prevent: it
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
        let row = create("row");
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
- **Changing the tree**: `create`, `clone_deep`, `append`, `insert_before`,
  `move_to`, `set_parent`, `replace_with`, `remove`.
- **Changing a node**: `set_attr`, `remove_attr`, `set_id`, `set_text`,
  `add_class`, `remove_class`, `toggle_class`, `set_class`, `set_style`.
- **Reading a node back**: `get_attr`, `text`, `classes`, `has_class`,
  `style_get`, `computed_style`, `outer_markup`, `inner_markup`, and geometry.

These names are the same in every host. Two of them exist in a one-argument
and a no-argument form, and because candela keys a host function by name
alone, the no-argument form gets its own name everywhere: `page(path)`
navigates and `page_current()` reads the active page, `computed_style(prop)`
reads one property and `computed_style_all()` reads the whole map.

An element a script builds is cascaded, measured and laid out in the same frame
it joins the tree, so a handler that tears a subtree down and rebuilds it never
paints a half-styled intermediate.

Adding and removing classes is the preferred way to change appearance, because
the element re-resolves against the stylesheet and keeps every rule and custom
property it inherits. Reach for `set_style` only for a value that belongs to
one element alone, such as a computed position; what it writes sits above every
rule, so a later class change cannot take it back. Clear it with `style_remove`
and the element takes the stylesheet's value for that property again, if a rule
sets one.

`set_inner_markup` parses a markup fragment, which needs the source parser. An
app compiled with `lumenc build` ships without it, so build those subtrees
element by element if you plan to precompile. See [Packaging](packaging.md).

A candela script has a second way to write a subtree: an `lmn!` block, which is
markup a function returns. Blocks compile to fragments when the app is built,
so they work in a precompiled app where `set_inner_markup` does not, and they
read as markup rather than as a sequence of calls. See
[components](composition.md#components).

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
issues a GET without holding up a tick and calls `on_fetch(tag, body)` or
`on_fetch_error(tag, message)` when it completes. For another method, headers,
or a request body, `http(request)` takes the whole request and answers on
`on_http(tag, response)`.

## Saving data

Reading and writing files is a runtime module rather than a builtin, so an app
that wants it says so in `lumen.toml`:

```toml
[dependencies]
lumen-fs = { bundled = true }
```

That puts a `files` namespace in front of every script the app runs:

```rust
let seed = files::read("data/seed.json");
files::write(files::data_dir() + "/session.json", session);
```

A relative path resolves against the app directory, so a file the app ships is
named the same way wherever the app was started from.

State the app writes back belongs somewhere else. An installed app directory is
read-only, and an update replaces what is in it, so a save written there is
either refused or lost. `files::data_dir()` answers with a directory of this
app's own, under the platform's user-data location and named by
[`[app] id`](../reference/lumen-toml.md); it exists by the time the call
returns.

Beyond reading and writing text there are `files::exists`, `files::is_dir`,
`files::list`, `files::mkdir`, `files::remove`, `files::copy`, and byte-level
`files::read_bytes` and `files::write_bytes`. A call that cannot do what it
was asked answers `false` or an empty value and explains itself on stderr, so
a script branches on what it got back. The full surface is in the scripting
reference for each host ([candela](../reference/scripting-candela.md#filesystem),
[Rhai](../reference/scripting-rhai.md#filesystem),
[Lua](../reference/scripting-lua.md#filesystem)); Lua spells the calls
`files.read(..)`.

Windows builds have no runtime modules yet, so this surface is unavailable
there.

## Functions the app's Rust adds

An app with Rust behind it can hand the script functions of its own, through the
SDK, the C ABI, or a plugin. They are described once and reach every language,
so a mixed-language app calls the same function from any of its scripts.

A function with no namespace of its own is a global in Rhai and Lua and lives
under `native` in candela:

```rust
// Rhai, Lua
now_ms()
```

```rust
// candela
native::now_ms()
```

One that chose a namespace is reached through it: `gpio::level(21)` in Rhai and
candela, `gpio.level(21)` in Lua. candela needs no `host` block for either; the
declarations are written for you from what was registered. A script you compile
ahead of time to an artifact is the exception, and declares the namespace itself.

Such a function can fail. It raises where the script called it, carrying the
message the Rust side gave and naming the function, so you catch it the way the
language catches anything else: `try`/`catch` in Rhai, `pcall` in Lua, and
`catch "host_fn_error"` in candela. An uncaught failure ends that one call and is
reported like any other script error; the app keeps running.

What is available is up to the app's Rust, so look in its source, not here.
[FFI and SDKs](../reference/ffi.md) covers exposing one, and
[Writing plugins](../contributing/plugins.md) covers doing it from a plugin.

## Hot reload

`lumenc run` watches every file the app loads. Editing markup, CSS, an included
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
- Markup a function returns: [Composition](composition.md#components)
- `[script]` and the rest of the config:
  [lumen.toml](../reference/lumen-toml.md)
- Calling Lumen from Rust, C, C++, or Python: [FFI and SDKs](../reference/ffi.md)
