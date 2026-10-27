# Docks and windows

A dock is a workspace of panels the user arranges: tabbed stacks in
resizable splits, panels shown and hidden, collapsed to their header,
maximized, popped out into windows of their own and docked back, and the
whole arrangement kept between runs. The same panels make an app's other
windows: a preferences window, an about box, a modal export dialog is a
panel that opens in a window of its own when the app shows it.

Every panel is part of your app's one document. Popping a panel out moves it
into another window without copying or rebuilding it, so a text field keeps
its text, a list keeps its scroll position, and `get_by_id` finds the
panel's elements from any handler in any window.

## Quick start

Declare the module:

```toml
[dependencies]
lumen-dock = { bundled = true }
```

Write the default layout in markup:

```html
<dock id="editor" label="Editor">
  <dock-split axis="row">
    <dock-stack basis="240">
      <dock-panel id="files" title="Files"><tree id="file-tree"/></dock-panel>
    </dock-stack>
    <dock-stack center="true">
      <dock-panel id="code" title="main.rs"><textarea/></dock-panel>
      <dock-panel id="notes" title="Notes"><textarea/></dock-panel>
    </dock-stack>
    <dock-stack basis="300">
      <dock-panel id="outline" title="Outline">...</dock-panel>
    </dock-stack>
  </dock-split>
</dock>
```

Each `<dock-stack>` shows its panels behind a strip of tabs, each
`<dock-split>` lays its children out along its `axis` with a gutter between
every two, and `basis` gives a child its size in pixels. The stack marked
`center` takes whatever room the others leave, so a window resize widens the
code and not the side panels.

From a script, rearrange it:

```rust
fn on_ready() {
    dock::detach("outline");          // its own window
}

fn dock_outline() {
    dock::attach("outline");          // back where it was
}
```

## Showing and hiding panels

A panel written straight under `<dock>`, outside any stack, waits hidden
until the app shows it; `place` says where it goes the first time:

```html
<dock id="editor">
  ...
  <dock-panel id="profiler" title="Profiler" place="right">...</dock-panel>
</dock>
```

`dock::show`, `dock::hide` and `dock::toggle` take a panel's id. A hidden
panel stays mounted and remembers where it was, so showing it again puts it
back in the same stack at the same tab. Closing a panel with its tab's close
button, a middle click or Delete hides it the same way, unless the panel has
`destroy-on-close`, which removes its element: the shape for documents the
user opens and closes.

Before a user close, the panel raises `panelclose`. A handler that calls
`prevent_default` keeps it open, which is how an editor asks about unsaved
changes:

```rust
fn ask_first(ev: int) {
    if dirty() {
        lumen::event_prevent_default(ev);
        show_save_prompt();
    }
}

fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("code"), "panelclose", "ask_first");
}
```

A Window menu item calls `dock::toggle`, and the `dock.shown.<panel>`
signal says whether a panel is in view, for a checkmark or a label to bind
to.

## Windows of their own

`dock::detach(panel)` pops a panel out into a window of its own, over the
place it has on screen; `dock::detach_group(panel)` takes its whole stack.
`dock::attach(panel)`, a double click on the window's tab strip, or the
window's close button docks it back where it came from. With
`window-close = "hide"` in the module's config, closing the window hides its
panels instead.

A panel written outside any `<dock>` is a window the app opens by showing it.
Its window's properties are attributes of the panel:

```html
<dock-panel id="prefs" title="Preferences" place="window" dockable="false"
            window-kind="dialog" window-size="640 480" resizable="false">
  <settings-form/>
</dock-panel>

<dock-panel id="export" title="Export" place="window" dockable="false"
            window-kind="dialog" modal="parent">
  ...
</dock-panel>
```

```rust
fn open_preferences() {
    dock::show("prefs");
}
```

- `window-kind="dialog"` stays above the main window and minimizes with it.
  A pop-out of an ordinary panel is a normal window with its own taskbar
  entry, which a user can put on another monitor.
- `modal="parent"` blocks input to the main window while the dialog shows;
  `modal="app"` blocks every other window. A press on a blocked window raises
  the dialog.
