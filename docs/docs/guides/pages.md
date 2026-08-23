# Multi-page apps

A Lumen app can be more than one screen. Each screen is its own `.lmn` file in
the app directory, and the filename is its name. There is no router to
configure and no route table to keep in sync: add a file, get a page.

## Add a page

Start from an app whose markup lives in `index.lmn`, then drop a second file
beside it:

- `index.lmn` - the home page
- `settings.lmn` - reachable as `settings`
- `user.lmn` - reachable as `user`

Each page file is a complete markup document with its own `<root>` element:

```html
<root>
  <column gap="8">
    <label text="Settings" font-size="24"/>
    <label text="Nothing to configure yet."/>
  </column>
</root>
```

Multi-page mode switches on as soon as the app directory holds more than one
`.lmn` file. An app with a single `main.lmn` keeps loading exactly as before.

The home page is `index.lmn`. If there is no `index.lmn`, Lumen uses the file
named by `[app] entry` in `lumen.toml`, then `main.lmn`, then the first page it
finds.

## Link between pages

Use an anchor. The `href` is a page name, not a URL:

```html
<row gap="12">
  <a href="index" text="Home"/>
  <a href="settings" text="Settings"/>
</row>
```

Clicking the anchor switches the active page. An `<a>` is an ordinary element
otherwise, so style it like any other: give it `bg`, `padding`, `radius`, or a
class. See [tags reference](../reference/tags.md).

A click handler that calls `prevent_default()` on the event stops the
navigation, which lets you confirm before leaving a page. See
[scripting](scripting.md).

## Navigate from a script

```rhai
fn open_settings(id) {
    page("settings");
}
```

The four navigation calls are:

- `page(path)` - go to a page
- `page()` - the name of the active page
- `page_back()` - one step back through visited pages
- `page_forward()` - one step forward

Lua uses the same names. In candela they live in the `lumen` namespace, and the
reader is spelled out because candela has no arity overloading:
`lumen::page(path)`, `lumen::page_current()`, `lumen::page_back()`,
`lumen::page_forward()`. Per-host details are in the
[candela](../reference/scripting-candela.md),
[Rhai](../reference/scripting-rhai.md), and
[Lua](../reference/scripting-lua.md) references.

Back and forward walk an in-memory history of the pages visited in this run.
Navigating to a new page after going back discards the entries ahead of it.

## Paths with parameters

Lumen never pattern-matches a path against `:id` placeholders. A requested path
resolves to the longest page name that matches its leading segments, and
whatever is left over is handed to the page as text.

With only `user.lmn` present, navigating to `user/42` mounts `user.lmn` and
leaves `/42` for the page to read:

```html
<label bind-text="route.segment" text="(none)"/>
```

Parse the segment yourself; the framework does not turn it into typed
parameters.

Two values are always readable:

- `route.path` - the name of the active page
- `route.segment` - the leftover path after the page name, empty when there is
  none

Bind them with `bind-text`, or gate on them with `<if>` the same way you would
any other signal. See [reactivity](reactivity.md).

A path that matches no page at all falls back to the home page with the whole
requested path in `route.segment`, so you can render your own not-found screen
there.

## What every page shares

Styling and scripting are app-wide, not per page.

- One stylesheet. `main.css` styles the whole app; split it with `@import` if it
  grows. See [styling](styling.md).
- One script program per language. The `<script>` sources from every page are
  grouped by host and combined, so define each function once across the app
  rather than once per page. `on_start` runs once at startup, not on every
  navigation. See [scripting](scripting.md) for how a file picks its host.
- Window settings live on the home page. `skin`, `frameless`, and a `<menubar>`
  are read from the home page's `<root>`; the same attributes on another page
  are ignored.

A shared header, nav bar, or frame belongs in a `<template>`. Put it in
`layout.lmn` and every page can use it; see
[composition](composition.md).

## Where a page sits

A page is not spliced straight into `<root>`. Each one mounts inside its own
box, and every page's box covers the same rect: the whole of `<root>`, which
is the whole window. A shell that asks for the full window gets it:

```html
<root>
  <column height="100%">
    <row height="56"/>
    <column grow="1"/>
    <row height="40"/>
  </column>
</root>
```

`height: 100%` measures against the page box, which measures the window, so
the bottom row sits on the bottom edge of the window rather than below it, and
`grow` on the middle column takes whatever is left between them. Nothing here
needs `position: absolute`.

The box supplies size and nothing else: no padding, background, or spacing of
its own. Everything visible on screen is the page's own markup. Because it
covers `<root>` edge to edge, padding written on `<root>` does not inset a
page - put the breathing room on the page's own shell instead.

The home page's `<root>` is the app's root element, so its attributes apply
under every page. On any other page `<root>` is only a wrapper: its children
mount, and attributes written on it are ignored.

## Page state

Navigating away despawns the old page's elements and spawns the new page's from
scratch. Anything you want to survive a navigation belongs in a signal rather
than in the widget tree.

## Configuration

Pages need no configuration. When you want to override the defaults, `lumen.toml`
takes a `[pages]` block:

```toml
[pages]
entry = "index"
enabled = true
include = ["index.lmn", "settings.lmn", "user.lmn"]
```

`entry` picks the home page, `enabled` forces multi-page mode on or off, and
`include` replaces directory discovery with an explicit list. Full key
descriptions are in the [lumen.toml reference](../reference/lumen-toml.md).

`include` paths are relative to the app directory and may point into a
subdirectory, which is the way to keep pages in a folder of their own:

```toml
[pages]
enabled = true
include = ["index.lmn", "pages/settings.lmn", "pages/user.lmn"]
```

A page's name is still its filename stem, so `pages/settings.lmn` is reachable
as `settings`. Discovery itself never looks inside a subdirectory: a `.lmn`
file down there is a page only when `include` names it, and `layout.lmn` is
picked up from the app directory only. `enabled` is what switches multi-page
mode on here, because the automatic switch counts the `.lmn` files in the app
directory and a subdirectory adds none.

## During development

`lumenc run` watches every page file. Editing any page, or the shared
`layout.lmn`, reloads the app in place.

## Shipping a multi-page app

`lumenc package` compiles every page into the app executable, together with the
page names navigation resolves against, so a packaged app routes exactly as it
does here while carrying no `.lmn` files at all. `lumenc build` bakes the same
page set into an artifact. Nothing about a page changes once it ships:
`page()`, `<a href>`, back and forward, and paths with parameters all behave as
described above.

The one difference is that a shipped app has no page files to reload, so adding
a page means rebuilding. See [packaging](packaging.md).

On the web the model is the same one it was designed for. `lumenc web` emits
each page as its own HTML document, and each `<a href>` becomes an ordinary
link to it, so a page has a URL a visitor can share and a crawler can index. A
path with parameters keeps the shape you wrote: `/user/42` stays `/user/42`,
and `route.segment` reads the leftover part in the browser exactly as it does
here. See [putting an app on the web](web.md).
