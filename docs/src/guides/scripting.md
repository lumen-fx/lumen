# Scripting

Markup describes the interface and CSS styles it. A script supplies the
behaviour: it reacts to events, writes signals, builds elements at runtime, and
talks to the OS.

## The script language

Scripts are written in **candela**, Lumen's own language, statically checked,
and the one every scaffold is written in. One import gives you the whole
surface:

```rust
import "lumen.cdl";

fn on_ready() {
    signal<int>("clicks").set(0);
    get_by_id("bump").on("click", "on_bump");
}

fn on_bump(ev: int) {
    let clicks = signal<int>("clicks");
    clicks.set(clicks.get() + 1);
}

fn main() {}
```

`signal<int>("clicks")` is a handle on one named cell, and the type argument
says what the cell holds, so `get` hands back an `int` and `set` takes one.

Beyond `lumen::`, the prelude adds `window::`, `document::`, and `history::`,
plus `Node` and `Event` wrappers so tree work reads as method chains. `main()`
stays empty; a Lumen app does its work in the lifecycle handlers.

candela's own standard library ships with the toolchain, so `import "std/time";`
and the array methods work in an app script the way they do anywhere else; see
[the candela reference](../reference/scripting-candela.md#the-candela-standard-library)
for where the modules are read from and how they reach a browser.

A script reaches the app through `<script src="main.cdl"/>`, or an inline
`<script>` block, which is read as candela. Files of the same language join
into one program, so two `.cdl` files share their functions the way two halves
of one file would, and each still opens with its own `import "lumen.cdl";`.
An error in that program still names the file and line you wrote: the script
file for a `<script src>`, and the markup file for an inline block.

### How a script runs

The host that runs a script is a runtime module the toolchain ships, loaded for
you: an app declares nothing for it. While you work (`lumenc run`), the script
is compiled from source when the app starts and again on every save, which is
what hot reload rides on. When you ship (`lumenc build`, `lumenc package`), the
build compiles it to bytecode and the shipped app runs that bytecode without a
compiler, so its source does not travel with it. A script that does not
compile fails the build rather than the shipped app.

When the module that runs a language is not installed, the app still starts:
a banner on stderr and in the window says which script did not run.

### Rhai and Lua

Rhai and Lua are deprecated: new scripting features land in candela only, and
their reference pages ([Rhai](../reference/scripting-rhai.md),
[Lua](../reference/scripting-lua.md)) are not extended. A `.rhai` or `.lua`
file runs on its module the way a `.cdl` file runs on candela's, side by side
with a candela program: the languages do not call each other, but they read
and write the same signal bus, and lifecycle and event callbacks reach every
language that defines them. To put every script, inline ones included, on one
language, name it in `lumen.toml`:

```toml
[script]
engine = "lua"
```

## Lifecycle

Two entry points run automatically:

- `on_start()` runs once when the app is constructed, before the first frame.
  The element tree is not queryable yet, so a lookup here finds nothing. Use it
  to seed signals and register handlers.
- `on_ready()` runs on the first tick, once the tree is mounted and queryable.
  The tree already reflects what `on_start` wrote: the body of an `<if>` its
  signals open, and the rows of a `<for>` over an array it set, are there to
  find and to append to. Build your initial dynamic content here.

`on_ready` is re-armed by hot reload, so a script that builds elements rebuilds
them after every edit.

`on_close()` runs before the window closes, whatever asked: its close button,
an interrupt signal, or the app itself through `window::close()` or
`app::quit()`. Return `false` to keep the window open, for a "save changes?"
prompt; any other return lets it close. When several windows close at once, as
on a quit, it runs once for each, and the `window::` functions answer for the
window being asked about. A Quit menu item is one call:

```rust
fn on_menu(id: string) {
    if id == "file-quit" {
        app::quit();
    }
}

fn on_close() {
    // Keep the window open while there are unsaved changes.
    return !signal<bool>("dirty").get();
}
```

## Handling events

There are three ways to receive an event, and they compose.

**Named callbacks** catch everything of one kind:

```rust
fn on_click(id: string) { ... }
fn on_double_click(id: string) { ... }
fn on_long_press(id: string) { ... }
fn on_timer(name: string) { ... }
```

Other callbacks in the same family cover text commits, toggles and sliders,
file drops and picks, in-app drag and drop, hotkeys, menus, the tray, dialog
results, HTTP replies, and window close.

In candela, annotate the parameters. Nothing in the script calls a handler, so
its declared types are all the compiler has to work from. A bare parameter is
taken as `any`, and `lumenc build` warns about each function that has one. A
body that needs a known type, doing arithmetic on the value or joining it to a
string, does not compile as `any`: run from source, it compiles at the first
event instead, and a packaged app, which carries no compiler, cannot call it at
all, which the build also warns about. `any` counts as an annotation, and
[the handler table](../reference/scripting-candela.md#event-handlers) gives the
type of every argument a named callback takes. Rhai and Lua have no
annotations, so their handlers stay bare.

**Per-id routing** sends one element's events straight to one function, which
reads better than an `if id == ...` chain:

```rust
lumen::on("click", "save", "handle_save");
```

Register it from `on_start`. Any event whose handler opens with an element id
or a tag can be routed, and the routed function takes the arguments the named
callback would have taken. Write the event name without the `on_` prefix.

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

The bindable types are `click`, `dblclick`, `contextmenu`, `pointerdown`,
`pointerup`, `pointermove`, `pointercancel`, `pointerenter`, `pointerleave`,
`gotpointercapture`, `lostpointercapture`, `wheel`, `keydown`, `keyup`,
`input`, `change`, `focus`, `blur`, `submit`, `scroll`, `pointerlockchange`,
and `pointerlockerror`. Of those, `input`
fires on every edit to a text field, while `change` and `submit` wait for the
field to commit; leaving a field after an edit raises `change` too. Checkboxes,
toggles, switches, sliders, radios, dropdowns and color fields raise `input` and
`change` as the user changes them. `click` and `contextmenu` have a default
action to prevent:
`click` suppresses the navigation an `<a href>` would otherwise perform, and
`contextmenu` keeps a browser from opening its own menu in a web build. Key
events go to the focused element; with nothing focused they go to the
document, so bind
app-wide shortcuts on `document_node()`. A key event's `key` is what the key
means under the user's layout and its `code` is where the key sits (`KeyW`,
`ShiftLeft`), named after the US layout; bind a game's movement keys by
`code`, so they stay put on every layout.

Pointer events come from the mouse, a pen, or a finger, and carry what the
device reports under the web's names: `pointer_id`, `pointer_type` (`"mouse"`,
`"pen"`, or `"touch"`), `is_primary`, `buttons`, `pressure`,
`tangential_pressure`, `tilt_x`, `tilt_y`, `twist`, `width`, and `height`. A
pen's eraser end presses button `5`. Lumen's own widgets follow the primary
pointer; a second finger reaches only your handlers.

