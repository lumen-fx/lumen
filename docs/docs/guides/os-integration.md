# OS integration

Menus, a tray icon, notifications, global hotkeys, file dialogs, the clipboard,
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
    if id == "file-quit" { /* ... */ }
}
```

Or route one item to its own function:

```rhai
on("menu", "file-open", "handle_open");
```

Linux has no native menu bar. Build an in-window one there.

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
`on_start` runs before that, when a lookup finds nothing. Menu items are not
elements, so they route by id with `on("menu", ...)`.

`close_menu(id)` closes it; picking an item closes it for you. Items reach the
same `on_menu` handler as the native bar. Style the panel and its rows with the
`.menu-panel`, `.menu-item`, and `.menu-separator` classes.

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

## Drag and drop

### Files dropped from the desktop

Mark the element that accepts them and handle the drop:

```html
<tile id="drop-zone" drop-target="true" text="Drop a file here"/>
```

```rhai
fn on_file_dropped(id, path) {
    if id == "drop-zone" { set_text("echo", "Dropped: " + path); }
}
```

`drop="true"` is accepted as an older spelling of the same thing.

### Dragging inside the app

Give the source a payload and the destination a drop target:

```html
<tile id="card-3" drag-payload="card-3" text="Card 3"/>
<column id="done" drop-target="true" accept="text/plain"/>
```

An empty `drag-payload=""` uses the element's `id` as the payload, and inside a
`<for>` you can build it per row with a `{row.field}` placeholder. `accept`
filters by MIME type; leave it out to accept anything.

```rhai
fn on_drag_start(source_id, payload) { }
fn on_drop(target_id, payload) { }
```

Both have per-id `on(...)` forms, `on("drag_start", ...)` and `on("drop", ...)`.

Style the destination while a drag hovers it:

```css
.column:drag-over { bg: #2a3550; }
```

Adding `draggable="true"` also moves the element under the pointer while it is
dragged, which suits a card you want to see follow the cursor.

Files dropped onto your window from other applications work. Dragging out of
your window into another application does not.

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
succeed and the position keeps advancing, with nothing audible.

## Running a program

Running another program is a runtime module. Declaring it in `lumen.toml` is
what makes `process::start` exist:

```toml
[dependencies]
lumen-process = { bundled = true }
```

Start a program with the arguments it takes and a tag you choose. The tag is
how you tell one child from another when its output comes back:

```rhai
fn on_ready() {
    signals.status.set("building");
    process::start("./tools/build.sh", ["--release"], "build");
}
```

`./tools/build.sh` is a program the app ships: a `cmd` with a path separator in
it resolves against the app directory, and a bare name like `git` is looked up
on `PATH`. Either way the child runs in the app directory.

The call answers as soon as the program is running, and `false` when it could
not start at all. Branch on that, because a program that never started sends
nothing afterwards:

```rhai
fn on_ready() {
    if !process::start("git", ["status", "--short"], "git") {
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

candela is the same call, with the handler's parameters annotated:

```rust
fn on_process_exit(tag: string, code: int) {
    if code == 0 {
        lumen::signal_set("status", "done");
    } else {
        lumen::signal_set("status", "failed");
    }
}
```

There is no way to write to a child's input and no way to end one from a
script. A child is also not ended when the app exits: a program still running
outlives the app that started it, so start a long-running one deliberately.