- `dockable="false"` keeps the panel in a window of its own: it takes no
  other panels and never docks.

`window::close()` in a handler of a pop-out closes that window the same way
its close button does. Closing the main window quits the app and its other
windows close with it, unless `[app] quit = "explicit"` keeps the app
running.

## The keyboard

Every arrangement has a keyboard path:

| Keys | What they do |
| --- | --- |
| F6, Shift+F6 | Move focus to the next or previous stack of the window, back to the element last focused there. |
| Ctrl+F6, Ctrl+Shift+F6 | Move focus to the next or previous window. |
| Ctrl+PageDown, Ctrl+PageUp | Show the next or previous tab of the stack holding focus. |
| Shift+Space on a tab | Maximize its panel to fill the dock, or give the dock its layout back. |
| Escape | Give a maximized dock its layout back. |
| Delete on a tab | Close its panel. |

The module's `keys` config rebinds any of them, or turns them off.

## Keeping the arrangement

`dock::save()` returns the whole arrangement as a string: every dock, each
window of its own with its size and the monitor it was on, and where each
hidden panel goes back to. `dock::restore(layout)` puts it back, in this run
or a later one. An editor keeps one per project this way, with the
[`lumen-fs`](../reference/lumen-toml.md#dependencies) module:

```rust
fn on_close_project() {
    files::write("project/layout.json", dock::save());
}
```

`persist = "auto"` in the module's config keeps it for you in the app's data
directory, restored when the app starts. When you restructure the default
layout in a way an old saved one should not override, raise `layout-version`:
a layout saved under another version is set aside for the markup's, with a
`layouterror` event. A saved layout that names a panel the app no longer has
keeps its place, and raises `panelrestore` so an app that creates panels at
run time can create it again.

```toml
[dependencies]
lumen-dock = { bundled = true, config = { persist = "auto", layout-version = 2 } }
```

Named layouts switch between arrangements: `<dock-layout name="Focus">`
writes one in markup, with `<dock-ref panel="...">` in place of the panels,
and `dock::save_layout(name)` keeps the user's own. A layout places the
panels it names; every other panel keeps the place the markup gives it.

## Panels made at run time

A panel created through the DOM API and appended under the `<dock>` takes its
place like one the markup wrote. Give it `kind` and `state` attributes and a
saved layout carries them, so a restore can ask for the panel again:

```rust
fn open_document(n: int) {
    let p = lumen::node_spawn("dock-panel");
    lumen::node_set_attr(p, "id", "doc-" + str(n));
    lumen::node_set_attr(p, "title", "Document " + str(n));
    lumen::node_set_attr(p, "kind", "document");
    lumen::node_set_attr(p, "destroy-on-close", "true");
    lumen::node_append(lumen::node_get_by_id("editor"), p);
}
```

A panel's element ids live in the app's one id space, so a panel built from a
template with an `id` on its use site prefixes the ids inside it, and two of
them never collide.

## Styling

The dock's look is CSS: tab height and padding, the strip, the stack border
and background. Set the `--lumen-dock-*` tokens in your stylesheet or skin,
or style the classes the dock uses (`dock-stack`, `dock-tab`,
`dock-tab-close`, `dock-split`, `dock-window`, and `center`, `collapsed`,
`single`, `maximized` on a stack). A window of panels carries `dock-window`
and `dock-window-<kind>`, so `.dock-window` styles every pop-out.

## In a page

A web build shows each dock's default layout, splits laid out and tabs that
switch. Moving, popping out, saving and restoring panels are desktop features
for now: in a page those calls leave the layout as it is and say so on the
console.

## Where to look things up

- [`<dock>` and its elements](../reference/tags.md#dock-and-its-panels): every attribute,
  event and class.
- The `dock` functions for [candela](../reference/scripting-candela.md#docks-and-windows),
  [Rhai](../reference/scripting-rhai.md#docks-and-windows) and
  [Lua](../reference/scripting-lua.md#docks-and-windows).
- [The module's config](../reference/lumen-toml.md#dependencies).
- [Docks from Rust, C++ and Python](../reference/ffi.md#docks).