A drawing tool keeps a stroke by capturing the pointer in its `pointerdown`
handler:

```rust
fn on_down(ev: int) {
    let e = event(ev);
    e.current_target().set_pointer_capture(e.pointer_id());
}
```

From the pointer's next event until its last button comes up, its events go to
that element wherever the pointer moves, outside the window included, with
`gotpointercapture` before them and `lostpointercapture` after. A finger is
captured by the element it touched without asking.

A 3D view or a game takes the mouse with a pointer lock: the cursor hides and
stays put, and each move carries how far the mouse went in `movement_x` and
`movement_y`.

```rust
fn on_view_click(ev: int) {
    event(ev).current_target().request_pointer_lock();
}

fn on_view_move(ev: int) {
    let e = event(ev);
    orbit(e.movement_x(), e.movement_y());
}
```

The lock goes only to the element that has focus, during or just after a
press (the click that focused it counts), and Escape always gives the mouse
back, as does the window losing focus. `pointerlockchange` says when the lock
starts and ends, `pointerlockerror` when it was refused, and
`document::exit_pointer_lock()` ends it from a script. A page does not offer
pointer lock yet.

What a pen or a touch screen reports depends on the desktop. On Windows a pen
reports pressure, tilt, twist, its barrel button and its eraser, and a touch
screen reports pressure. On macOS and Linux a pen moves the mouse pointer and
reports neither pressure nor tilt; a Linux touch screen reports touches
without pressure, and macOS reports no touches. In a web build every reading
comes from the browser.

A module raises events of its own, under names of its own: a dock raises
`layoutchange` when its panels move and `panelclose` before a user closes one.
Bind them like any other event and read what they carry with `detail()`, a
map:

```rust
fn on_close(ev: int) {
    let e = event(ev);
    let reason = (e.detail() as {string: any})["reason"] as string;
    if unsaved() {
        e.prevent_default();
    }
}

fn on_ready() {
    get_by_id("editor").on("panelclose", "on_close");
}
```

