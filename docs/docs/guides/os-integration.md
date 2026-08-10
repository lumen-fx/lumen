# OS integration

Menus, a tray icon, notifications, global hotkeys, file dialogs, the clipboard,
drag and drop, and audio. Some of these are markup, the rest are script calls
with a callback.

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
fn handle_open_menu(id) { open_menu("actions"); }

fn on_start() {
    on("click", "open-actions-menu", "handle_open_menu");
    on("menu", "rename", "handle_menu_action");
    on("menu", "delete", "handle_menu_action");
}
```

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

Only a left click is reported, and only on macOS and Windows; on Linux the icon
appears but its clicks do not reach the app. On GNOME the icon needs the
AppIndicator extension to be visible at all.

## Notifications

```rhai
notify("Lumen", "Export finished.");
```

A title and a body, delivered to the desktop's notification service. There are
no action buttons and no callback when someone clicks the toast.

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

If another application already holds a chord, the registration is skipped and
your name stays unbound; the app keeps running. On Linux, global hotkeys need
X11, so a pure Wayland session without XWayland has none.

## File dialogs

Every dialog call takes a `tag` you choose, and the answer comes back on a
callback carrying that tag:

```rhai
fn handle_open(id) { pick_file("import"); }

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

Native file dialogs do not open on macOS in a markup app; the call returns an
empty result immediately.

## Clipboard

Text copy, cut, and paste work inside text inputs with the usual keyboard
shortcuts, with nothing for you to wire.

Images have a script surface:

```rhai
copy_image("shots/graph.png");        // put a PNG on the clipboard
save_clipboard_image("shots/in.png"); // write the clipboard image to disk
```

There is no script call for clipboard text.

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

One track plays at a time:

```rhai
fn on_start() { audio_play("music/track.ogg"); }

fn toggle(id) { audio_pause(); }
```

The transport is `audio_play(path)`, `audio_pause()`, `audio_resume()`,
`audio_stop()`, `audio_seek(seconds)`, and `audio_volume(level)`.

Playback state is published as signals you can bind to without polling:

- `audio_position` - seconds elapsed
- `audio_duration` - track length in seconds
- `audio_playing` - `true` or `false`

```html
<label bind-text="audio_position"/>
```

`on_audio_end()` runs once when a track finishes, which is where you advance a
playlist.

WAV and Ogg Vorbis decode. On a machine with no working audio device the calls
succeed and the position keeps advancing, with nothing audible.

Audio initialises automatically when your app mentions it. Force it either way
with `[runtime] audio` in `lumen.toml`, and drop it from a static bundle with
`[capabilities] audio`; see the
[lumen.toml reference](../reference/lumen-toml.md).
