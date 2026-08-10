# Pages

A Lumen app can be more than one screen. Drop a second `.lmn` file next to
`main.lmn` and the app gets a second page, addressable by URL-shaped paths,
navigable with real `<a href>` links, and switchable from script. There is no
router component to configure: the file itself is the route.

Use this when an app has more than one real screen - a settings page, a user
detail view, an onboarding flow - and you want each one addressable and
bookmarkable in the same way a small multi-page website is, without hand-
rolling the show/hide logic in signals.

## How a file becomes a page

Every `.lmn` file in the app directory is a page, keyed by its filename stem:
`settings.lmn` becomes the page `settings`, `user.lmn` becomes `user`. The
home page is `index.lmn` when one exists; otherwise the `[app] entry` stem
(usually `main`) is home. An app with only one `.lmn` file never enters
multi-page mode at all - it keeps running the single-file path Lumen has
always used, so adding pages to an app is opt-in by simply having more than
one file.

You do not need any `lumen.toml` entry to get this. `[pages]` in
`lumen.toml` exists for the cases where you want to pin something the
directory scan would otherwise leave automatic:

```toml
[pages]
entry   = "index"                                # home page key
enabled = true                                    # force multi-page on or off
include = ["index.lmn", "settings.lmn", "user.lmn"]  # explicit page set
```

`entry` picks the home page by key when the default (`index.lmn`, then the
`[app] entry` stem, then `main`) is not what you want. `enabled` overrides the
automatic "more than one `.lmn` file present" detection. `include` replaces
directory auto-discovery with an explicit, ordered list - useful once an app
directory holds `.lmn` files that are not meant to be pages. See
[Per-app config](./lumen-toml.md#pages) for the full key reference.

One filename is reserved: `layout.lmn` never becomes a page of its own (see
below), regardless of what the directory scan finds.

Multi-page assembly is a from-source feature today: it runs when `lumenc run`
loads an app directory. A precompiled artifact (`lumenc build` / `lumenc
bundle`) does not yet bake the assembled multi-page tree in; ship a
multi-page app by running it from source until that lands.

## Shared layout with `<template>` and `<slot>`

Every `<template>` block in any `.lmn` file in the app directory is
available to every page, not just the file that defines it. This is what
makes a shared nav bar or page shell practical: put it in its own
`layout.lmn`, and every page can `<use template="layout">` it even though
`layout.lmn` is not itself a page.

```xml
<!-- layout.lmn -->
<root>
  <template name="layout">
    <column padding="20" gap="16">
      <row gap="12">
        <a href="index"    text="Home"/>
        <a href="settings" text="Settings"/>
      </row>
      <column gap="8">
        <slot/>
      </column>
    </column>
  </template>
</root>
```

```xml
<!-- settings.lmn -->
<root>
  <use template="layout">
    <column gap="8">
      <label text="Settings"/>
    </column>
  </use>
</root>
```

The nav bar and the `<slot/>` come from `layout.lmn`; each page fills the
slot with its own content. See [Templates and slots](./templates.md) for the
`<template>` / `<slot>` mechanics themselves - this page only covers the
part specific to sharing one across every page in a multi-page app.

## Navigating with `<a href>`

`<a href="settings">` is a real anchor. A click on it navigates the app to
the `settings` page, the same way a link navigates a browser tab. `href` can
also carry a leading-slash path or a deeper one (`/user/42`); see path
resolution below for how that gets matched to a page file.

```xml
<a href="user/42" text="User 42"/>
```

Middle-click-to-open-in-new-tab and the rest of a real browser's anchor
affordances do not apply on desktop; this is a same-window, same-process
navigation. The anchor markup and the `href` semantics are the same ones a
future web-transpile target maps onto a real DOM `<a href>` and the
browser's own URL bar.

## Navigating from script

The scripting surface to navigate is `page(path)`:

```rhai
// Rhai or Lua
fn goto_settings(id) { page("settings"); }
```

```candela
// candela
fn goto_settings(id) { lumen::page("settings"); }
```

Reading the current page back is `page()` with no arguments in Rhai and
Lua, and `page_current()` in candela, where one name carries one argument
list. candela also reaches the same navigation through `window::set_href`
and `window::href`. All three hosts step through the in-memory history
stack under a `history` namespace - `back()`, `forward()`, `go(delta)` -
spelled `history.back()` in Rhai/Lua and `history::back()` in candela. See
[Scripting](./scripting.md) for how the three hosts differ more broadly.

Under the hood every one of these calls - the anchor click included - goes
through the same host-neutral navigation request; there is no separate
"script navigation" system to keep in sync with anchor clicks.

## Path resolution

The framework does not pattern-match route segments like `:id`. A requested
path resolves against the known page keys by longest existing-file prefix:
if you navigate to `/user/42` and there is a `user.lmn` but no `user/42.lmn`,
the `user` page mounts and `/42` is exposed as the leftover segment for that
page's own script to read. Navigating to a path that matches nothing at all
falls back to the home page, with the whole requested path exposed as the
segment - the shape you'd use to render your own "not found" state.

Two reserved signals carry the result, readable from markup and script on
any host:

| Signal | Holds |
|---|---|
| `route.path` | The resolved page key (the matched filename stem). |
| `route.segment` | Whatever came after the matched prefix, leading-slash included, or empty when the whole path matched a page exactly. |

```xml
<!-- user.lmn -->
<label text="Resolved to user.lmn; leftover path segment:"/>
<label bind-text="route.segment" text="(none)"/>
```

Bind `route.segment` like any other signal, or read it from script
(`signal_get("route.segment")` in Rhai/Lua, `lumen::signal_get("route.segment")`
in candela) and parse it yourself - the framework hands you the raw
leftover text and stops there.

## A worked example

[`apps/pages-demo`](https://github.com/lumen-fx/lumen/tree/main/apps/pages-demo)
is a small runnable app built exactly this way: `index.lmn`, `settings.lmn`,
and `user.lmn` as the three pages, `layout.lmn` as the shared nav shell, and
no `[pages]` block at all - the directory scan is all it needs. Its home
page also shows the programmatic path:

```xml
<button id="go-settings" text="Open Settings"/>
<script>
  fn goto_settings(id) { page("settings"); }
  fn on_start() { on("click", "go-settings", "goto_settings"); }
</script>
```

That script is Rhai, and the app says so with `[script] engine = "rhai"`.
It has to: the script is inline, so there is no file extension for host
inference to read, and the fallback is candela. See
[Choosing a host](./scripting.md#choosing-a-host).

Run it to see file-based routing end to end, including the `/user/42`
prefix-resolution case described above.

## Limitations

There is one in-memory history stack per running app, not per window; a
future multi-window target will need to decide whether history is per-window
or shared. The web-transpile target is expected to swap this same navigation
surface onto the real History API and real URLs without changing anything
you author; `<a href>` and `page()` are written now the way they'll work
then.
