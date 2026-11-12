# OS integration

Menus, commands and keyboard shortcuts, a tray icon, notifications, global
hotkeys, file dialogs, the clipboard,
drag and drop, opening links, keeping the machine awake, recent files,
autostart, single-instance launches, and audio. Some of these are markup, the
rest are script calls with a callback.

Examples here are written in Rhai. Lua uses the same names with Lua syntax;
candela puts them in the `lumen` namespace, so `notify(...)` becomes
`lumen::notify(...)`. Full per-host signatures are in the
[candela](../reference/scripting-candela.md),
[Rhai](../reference/scripting-rhai.md), and
[Lua](../reference/scripting-lua.md) references.

## Menus

### Native menu bar

Declare it in markup, as a direct child of `<root>`:

```html
<root>
  <menubar>
    <menu label="File">
      <menuitem id="file-open" label="Open..." accel="CommandOrControl+O"/>
      <separator/>
      <menuitem id="file-quit" label="Quit"/>
    </menu>
    <menu label="Help">
      <menuitem id="help-about" label="About"/>
    </menu>
  </menubar>

  <column>...</column>
</root>
```

A `menu` needs a `label`; a `menuitem` needs an `id` and takes an optional
`label` (defaulting to the id) and an optional `accel`. One menu bar per app,
declared on the home page.

Selecting an item calls `on_menu`:

```rhai
fn on_menu(id) {
    if id == "file-quit" { app.quit(); }
}
```

Or route one item to its own function:

```rhai
on("menu", "file-open", "handle_open");
```

Linux desktops have no native menu bar, so there the same markup is drawn in
the window: a bar along the top with one button per menu. Its items reach
`on_menu` the same way, and their `accel` shortcuts work while the window has
focus. A shortcut a focused text field uses (typing, Ctrl+Z, Ctrl+C and the
other editing chords) stays with the field; any other chord reaches the bar. The skin styles it, and an app can restyle the `.menubar` and
`.menubar-button` classes and the `menu` and `menuitem` tags like any other
element.

To draw the bar in the window on macOS and Windows too, so an app with its
own theme looks the same everywhere, set the `menubar` key in `lumen.toml`:

```toml
[window]
menubar = "window"
```

The OS bar then shows no menus of the app's.

### In-window menus

A `menu` outside a `menubar` is a popup panel you open yourself, and it works
on every platform:

```html
<button id="open-actions-menu" text="Actions"/>

<menu id="actions">
  <menuitem id="rename" label="Rename"/>
  <menuitem id="duplicate" label="Duplicate" disabled="true"/>
  <separator/>
  <menuitem id="delete" label="Delete"/>
</menu>
```

```rhai
fn handle_open_menu(ev) { open_menu("actions"); }

fn on_ready() {
    get_by_id("open-actions-menu").on("click", Fn("handle_open_menu"));
    on("menu", "rename", "handle_menu_action");
    on("menu", "delete", "handle_menu_action");
}
```

Bind element events from `on_ready`, which runs once the tree is mounted;
`on_start` runs before that, when a lookup finds nothing. A picked item routes
by its id with `on("menu", ...)`, the same way a menu bar item does.

`close_menu(id)` closes it; picking an item closes it for you. Items reach the
same `on_menu` handler as the native bar. Style the panel and its rows with the
`menu`, `menuitem` and `separator` tags.

The menu and its items are elements from the first frame, so a menu that lists
something the app keeps, recent files or saved layouts, changes with the DOM
calls you use for any element:

```rust
import "lumen.cdl";

fn add_layout(id: string, name: string) {
    let item = create("menuitem");
    item.set_id("layout-" + id);
    item.set_attr("label", name);
    get_by_id("window").append(item);
}

fn rename_layout(id: string, name: string) {
    get_by_id("layout-" + id).set_attr("label", name);
}

fn main() {}
```

Or keep the items in a list signal and let a `<for>` in the menu build them:

