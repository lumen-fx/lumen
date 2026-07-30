# Rhai builtins

Scripts opt in by writing `main.rhai` next to `main.lmn` (or pulling in
additional files via `<script src="…rhai" />`). The host injects a
fixed set of builtins on top of Rhai's standard library — they're the
bridge between scripts and the ECS world.

The authoritative list is `register_fn(...)` calls in
[`lumen/script-rhai/src/lib.rs`](https://github.com/lumen-ui/lumen/blob/main/lumen/script-rhai/src/lib.rs).
External Rhai LSPs treat these as unknown functions; point your editor
at [`docs/lumen-rhai-builtins.rhai`](https://github.com/lumen-ui/lumen/blob/main/docs/lumen-rhai-builtins.rhai)
to silence the warnings.

Builtins are grouped by category below. Each entry shows the signature,
a one-line description, and a sample.

## Lifecycle callbacks

These aren't builtins — they're **functions you write** that the
runtime calls by convention.

| Function | When called |
|---|---|
| `on_start()` | Once after the layout tree spawns. Register handlers / signals here. |
| `on_click(id)`, `on_double_click(id)`, `on_long_press(id)` | Global pointer dispatchers. Per-id handlers (`on("click", id, fn)`) skip the global for that pair. |
| `on_text_input(id, text)` | IME commit on `<input>` / `<textarea>`. `text` is the committed substring. |
| `on_toggle(id, checked)` | Toggle state change. |
| `on_slider(id, value)` | Slider drag commit. `value` is f64. |
| `on_file_dropped(id, path)` | Dropped onto a `<… drop="true">` target. |
| `on_file_picked(tag, path)` / `on_files_picked(tag, paths_joined_by_pipe)` / `on_folder_picked(tag, path)` | File dialog results. Empty path = cancelled. |
| `on_fetch(tag, body)` / `on_fetch_error(tag, message)` | `fetch(url, tag)` result. |
| `on_timer(name)` | `set_timeout` / `set_interval` fired. |
| `on_menu(id)` | `<menubar>` or `<menu>` item clicked. |
| `on_hotkey(name)` | Global hotkey fired. |
| `on_tray(id)` | System tray icon clicked. |

Per-id routing via `on(event, id, fn)` is documented under "Routing"
below.

## Signals (reactive state)

### `signal(name, default) → Signal`

Return a handle to a named scalar signal. Auto-initialises the
host-local mirror with `default` if the signal is unset. Subsequent
calls return a fresh handle; the underlying value is shared.

```rhai
let c = signal("clicks", 0);
c.set(c.get() + 1);

// Vue-style accessor sugar:
c.value = c.value + 1;
```

The `Signal` type has `.get()`, `.set(v)`, and the `.value` get/set
property.

### `signal_array(name) → ArraySignal`

Return a handle to a named reactive array. Backs `<for each="name">`
markup.

```rhai
let todos = signal_array("todos");
todos.set([
    #{ id: "1", label: "Task A" },
    #{ id: "2", label: "Task B" },
]);
todos.push(#{ id: "3", label: "Task C" });
let n = todos.len();
let row = todos.get(0);     // → the first map, or () if empty
```

Methods: `.set(arr)`, `.push(item)`, `.len()`, `.get(i)`, `.all()`. The
array is created lazily on first `.set` / `.push`; reads before that
return `()` / `0`.

`.all()` returns a snapshot of the whole backing array as a Rhai array
of maps. The snapshot is owned by the caller, so mutating it does not
propagate back — follow up with `.set(...)` to write changes:

```rhai
let todos = signal_array("todos");
let rows  = todos.all();          // Vec of the item maps
rows.retain(|r| r.status != "done");
todos.set(rows);                  // write the filtered list back
```

### `derive(name, deps, fn) → Signal`

Register a computed signal. `deps` is either an array of `Signal`
handles or an array of strings naming signals. The closure runs
whenever any dep is in the dirty set; its return value is stringified
into signal `name`.

```rhai
let clicks = signal("clicks", 0);
let label  = derive("counter_label", [clicks], |n| "Clicks: " + n);

// Multi-dep:
derive("greeting", ["who"], |s|
    if s == "" { "(nobody yet)" } else { "hi, " + s + "!" }
);

// Empty deps fires once at startup:
derive("status", [], || "ready");
```

> **First-run.** `derive(...)` marks the signal as pending-initial,
> so the closure fires on the first tick after registration even if
> no dep has been written.

## Per-element mutators

### `set_text(target_id, text)`

Replace the `TextContent` of the entity whose `id` matches. Mostly
superseded by `bind-text="<signal>"` + `signal().set(v)`.

### `set_class(id, classes)`

Replace `LumenClasses` on a `LumenId`-tagged entity. The CSS apply
pass re-runs against the new class set; the `class_invalidation_set`
fast-rejects no-op flips.

```rhai
set_class("card-1", "card highlighted");
```

### `set_root_class(classes)`

Same as `set_class` but targets the `<root>` element directly. Drives
theme switches (no need to give `<root>` an explicit `id`).

```rhai
set_root_class("app theme-dark");
```

### `set_src(target_id, path)`

Swap an `<image>`'s asset path at runtime. The old loaded asset is
dropped and a fresh decode is enqueued.

```rhai
set_src("hero", "icons/sun.png");
set_src("forecast-day-3", "icons/cloud.png");
```

## Routing

### `on(event, id, fn_name)`

Register a per-id handler. When `<event>` fires on entity `id`, the
named function is called with the entity id instead of the global
`on_<event>(id)` fallback.

```rhai
on("click", "save",   "handle_save");
on("click", "cancel", "handle_cancel");

fn handle_save(_id)   { /* … */ }
fn handle_cancel(_id) { /* … */ }
```

Templates auto-namespace inner ids. A handler registered for
the bare suffix matches every instance:

```rhai
on("click", "save", "handle_save");
// fires for `user-card:save`, `team-card:save`, etc. via suffix fallback.
```

Pass the qualified id (`"user-card:save"`) for per-instance routing.

### `local_id(source, suffix) → string`

Return a sibling id within the same template instance. If `source` is
`"user-card:btn"`, `local_id(source, "label")` returns
`"user-card:label"`. Source without a colon returns `suffix` unchanged.
Multi-level prefixes (`"a:b:btn"`) stack — result is `"a:b:label"`.

```rhai
fn handle_save(id) {
    let label_id = local_id(id, "status");
    set_text(label_id, "Saved!");
}
```

## Timers

### `set_timeout(name, ms)`

One-shot timer. Fires `on_timer(name)` after `ms` ms.

```rhai
set_timeout("hide-toast", 3000);
fn on_timer(name) {
    if name == "hide-toast" {
        signal("toast", "").set("");
    }
}
```

### `set_interval(name, ms)`

Repeating timer. Fires `on_timer(name)` every `ms` ms until
`cancel_timer` is called.

### `cancel_timer(name)`

Cancel a pending or repeating timer.

## HTTP

### `fetch(url, tag)`

Issue an HTTP GET. The runtime spawns a worker thread; when the
response lands `on_fetch(tag, body)` runs. Errors fire
`on_fetch_error(tag, message)`.

```rhai
fetch("https://api.example.com/weather?lat=40.7", "weather");

fn on_fetch(tag, body) {
    if tag == "weather" {
        let data = parse_json(body);
        signal("temp", "").set("" + data.current.temp);
    }
}

fn on_fetch_error(tag, msg) {
    signal("status", "").set("error: " + msg);
}
```

### `parse_json(body) → Dynamic`

Parse a JSON string into a Rhai Map / Array / scalar for ergonomic
field access. Returns `()` on parse failure.

## Dialogs

### `pick_file(tag)` / `pick_files(tag)` / `pick_folder(tag)`

Show a native file dialog. The runtime opens the dialog on the main
thread via `rfd`, then fires `on_file_picked(tag, path)`,
`on_files_picked(tag, paths_joined_by_pipe)`, or
`on_folder_picked(tag, path)`. Cancellation fires once with an empty
path so scripts can clean up modal state.

```rhai
on("click", "open", "do_open");
fn do_open(_id) { pick_file("open"); }

fn on_file_picked(tag, path) {
    if tag == "open" && path != "" {
        signal("opened", "").set(path);
    }
}
```

### `pick_file_filtered(tag, spec)`

`pick_file` plus a filter list. The spec is pipe-separated
`<label>:<ext1>,<ext2>,…` groups; bare `*` means "no filter".

```rhai
pick_file_filtered("open-img", "Images:png,jpg,jpeg|All:*");
```

### `save_file(tag, default_name)`

Native save dialog.

```rhai
save_file("export", "out.json");
fn on_file_picked(tag, path) {
    if tag == "export" && path != "" { /* write... */ }
}
```

## Native shell

### `notify(title, body)`

OS notification via notify-rust. Fire-and-forget; missing daemon logs
to stderr.

```rhai
notify("Lumen", "Build finished.");
```

### `register_hotkey(name, accelerator)` / `unregister_hotkey(name)`

OS-level global accelerator. `on_hotkey(name)` fires when the OS
dispatches the chord (window focus optional). Accelerator syntax
follows global-hotkey / Electron conventions.

```rhai
register_hotkey("save", "CommandOrControl+S");
register_hotkey("toggle", "Alt+Space");

fn on_hotkey(name) {
    if name == "save" { /* … */ }
}
```

### `tray_icon(id, icon_path, tooltip)` / `unregister_tray(id)`

System tray icon (macOS / Windows; Linux logs a warning). Click fires
`on_tray(id)`. Empty tooltip means "no tooltip".

```rhai
tray_icon("main", "icons/tray.png", "Lumen");

fn on_tray(id) {
    if id == "main" { /* … */ }
}
```

### `copy_image(path)` / `save_clipboard_image(path)`

PNG-round-trip clipboard images. `copy_image` reads the PNG at
`path` and puts it on the system clipboard; `save_clipboard_image`
pulls the current clipboard image and writes it to `path` as PNG.

```rhai
copy_image("icons/logo.png");
save_clipboard_image("pasted.png");
```

### `open_menu(id)` / `close_menu(id)`

Flip the `__menu_open:<id>` signal driving a `<menu id="…">` popup.
Equivalent to `signal("__menu_open:<id>", "false").set("true")`
but reads as the intended verb.

```rhai
on("click", "show-actions", "open_actions");
fn open_actions(_id) { open_menu("actions"); }
```

## Miscellaneous

### `add_clicks(n)`

Legacy host-interpreted token. The runtime emits the token but doesn't
have semantics around it — kept for back-compat with early-α apps.

### `set_string(key, value)`

Free-form key/value setter — host emits the token without semantics.
Useful only with custom host extensions.

### `print(...)`

Rhai's built-in `print` is overridden to capture lines into the host
diagnostic stream (shows up in `lumenc` stderr instead of stdout).

```rhai
print("debug: clicks = " + signal("clicks", 0).get());
```

## A worked example

End-to-end click counter + theme switch + file dialog:

```rhai
fn on_start() {
    let clicks = signal("clicks", 0);
    let dark   = signal("dark", true);

    derive("counter-label", [clicks], |n| "Clicks: " + n);
    derive("theme-status",  [dark],   |v|
        if v == "true" || v == true { "dark mode" } else { "light mode" }
    );

    on("click", "bump",   "handle_bump");
    on("click", "reset",  "handle_reset");
    on("click", "open",   "handle_open");
    on("click", "theme",  "handle_theme");

    register_hotkey("save", "CommandOrControl+S");
}

fn handle_bump(_id)  { let c = signal("clicks", 0); c.set(c.get() + 1); }
fn handle_reset(_id) { signal("clicks", 0).set(0); }
fn handle_open(_id)  { pick_file_filtered("open", "Text:txt,md|All:*"); }

fn handle_theme(_id) {
    let dark = signal("dark", true);
    let now = dark.get();
    if now == true || now == "true" {
        dark.set(false);
        set_root_class("app theme-light");
    } else {
        dark.set(true);
        set_root_class("app theme-dark");
    }
}

fn on_hotkey(name) {
    if name == "save" { notify("Lumen", "Saved (pretend)."); }
}

fn on_file_picked(tag, path) {
    if tag == "open" && path != "" {
        signal("status", "").set("opened " + path);
    }
}
```

This pattern — `on_start` registers handlers + derivations, per-id
handlers do the actual work, lifecycle callbacks (`on_hotkey`,
`on_file_picked`) handle async results — is the canonical Lumen Rhai
shape. The widget-garden
[`main.rhai`](https://github.com/lumen-ui/lumen/blob/main/apps/widget-garden/main.rhai)
exercises it at scale.
