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
- One script program. The `<script>` blocks from every page are combined into a
  single program, so define each function once across the app. `on_start` runs
  once at startup, not on every navigation.
- Window settings live on the home page. `skin`, `frameless`, and a `<menubar>`
  are read from the home page's `<root>`; the same attributes on another page
  are ignored.

A shared header, nav bar, or frame belongs in a `<template>`. Put it in
`layout.lmn` and every page can use it; see
[composition](composition.md).

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

## During development

`lumenc run` watches every page file. Editing any page, or the shared
`layout.lmn`, reloads the app in place.