```html
<menu id="window">
  <menuitem id="reset-layout" label="Reset layout"/>
  <separator/>
  <for each="layouts" key="id">
    <menuitem id="layout-{id}" label="{name}"/>
  </for>
</menu>
```

An item's `accel` shows its shortcut at the end of the row, written the way the
platform's menus write one: `Ctrl+Z` on Linux and Windows, the Command symbol
followed by `Z` on macOS. A popup menu only shows the shortcut; to have the key
run it, make the item a [command](#commands-and-shortcuts). The menu bar's
menus show their shortcuts the same way, and run them too. The `.menu-accel`
class styles the shortcut.

### Context menus

A right click raises the `contextmenu` event on the element under the
pointer, as it does in a browser. The ContextMenu key and Shift+F10 raise it on
the focused element, and so does a screen reader asking for the element's
menu. Answer it by opening a `menu` at the event's position:

```html
<column id="canvas" grow="1"/>

<menu id="canvas-menu">
  <menuitem id="cut" label="Cut"/>
  <menuitem id="copy" label="Copy"/>
  <menuitem id="paste" label="Paste"/>
</menu>
```

```rust
import "lumen.cdl";

fn show_canvas_menu(ev: int) {
    let e = event(ev);
    e.prevent_default();
    lumen::open_menu_at("canvas-menu", e.client_x(), e.client_y());
}

fn on_ready() {
    get_by_id("canvas").on("contextmenu", "show_canvas_menu");
}

fn main() {}
```

The menu opens with its top-left corner at the point, and to the left of or
above it where it would cross the window edge. For a keyboard request the
position is just below the element's left edge. `prevent_default` matters in a
web build, where it keeps the browser's own menu from opening; a desktop window
has no menu of its own to suppress. Declare the menu near the root, outside any
scrolled container, so the point it opens at is the point you passed.

A Rust app reads the same requests as `ContextMenuEvent` messages and opens the
menu with `Signals::open_menu_at`.

## Commands and shortcuts

A command is a named action with keyboard shortcuts: Save on Ctrl+S, the
command palette on Ctrl+Shift+P. Declare it once, and its keys, its menu items
and a searchable palette all run it, and the user can rebind its keys. Add the
`lumen-commands` module to `lumen.toml`:

```toml
[dependencies]
lumen-commands = { bundled = true }
```

Declare the commands in markup:

```html
<commands>
  <command id="file.save" label="Save" keys="CommandOrControl+S"/>
  <command id="file.export" label="Export" keys="CommandOrControl+Shift+E" enabled="false"/>
  <command id="edit.redo" label="Redo" keys="CommandOrControl+Y, CommandOrControl+Shift+Z"/>
</commands>
```

`keys` is one chord or a comma-separated list. `CommandOrControl` is Command
on macOS and Control elsewhere. `<commands>` shows nothing; give a `<command>`
a `translatable` key to translate its label like any element's text.

Whatever runs a command, a key, a menu item, the palette or a call, the app
hears one `command` event on the document, with the command's id in its
detail:

```rhai
fn on_ready() {
    document_node().on("command", |e| {
        if e.detail().id == "file.save" { save(); }
    });
}
```

A script makes commands at run time and drives them through the `commands`
namespace: `commands::register(id, label, keys)`, `commands::unregister(id)`,
`commands::run(id)` and `commands::set_enabled(id, on)`. A disabled command
does not run, shows greyed out in its menu items, and is left out of the
palette.

### Which keys reach a command

The first enabled command bound to a key runs, wherever focus is, with two
exceptions. A focused text field keeps what it uses: typing, Shift with a key,
and its editing chords (Ctrl or Command with A, C, X, V, Z or Y, the arrows,
Home, End, Backspace and Delete). Every other chord reaches the commands, so
Ctrl+S saves while the user types. A focused element that takes every key,
a `<surface keyboard="all">` or one holding the pointer lock, keeps them all.
A held key's repeats do not run a command again.

### Menu items

A menu item that names a command takes its label, its shortcut and its
greyed-out state from it, and a click on it runs the command:

```html
<menu id="file">
  <menuitem command="file.save"/>
  <menuitem command="file.export"/>
</menu>
```

An item with a `command` and no `id` takes the command's id. This works in
popup menus and in the menu bar Lumen draws in the window, where a chord a
command answers runs the command once rather than the item too. The OS menu bar on
macOS and Windows is built once from the markup: there an item with a
`command` runs it when clicked, but shows the label and `accel` the markup
gave it rather than the command's.

### The command palette

The palette lists every enabled command with its shortcut and narrows the list
as the user types, keeping the commands whose label holds every word typed.
The arrow keys move the highlight, Enter runs the highlighted command, and
Escape closes it. CommandOrControl+Shift+P opens it, and so does
`commands::open_palette()`; its own command is `commands.palette`, so it can be
rebound like any other. Place it with `<command-palette/>`, which opens it near
the top of the window; an app that writes none gets one there. The
`--lumen-command-palette-*` tokens style it (see the
[tag reference](../reference/tags.md#commands-command-and-command-palette)).

### Rebinding keys

`commands::bind(id, keys)` binds a command to new keys, `commands::reset(id)`
gives it its declared ones back, and `commands::keys(id)` reads what runs it
now. `commands::record(id)` binds the next chord the user presses, which is
how a settings screen asks for one; Escape cancels. A chord another command
had moves to this one. Each change raises `keyschange` on the document with
the command's `id` and its new `keys`.

The user's bindings are kept in `keymap.json` in the app's data directory and
come back on the next run. Set `persist = "none"` on the module to keep them
for one run only:

```toml
[dependencies]
lumen-commands = { bundled = true, config = { persist = "none" } }
```

In a page, the bindings are kept in the browser's local storage, a command
also raises `command` on the JavaScript `document`, and a script hears it as
`on_command(id)` (and a binding change as `on_keyschange(id, keys)`) rather
than through `document_node().on`.

## System tray

```rhai
fn on_start() {
    tray_icon("main", "icons/tray.png", "Lumen hotkeys demo");
}

fn on_tray(id) {
    // the icon was clicked
}
```

The icon is a PNG path relative to the app directory. An empty tooltip leaves
the icon untitled. `unregister_tray(id)` removes it.

Only a left click is reported. On GNOME the icon needs the AppIndicator
extension to be visible at all.

### A tray context menu

`tray_icon_menu` adds a right-click menu and the macOS template-image flag:

```rhai
tray_icon_menu("main", "icons/tray.png", "Lumen", "show:Show|-|quit:Quit", true);
```

The menu is a list of `id:Label` entries separated by `|`, where `-` is a
separator. Picking one calls `on_menu(id)`, the same handler the menu bar uses,
so `on("menu", "quit", "handle_quit")` routes a single item.

The last argument is the macOS template flag: pass `true` for a monochrome icon
you want recoloured for the light or dark menu bar, `false` for a full-colour
icon. It is ignored elsewhere.

## Notifications

```rhai
notify("Lumen", "Export finished.");
```

A title and a body, delivered to the desktop's notification service.

For an icon, an urgency, and buttons, use `notify_ex`:

```rhai
notify_ex("export-done", "Lumen", "Export finished.",
          "icon:document-save|urgency:critical", "open:Open|dismiss:Dismiss");

fn on_notification_action(id, action_id) {
    if id == "export-done" && action_id == "open" { /* ... */ }
}
```

The first argument is an id you choose; it comes back on the callback. The
fourth is a settings list of `key:value` entries separated by `|`: `icon` takes
a themed icon name or a path, and `urgency` takes `low`, `normal`, or
`critical`. The fifth is the buttons, in the same `id:Label|id2:Label2` shape as
the tray menu. An empty string in either position means the defaults.

Button presses report back on Linux and the BSDs. On macOS and Windows the
buttons render but their presses do not reach the app.

Set `[app] id` in `lumen.toml` so notifications are attributed to your app:
Windows keys toasts off it and macOS treats it as the bundle id.

## Global hotkeys

A hotkey fires even when your window is not focused.

```rhai
fn on_start() {
    register_hotkey("save", "CommandOrControl+Shift+L");
    on("hotkey", "save", "handle_save");
}

fn handle_save(name) { /* ... */ }
```

`on_hotkey(name)` catches every hotkey if you would rather branch yourself.
Accelerators are written in the Electron style: `CommandOrControl+S`,
`Alt+Space`, `F11`. Registering a name that already exists replaces it, and
`unregister_hotkey(name)` releases it.

Releasing the chord calls `on_hotkey_release(name)`, so one hotkey can drive
push-to-talk:

```rhai
fn on_hotkey(name)         { if name == "talk" { start_capture(); } }
fn on_hotkey_release(name) { if name == "talk" { stop_capture(); } }
```

If another application already holds a chord, the registration is skipped and
your name stays unbound; the app keeps running. On Linux, global hotkeys need
X11, so a pure Wayland session without XWayland has none.

## File dialogs

Every dialog call takes a `tag` you choose, and the answer comes back on a
callback carrying that tag:

```rhai
fn on_ready() { get_by_id("open").on("click", Fn("handle_open")); }

fn handle_open(ev) { pick_file("import"); }

fn on_file_picked(tag, path) {
    if path == "" { return; }   // cancelled
    set_text("status", "opened " + path);
}
```

The calls are `pick_file(tag)`, `pick_files(tag)`, `pick_folder(tag)`,
`save_file(tag, default_name)`, and `pick_file_filtered(tag, spec)`, where a
filter spec looks like `Images:png,jpg,webp|All:*`.

Results arrive on `on_file_picked(tag, path)` for a single file or a save,
`on_files_picked(tag, paths)` for a multi-select with the paths joined by `|`,
and `on_folder_picked(tag, path)` for a directory. Each also has an `on(...)`
form: `on("file_picked", "import", "handle_import")`.

Cancelling still calls back once, with an empty path, so you can clear a
loading state.

A Rust app opens the same dialogs with `FileDialog`, which also takes a
starting folder, and gets the result in a callback; see
[Dialogs and the clipboard](../reference/ffi.md#dialogs-and-the-clipboard).

## Clipboard

Text copy, cut, and paste work inside text inputs with the usual keyboard
shortcuts, with nothing for you to wire.

For clipboard text under your own control:

```rhai
fn on_ready() {
    get_by_id("copy").on("click", Fn("handle_copy"));
    get_by_id("paste").on("click", Fn("handle_paste"));
}

fn handle_copy(ev) { clipboard_write("copied from Lumen"); }

fn handle_paste(ev) { clipboard_read("editor"); }

fn on_clipboard(tag, text) {
    if tag == "editor" { set_text("field", text); }
}
```

`clipboard_write(text)` is immediate. `clipboard_read(tag)` is a request: the
clipboard lives on the OS side, so the text arrives on `on_clipboard(tag, text)`
on the next tick. A clipboard holding no text still calls back once, with an
empty string. `on("clipboard", "editor", "handle_paste_result")` routes one tag.

Images have their own pair:

```rhai
copy_image("shots/graph.png");        // put a PNG on the clipboard
save_clipboard_image("shots/in.png"); // write the clipboard image to disk
```

Both paths are relative to the app directory. Neither call reports back; a
failure logs to stderr. A Rust app reads and writes clipboard images as pixels
in memory instead, with no file in between; see
[Dialogs and the clipboard](../reference/ffi.md#dialogs-and-the-clipboard).

## Opening links and files

Hand something to the platform's default handler:

```rhai
open_url("https://lumenfx.dev");   // default browser, or mail client for mailto:
open_path("reports/q3.pdf");       // default application for the file type
reveal_path("reports/q3.pdf");     // show it in Finder, Explorer, or Files
```

Paths are relative to the app directory. These calls do not report back; a
failure logs to stderr.

## Keeping the machine awake

While a long job runs, hold off the screensaver and system sleep:

```rhai
fn on_ready() { get_by_id("export").on("click", Fn("start_export")); }

fn start_export(ev)  { keep_awake("export", "Exporting video"); }

fn on_export_done()  { allow_sleep("export"); }
```

The name pairs the two calls, so several jobs can hold their own request.
Repeating a live name replaces its request rather than stacking a second one.
The reason string is what the platform's power settings show. Nothing is held
after the app exits.

## Recent files

Keep a per-app "recently opened" list, most recent first:

```rhai
fn on_file_picked(tag, path) {
    if path == "" { return; }
    add_recent_file(path, "");   // empty label derives one from the file name
}

fn on_ready() { list_recent_files("startup"); }

fn on_recent_files(tag, paths) {
    if tag == "startup" { /* paths is joined by "|" */ }
}
```

`add_recent_file(path, label)` records a path, moving it to the front if it
was already there; the list caps at 32 entries, dropping the oldest.
`list_recent_files(tag)` is a request - the answer arrives on
`on_recent_files(tag, paths)` with the paths joined by `|`, most recent
first. `clear_recent_files()` empties the list. The list is stored per app
under the platform's user-data directory, the same root `data_dir()` uses.

## Autostart

```rhai
fn on_ready() {
    get_by_id("autostart-toggle").on("click", Fn("handle_toggle"));
    query_autostart("startup-toggle");
}

fn handle_toggle(ev) { set_autostart(true); }

fn on_autostart_enabled(tag)  { set_text("autostart-toggle", "Autostart: on"); }
fn on_autostart_disabled(tag) { set_text("autostart-toggle", "Autostart: off"); }
```

`set_autostart(on)` writes (or removes) the platform's login-item entry: a
`.desktop` file under `~/.config/autostart` on Linux, a LaunchAgent plist on
macOS, or a `Run` registry value on Windows. `query_autostart(tag)` reads the
current state back, split across `on_autostart_enabled(tag)` /
`on_autostart_disabled(tag)` rather than a boolean argument, the same way
dialog results split into `on_dialog_accepted` / `on_dialog_rejected`.

## Single-instance launches

Set `single_instance = true` under `[app]` in `lumen.toml` to make a second
launch forward its command-line arguments to the already-running window and
exit, instead of opening a second one:

```toml
[app]
single_instance = true
```

```rhai
fn on_second_instance(args) {
    // args is the second launch's argv, joined by "|"
}
```

Off by default: most apps are fine with more than one window open, and
holding the lock (a bound socket on Linux/macOS, a named pipe on Windows) is
a real side effect an app should opt into rather than acquire by surprise.
The lock is windowed-run only - a headless run (`lumenc run --headless`,
the Rust SDK's non-interactive embedding) never takes it, so nothing there
contends with a real running instance over the same socket.

On Linux/macOS the socket is scoped to `$XDG_RUNTIME_DIR`, which the XDG
spec requires to be private to your user, and is locked to your user alone
right after it binds. When `$XDG_RUNTIME_DIR` is unset, the app runs without
the lock rather than falling back to a shared directory another local user
could reach.

On Windows the pipe is granted to your user alone, and it rejects clients
from other machines. The first launch claims the name; a later launch finds
it taken and forwards its arguments there. The name is scoped to the machine
rather than to your user, so a second person signed in to the same machine
finds the lock held by the first and their launch exits without opening a
window. If the pipe's permissions cannot be resolved, the app runs without
the lock rather than with permissions it did not choose.

## Drag and drop

### Dragging inside the app

In-app drags work in a desktop app only: they do not reach a web page yet
(see [the web target's limits](web.md#known-limits)).

Give the source a payload and the destination a drop target:

```html
<tile id="card-3" drag-payload="card-3" text="Card 3"/>
<column id="done" drop-target="true"/>
```

An empty `drag-payload=""` uses the element's `id` as the payload, and inside a
`<for>` you can build it per row with a `{row.field}` placeholder.

The payload is `text/plain` unless `drag-type` names another MIME type, and a
target's `accept` lists the types it takes, comma-separated, with `type/*`
wildcards, as HTML `<input accept>` spells it. Leave `accept` out to take
anything:

```html
<tile drag-payload="{row.path}" drag-type="application/x-asset" text="{row.name}"/>
<column id="viewport" drop-target="true" accept="application/x-asset, image/*"/>
```

A target only lights up and only takes the drop when its list names one of the
drag's types, so a scene view that wants assets never highlights for a text
drag. Style it while an acceptable drag hovers it:

```css
.column:drag-over { bg: #2a3550; }
```

The drop arrives as the DOM drag events, which bubble like the web's:
`dragstart` and `dragend` on the source, and `dragenter`, `dragover`,
`dragleave` and `drop` on the target under the pointer. Read the payload by
type; `ev.x()` / `ev.y()` are where it landed on the target as you see it,
scrolling included:

```rust
fn on_viewport_drop(ev: int) {
    let path = lumen::event_get_data(ev, "application/x-asset");
    place_asset(path, lumen::event_x(ev), lumen::event_y(ev));
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("viewport"), "drop", "on_viewport_drop");
}
```

`lumen::event_types(ev)` lists the types a drag carries. A `dragstart` handler
can add more with `lumen::event_set_data(ev, type, value)`, and a target's
`accept` list sees them; its `prevent_default` cancels the drag. Rhai and Lua
spell these `e.types()`, `e.get_data(type)` and `e.set_data(type, value)`.

Two global functions are the short form, the way `on_click` sits beside
`click`. They receive the payload's `text/plain` value, or its only value when
it has no text:

```rust
fn on_drag_start(source_id: string, payload: string) { }
fn on_drop(target_id: string, payload: string) { }
```

Both have per-id `on(...)` forms, `on("drag_start", ...)` and `on("drop", ...)`.

Adding `draggable="true"` also moves the element under the pointer while it is
dragged, which suits a card you want to see follow the cursor.

To place a drop on a `<surface>` in the producer's own pixels, pass the drop's
window position to `surface::to_texture`; see
[Surfaces](surfaces.md).

### Files dropped from the desktop

A file dropped on a drop target from another application is a `drop` event
carrying `text/uri-list`, and `on_drop` receives the same uri:

```rust
fn on_zone_drop(ev: int) {
    lumen::signal_set("dropped", lumen::event_get_data(ev, "text/uri-list"));
}
```

`accept="text/uri-list"` makes a target take files and nothing else.
`on_file_dropped(id, path)` still fires with the plain path; it is deprecated
in favour of the `drop` event and goes in the next release. `drop="true"` is
accepted as an older spelling of `drop-target`.

Dragging out of your window into another application is not supported, and
neither is a drag from one of your app's windows to another.

## Audio

Audio is a runtime module. Declaring it in `lumen.toml` is what makes the
`audio_*` functions exist:

```toml
[dependencies]
lumen-audio = { bundled = true }
```

Without the declaration there is no audio surface at all: a script calling
`audio_play` gets the host's ordinary unknown-function error, the same as any
other name nothing registered. A statically built app compiles the module's
plugin in instead of loading it.

One track plays at a time:

```rhai
fn on_ready() {
    audio_play("music/track.ogg");
    get_by_id("pause").on("click", Fn("handle_pause"));
}

fn handle_pause(ev) { audio_pause(); }
```

The transport is `audio_play(path)`, `audio_pause()`, `audio_resume()`,
`audio_stop()`, `audio_seek(seconds)`, and `audio_volume(level)`. Track
paths resolve like any asset: relative to the app directory, out of the
app's packed archive when it ships one, and a `lumen://app/...` URI names a
packed track directly.

Playback state is published as signals you can bind to without polling:

- `audio_position` - seconds elapsed
- `audio_duration` - track length in seconds
- `audio_playing` - `true` or `false`

```html
<label bind-text="audio_position"/>
```

`on_audio_end(path)` runs once when a track finishes, with the path you
passed to `audio_play`; that is where you advance a playlist. A per-track
handler registered with `on("audio_end", path, "fn_name")` wins over it.

WAV and Ogg Vorbis decode. On a machine with no working audio device the calls
succeed and the position keeps advancing, with nothing audible. A headless run
opens no output device at all and behaves the same way: the transport, the
position, and `on_audio_end` all work, and nothing is audible.

## Running a program

Running another program is a runtime module. Declaring it in `lumen.toml` is
what makes `process::start` exist:

```toml
[dependencies]
lumen-process = { bundled = true }
```

Start a program with the arguments it takes, a tag you choose, and its
options. The tag is how you tell one child from another when its output comes
back:

```rhai
fn on_ready() {
    signals.status.set("building");
    process::start("./tools/build.sh", ["--release"], "build", #{});
}
```

`./tools/build.sh` is a program the app ships: a `cmd` with a path separator in
it resolves against the app directory, and a bare name like `git` is looked up
on `PATH`.

The options are the fourth argument, and every field has a default, so pass
only what you change:

| Field | Default | What it does |
| --- | --- | --- |
| `cwd` | `""` | The directory the child starts in, relative to the app directory. Empty is the app directory itself. |
| `env` | empty | Variables set for the child on top of the environment it inherits. |
| `end_at_exit` | `false` | End the child when the app exits. By default a child outlives the app that started it. |

In Rhai and Lua the options are a map, and a key you leave out takes its
default. In candela they are the `process::StartOptions` struct; set the
fields you want and end the literal with a bare `..` to take the rest from the
defaults, or pass `process::StartOptions::default()` for all of them:

```rust
fn launch() -> bool {
    return process::start("java", ["-jar", "game.jar"], "game",
        process::StartOptions { cwd: "instances/a", .. });
}

fn status() -> bool {
    return process::start("git", ["status", "--short"], "git", process::StartOptions::default());
}
```

A field the options do not have, or a value of the wrong type, is an error in
the script before anything starts.

The call answers as soon as the program is running, and `false` when it could
not start at all. Branch on that, because a program that never started sends
nothing afterwards:

```rhai
fn on_ready() {
    if !process::start("git", ["status", "--short"], "git", #{}) {
        signals.status.set("git is not installed");
    }
}
```

Output arrives a line at a time, and the exit is always the last thing you hear
about a tag:

```rhai
fn on_process_stdout(tag, line) {
    signals.log.set(signals.log.get() + line + "\n");
}

fn on_process_exit(tag, code) {
    if code == 0 {
        signals.status.set("done");
    } else {
        signals.status.set("failed");
    }
}
```

`code` is the program's own exit code, or 128 plus the signal that ended it.
Route one child to handlers of its own with
`on("process_exit", "build", "build_done")`, the same per-key routing every
event has.

candela's handlers take the same arguments, annotated:

```rust
fn on_process_exit(tag: string, code: int) {
    if code == 0 {
        lumen::signal_set("status", "done");
    } else {
        lumen::signal_set("status", "failed");
    }
}
```

`process::stop(tag)` ends the program running under a tag and answers whether
there was one. On Linux and macOS the child gets `SIGTERM` first, so it can
save its state, and is killed if it is still running two seconds later; on
Windows it is ended at once. The stop also reaches every program the child
started, so a shell script's commands end with the shell. Its
`on_process_exit` still arrives, as the last event for the tag. When several children share a tag, `stop` ends all of them.

A child started with `end_at_exit` is ended the same way when the app closes,
and the app waits for it to go before it exits. An app that is killed rather
than closed leaves its children running.

There is no way to write to a child's input.