They travel the same capture, target and bubble path as the built-in events.
Where a module asks a question with one, as `panelclose` asks whether the
panel may close, `prevent_default` is the answer no; the module's
documentation says which of its events take one. A built-in event's
`detail()` is an empty map.

A handler that raises an uncaught error stops at that line and the error is
reported; the app keeps running. What the handler did before the error stays
done, as it does in a browser: text it set, signals it wrote, timers it armed,
and lines it printed all apply.

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

A selector in `query` and `closest` tests live state the way a browser does:
`query(":hover")` is the element under the pointer and its ancestors,
`:focus` the focused element, and `:active`, `:checked`, `:selected`,
`:disabled` and `:drag-over` follow their states.

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
property it inherits, and a class removed takes with it whatever only it set. Reach for `set_style` only for a value that belongs to
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

## Windows and docking

An app has one main window, and `window::` reads and changes the window the
event being handled happened in: `window::set_title`, `window::set_size`,
`window::close`. Every other window is a panel in a window of its own, so the
functions that open, place and close windows are the `lumen-dock` module's
`dock::` functions, which take a panel's id:

```rust
fn on_click(id: string) {
    if id == "open-prefs" { dock::show("prefs"); }        // a dialog window
    if id == "pop-out" { dock::detach("inspector"); }     // a panel into its own window
    if id == "dock-back" { dock::attach("inspector"); }
}
```

