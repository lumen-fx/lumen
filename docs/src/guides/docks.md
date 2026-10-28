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

The user does the same from a tab's menu, opened with a right click on the
tab or the Menu key or Shift+F10 when the tab has focus: Move to new window,
Move group to new window, Float, Pin to sidebar, Dock back, Maximize, and
Close, whichever fit where the panel is. A screen reader hears each move ("Inspector moved to a new
window"). The menu's words are the module's messages, so the app's own
catalogue translates them; the [tag reference](../reference/tags.md#dock-and-its-panels)
lists their ids.

A document panel's window can carry the name of what it shows:
`bind-window-title="doc.name"` makes the window's title follow that signal,
and a panel's `title` and `icon` attributes, changed from a script, change
its tab.

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
running: then the main window and the panels' windows hide, and showing the
main window again shows them again.

## Rearranging by dragging

Drag a tab to move its panel. While the drag runs nothing in the layout
moves: a ghost of the tab follows the pointer, and a highlighted region shows
where the panel lands if you let go there.

- Between two tabs of a strip: the panel joins that stack at that place, or
  takes a new place in its own strip.
- The middle of a stack: the panel joins it as its last tab.
- Near an edge of a stack: the panel takes that half, in a stack of its own
  beside it.
- Along an edge of the whole dock: the panel takes a stack along that edge.
- Over no stack at all, inside the window (a toolbar, a status bar): the
  panel floats there, over the dock.

Dragging a stack's strip, away from its tabs, moves the whole stack. Escape
puts everything back where it was, and a screen reader hears where a panel
went, or that the move was cancelled. A panel with `movable="false"` does not
drag; the tab menu and the `dock` functions still move it.

Pull a tab past the edge of the window and it tears off into a window of its
own that follows the pointer; let go and the window stays there. Drag a
window of one panel by its tab to move the whole window: over another window
of the app that window shows where the panel would land and the window you
are dragging gets out of the way, and letting go docks it there. Dragging one
tab out of a window that holds several tears that tab off on its own.

On Windows, macOS, X11 and the Wayland desktops that let an app drag its
windows (KDE Plasma, GNOME), the window follows the pointer. On other Wayland
compositors (Hyprland, sway and the rest of the wlroots family) a window
cannot follow the pointer: tearing a tab off shows nothing until you let go,
and the panel then opens in a window of its own where the compositor places
it. Dragging a tab or a window's tab onto another window still docks it.
These compositors keep keys from every app while a drag is outside its
window, so Escape cancels a drag only before the pointer leaves the window it
began in. The tab menu's Move to new window and Dock back work everywhere.

## Floating panels

A float is a group of panels lying over its dock, inside the dock's window:
a tool palette the user keeps beside their work without giving it a share of
the layout. A panel floats by a drag released over no stack, by Float in its
tab menu, or from a script:

```rust
fn float_palette() {
    // From the dock's top-left corner: x, y, width, height.
    dock::float("palette", 40.0, 40.0, 280.0, 360.0);
}
```

The float's tab strip is its title bar: drag it there to move the float, and
drag the frame along its edges and corners to resize it. Its tabs drag like
any other, onto a stack, beside one, or out of the window. A double click on
its strip, Dock back in the tab menu, or `dock::attach` puts its panels back
where they came from. A click in a float brings it above the others, floats
are saved with the layout, and `floatable="false"` on a panel keeps it from
floating.

## Pinning panels to the edges

A panel pinned to an edge of its dock leaves the layout and shows as a
button in a strip along that edge, the way an editor keeps its console and
its search results out of the way until they are wanted. Clicking the button
slides the panel out over the dock; a click elsewhere, Escape, or moving focus
out of it slides it back in. Pin panels in the markup with `<dock-sidebar>`,
from the tab menu's Pin to sidebar, or with `dock::pin`:

```html
<dock id="editor">
  ...
  <dock-sidebar edge="bottom">
    <dock-panel id="console" title="Console">...</dock-panel>
  </dock-sidebar>
</dock>
```

```rust
fn on_ready() {
    dock::pin("outline", "right");
}
```

`dock::show` and `dock::toggle` slide a pinned panel out, the way a Window
menu item opens it. The Dock button in the slid-out panel's header, or
`dock::attach`, puts the panel back where it was before it was pinned.

## Crowded tab strips

When a stack holds more tabs than its strip has room for, the tabs shrink to
their `min-width` first, then the strip scrolls: the wheel over it moves it
along, the shown tab is kept in view, and a button at the strip's end lists
every tab of the stack to pick from.

## The keyboard

Every arrangement has a keyboard path:

| Keys | What they do |
| --- | --- |
| F6, Shift+F6 | Move focus to the next or previous stack of the window, back to the element last focused there. |
| Ctrl+F6, Ctrl+Shift+F6 | Move focus to the next or previous window. |
| Ctrl+PageDown, Ctrl+PageUp | Show the next or previous tab of the stack holding focus. |
| Shift+Space on a tab | Maximize its panel to fill the dock, or give the dock its layout back. |
| Escape | Cancel a drag in progress, slide a pinned panel back in, or give a maximized dock its layout back. |
| Delete on a tab | Close its panel. |
| Menu key, Shift+F10 on a tab | Open its tab menu: move the panel to a new window, dock it back, maximize it, close it. |

The module's `keys` config rebinds any of them, or turns them off.

## Keeping the arrangement

`dock::save()` returns the whole arrangement as a string: every dock, each
window of its own with its size, the monitor it was on and whether it was
maximized or fullscreen, and where each hidden panel goes back to.
`dock::restore(layout)` puts it back, in this run or a later one. On Wayland
the desktop places each window itself, so a restore brings back its size and
state but not its position; the saved position stays in the layout for a
desktop that can use it. An editor keeps one per project this way, with the
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

The dock's look is CSS. By default each stack has a darker strip of compact
tabs, each as wide as its title, and the shown tab takes the stack's color,
full-strength text and an accent line along its top edge. It reads the
skin's tokens where a skin sets them and suits the dark window of an app
with no skin otherwise.

Set the `--lumen-dock-*` tokens in your stylesheet or skin, or style the
classes the dock uses (`dock-stack`, `dock-tab`, `dock-tab-icon`,
`dock-tab-label`, `dock-tab-close`, `dock-split`, `dock-window`, `center`,
`collapsed`, `single`, `maximized` and `overflowing` on a stack, and `active`
on the shown tab, `dock-float` on a float, `dock-sidebar` and
`dock-sidebar-button` on an edge's strip and its buttons, `dock-sidebar-open`
on a slid-out panel, `dock-dragging` on the dock while a drag runs,
`dock-drag-ghost` on the ghost and `dock-drop-preview` with `dock-drop-tab`,
`dock-drop-center`, `dock-drop-<side>`, `dock-drop-root-<side>` or
`dock-drop-float` on the highlighted region; the
[tag reference](../reference/tags.md#dock-and-its-panels) lists them all).
`--lumen-dock-tearoff-distance` sets how far past the window's edge a tab
goes before it tears off. The dock's own rules name the stack (`.dock-stack .dock-tab`), so your
CSS always wins and a skin wins where its rule is as specific. A window of
panels carries `dock-window` and `dock-window-<kind>`, so `.dock-window`
styles every pop-out. A tab's close button shows the multiplication sign; a
skin that ships an icon draws its own with `mask-image` on `.dock-tab-close`
and a transparent `text-color`.

## In a page

A web build shows each dock's default layout, splits laid out and tabs that
switch. Moving, floating, pinning, popping out, saving and restoring panels
are desktop features for now: in a page those calls leave the layout as it
is and say so on the console, and the panels of a `<dock-sidebar>` stay out
of the page's layout.

## Where to look things up

- [`<dock>` and its elements](../reference/tags.md#dock-and-its-panels): every attribute,
  event and class.
- The `dock` functions for [candela](../reference/scripting-candela.md#docks-and-windows),
  [Rhai](../reference/scripting-rhai.md#docks-and-windows) and
  [Lua](../reference/scripting-lua.md#docks-and-windows).
- [The module's config](../reference/lumen-toml.md#dependencies).
- [Docks from Rust, C++ and Python](../reference/ffi.md#docks).