A panel keeps its elements and their state wherever it shows, and its
elements stay in the app's one document, so `get_by_id` and the signals reach
them from a handler in any window. See [Docks and windows](docks.md) for
writing a docked workspace and dialog windows, and the
[catalog](../reference/scripting-candela.md#docks-and-windows) for every
function.

## Reaching the OS

Notifications, clipboard, file dialogs, menus, the tray, global hotkeys, audio,
and drag and drop are all script builtins. See
[OS integration](os-integration.md). HTTP lives here too: `fetch(url, tag)`
issues a GET without holding up a tick and calls `on_fetch(tag, body)` or
`on_fetch_error(tag, message)` when it completes. For another method, headers,
or a request body, `http(request)` takes the whole request and answers on
`on_http(tag, response)`.

### Downloading a file

`fetch` answers with a string held in memory and hands it to a handler in one
piece. When what you are after is a file, the `lumen-download` module streams
it to disk instead:

```toml
[dependencies]
lumen-download = { bundled = true }
```

```rust
download::to_file("https://example.com/pack.zip", "cache/pack.zip", "pack", "");
```

The call returns as soon as the transfer is running, and reports under the tag
it was given: `on_download_progress(tag, received, total)` while it runs, then
`on_download_done(tag, path)` or `on_download_error(tag, message)`. Give the
fourth argument a `sha256:` or `sha1:` digest and the file is verified before it
is put in place; give it an empty string to take whatever arrives. Either way the
destination is written by a rename at the end, so a transfer that fails leaves
no half file where the real one goes.

`download::cancel(tag)` stops a running transfer and reports it through
`on_download_error(tag, "cancelled")`, which is how a cancel button on a
download works.

Reach for it over `fetch` when the answer is large or binary, when you want it
on disk, when a progress bar has to move, or when you have a checksum to check
it against. The full surface is in the scripting reference for each host
([candela](../reference/scripting-candela.md#downloads),
[Rhai](../reference/scripting-rhai.md#downloads),
[Lua](../reference/scripting-lua.md#downloads)); Lua spells the calls
`download.to_file(..)` and `download.cancel(..)`.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

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
`files::list`, `files::mkdir`, `files::remove`, `files::copy`, byte-level
`files::read_bytes` and `files::write_bytes`, and `files::digest`, which
answers a file's md5, sha1 or sha256 digest. A call that cannot do what it
was asked answers `false` or an empty value and explains itself on stderr, so
a script branches on what it got back. The full surface is in the scripting
reference for each host ([candela](../reference/scripting-candela.md#filesystem),
[Rhai](../reference/scripting-rhai.md#filesystem),
[Lua](../reference/scripting-lua.md#filesystem)); Lua spells the calls
`files.read(..)`.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

## Unpacking archives

Content an app ships or downloads often arrives as one file: a theme pack, a
set of templates, an update. Unpacking it is a runtime module of its own:

```toml
[dependencies]
lumen-archive = { bundled = true }
```

That adds `archive::extract(src, dest, tag, opts)`, which reads zip, tar, and
gzip-compressed tar. Downloading and unpacking is the usual pairing, so the
call is built for it: extraction never happens inside the call, because an
archive of any size would hold up the frame. The call answers with whether the
job was taken, and the result arrives later as an event named by the tag you
passed:

```rust
fn on_start() {
    archive::extract("themes.zip", "themes", "themes", archive::ExtractOptions::default());
}

fn on_archive_done(tag: string, dest: string, count: int) {
    signal<string>("status").set("unpacked " + string(count));
}
fn on_archive_error(tag: string, message: string) {
    signal<string>("status").set(message);
}
```

Both paths resolve against the app directory, the same as everywhere else.
`on("archive_done", tag, fn)` handles one job on its own, so several
extractions in flight stay apart.

`opts` picks which files to keep. A launcher unpacking a jar of native
libraries wants the libraries and not the jar's metadata, so it names them
and nothing else reaches the disk:

```rust
archive::extract("natives.jar", "natives", "natives",
    archive::ExtractOptions { include: ["*.so", "*.dll", "*.dylib"] });
```

An archive from the network is untrusted input, and the module treats it that
way. An entry that would write outside `dest`, by naming an absolute path or
climbing out with `..`, ends the whole extraction with an error naming it
rather than being quietly passed over; symbolic and hard links are skipped,
because a link can point outside the destination once extraction is over. The
container is decided by the file's leading bytes, so an archive saved under
the wrong name still unpacks. The full rules are in the scripting reference
for each host ([candela](../reference/scripting-candela.md#archives),
[Rhai](../reference/scripting-rhai.md#archives),
[Lua](../reference/scripting-lua.md#archives)); Lua spells the call
`archive.extract(..)`.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

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
ahead of time to an artifact is the exception, and declares the namespace itself,
unless the function comes from a module the app declares under `[dependencies]`.

Such a function can fail. It raises where the script called it, carrying the
message the Rust side gave and naming the function, so you catch it the way the
language catches anything else: `try`/`catch` in Rhai, `pcall` in Lua, and
`catch "host_fn_error"` in candela. An uncaught failure ends that one call and is
reported like any other script error; the app keeps running.

The script engine itself can give up the same way, on a desktop or a server
run: the call it was in ends and is reported, and the app or the render server
keeps running, with the script stopped for the rest of that run rather than the
process taken down with it.

What is available is up to the app's Rust, so look in its source, not here.
[FFI and SDKs](../reference/ffi.md) covers exposing one, and
[Writing plugins](../contributing/plugins.md) covers doing it from a plugin.

## Hot reload

`lumenc run` watches every file the app loads. Editing markup, CSS, an included
fragment, a stylesheet import, a script, a translation catalogue, or an image or
SVG the app shows updates the running window without restarting it.

What survives an edit:

- Signal values, including array signals and anything derived from them.
- Per-element state of anything carrying an `id`: typed text and cursor,
  toggle and slider values, scroll positions. Elements without an `id` have no
  stable name to match on and start fresh. Text still as the markup wrote it
  takes your edit; text a user typed or a script set is kept.
- Handler and derivation registrations. A handler registered from `on_start`
  keeps working even though `on_start` does not run again; anything the new
  source registers replaces its match.

Elements a script created through the DOM API are rebuilt rather than
preserved: the tree is respawned from markup and `on_ready` fires again, so the
script recreates them the way it did at first mount.

An error keeps the app alive. A markup or CSS problem leaves the last working
version running, and a script that fails to compile leaves the previously
loaded script in place. Either way the error shows on stderr and in a banner
across the top of the window, which the next good save clears and Esc
dismisses. When the error names a place in a file, the banner reads
`file:line:col` and shows that line with a caret under the column:

```text
hot-reload: candela script: Cannot find function not_there in this scope
 --> /home/me/myapp/src/main.cdl:4:5
  |
4 |     not_there(n);
  |     ^
```

The banner's text is selectable: drag across it and copy with the platform
shortcut, or press Copy to put the whole report on the clipboard. Markup and
CSS errors do not name a file position yet; their banner shows the parser's
message. The banner takes its look from the `--lumen-error-bg`,
`--lumen-error-text`, `--lumen-error-field-bg`, `--lumen-error-button-bg`,
`--lumen-error-font`, `--lumen-error-selection` and
`--lumen-error-selection-text` tokens, so a `:root` rule restyles it.

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
