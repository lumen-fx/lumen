# Putting an app on the web

`lumenc web` turns a Lumen app into a static site: real HTML, real CSS, real
links. Every page becomes its own document with the markup already in it, so
a search engine, a screen reader and a browser with scripting turned off all
get the page rather than an empty shell.

Use it when you want the same app to run on a desktop and to be readable at a
URL. The app is not rewritten for the web and not drawn onto a canvas; the
tree you wrote becomes elements, and the browser lays them out.

## Build a site

```
lumenc web myapp
```

That writes `myapp/dist/web`. To look at it, ask for a server:

```
lumenc web myapp --serve
```

It prints the address it is listening on. Open that; press Ctrl-C to stop.
Opening the files directly from disk does not work, because a browser refuses
to load a module script or a streamed WebAssembly module without a real
origin.

## What lands in the output directory

```
index.html          the entry page
settings.html       one document per page
404.html            the shell a path with no document of its own falls back to
styles.css          the app's stylesheet, plus the reset
app.lmna            the compiled app
app.cdlb            the compiled candela program, when the app has one
lumen.web.json      what the browser runtime reads before anything else
lumen-web.wasm      the runtime
lumen-web.js        the module that loads it
assets/             every file the markup points at
```

Nothing here is per-app code. The runtime is one prebuilt pair of files that
ships with the toolchain and loads the compiled app, the same way the desktop
runtime loads it. A build never compiles Rust or WebAssembly, so it takes
about as long as `lumenc build`.

## How a page reaches the browser

The document holds the page as it looks on arrival. The runtime then adopts
what is already there: it binds itself to the existing elements instead of
building them again, so nothing moves when it starts. From then on the app
behaves as it does on the desktop, and a row or a branch that appears later
is built from the same compiled app the desktop reads.

Input comes from the browser and behaviour stays Lumen's. A click on a tab, a
toggle or a checkbox reaches the same widget code a desktop app runs, and what
it changes reaches the page as an attribute the stylesheet already matches.
Typing in a bound `<input>` writes its signal, so a `bind-text` label next to
it follows along.

A script runs the same way. Its `on_start` publishes the signals the markup
binds to, a handler bound with `on("click", ...)` runs when that element is
clicked, and a `derive()` recomputes when one of its dependencies changes.
The one thing to know is that the browser runs your script as bytecode, with
no compiler behind it, so a function the runtime calls by name has to declare
its parameters and their types:

```
fn calc_greeting(who: string) {
    return "hi, " + who + "!";
}
```

That covers handlers, `derive()` bodies and lifecycle functions. A function
with an unannotated parameter is not callable from the runtime and its
handler silently does nothing, which is the one thing that behaves
differently here than on the desktop.

The browser runtime is not published yet. Until it is, a build says so and
emits the site without it: the pages read, the links work, and nothing runs.
Point `--lib-dir` at a directory holding `lumen-web.wasm` and `lumen-web.js`
to use a copy you built yourself.

Which page a document shows is decided at build time. State comes from
`[web.seed]` and from the defaults the markup declares; set `[web] prerender
= "none"` to render the markup alone, with no branch taken and no rows.

A browser that cannot run the runtime is not left with a blank page: a link is
an ordinary `<a href>`, and following it loads the next document. That
degraded mode needs no configuration.

## Links and deep paths

A link to a page becomes a link to that page's document: `<a href="settings">`
is `/settings.html`. A link that goes deeper than a page, like
`<a href="user/42">` where the app has `user.lmn`, keeps the path the author
wrote, because that is the URL a visitor should see and share. The app reads
the leftover `/42` from `route.segment`, exactly as it does on the desktop.

A static file server has no file at `/user/42`, so it serves `404.html`, which
carries the app and resolves the path in the browser. That works on any file
server without configuration. If your host can rewrite instead, name it and
the matching file is written for you:

```toml
[web]
host = "netlify"   # or vercel, apache, nginx
```

Then the host serves those paths with a 200 and the URL stays as the visitor
typed it. `lumenc web --serve` answers deep paths the way a plain file server
does, so what you see locally is what an unconfigured host does.

A link with a scheme, a protocol-relative link and a fragment are written into
the document unchanged.

## Serving from a subdirectory

Every reference a document makes is rooted at the site's base path, so a site
served from somewhere other than the domain root needs to be told:

```
lumenc web myapp --base /docs
```

or, in `lumen.toml`:

```toml
[web]
base_path = "/docs"
```

## More than one language

Name the locales and the site is emitted once per locale:

```toml
[web]
locales = ["en-US", "de-DE"]
```

The first locale is served from the site root and each of the others from a
directory named after its tag, so the German settings page is
`/de-DE/settings.html`. Text marked `translatable` is resolved while the site
is built, so a page arrives already in its language; `<html lang>` and the
writing direction follow the locale, and every document links to its
counterparts with `hreflang`. What the whole site shares - the stylesheet, the
compiled app, the runtime, the assets - is written once at the root.

## What a crawler sees

Give the site its address and the documents carry a canonical link, Open Graph
and Twitter metadata, and a sitemap:

```toml
[web]
url = "https://example.com"
description = "A Lumen app"
og_image = "assets/preview.png"
```

A page can say more about itself:

```toml
[web.pages.settings]
title = "Settings"
description = "Everything you can change"
```

Without a `url` the canonical link and the sitemap are left out, since neither
means anything without an absolute address.

## Known limits

- `@font-face` and `@keyframes` are not emitted. A font has to be one the
  visitor's system has, and an animation written as a keyframe rule does not
  reach the site.
- A style written as an attribute for a state, such as `hover-bg`, is not
  applied. Write it as a CSS rule and it is.
- A value bound with `bind-*`, or interpolated into text with `{name}`, is
  written as the markup wrote it. The seeded value replaces it when the app
  runs; a branch taken with `<if>` is decided at build time.
- Elements a script creates appear when the runtime starts, not in the
  document, so a crawler does not see them.
- A list built with `<for>` emits its anchor and no rows; the rows are built
  when the app runs.
- A script written in Rhai or Lua does not run in the browser. candela does.
- An element a script creates does not appear, and neither does a class it
  sets with `set_class` or `set_root_class`. Signals, arrays and `set_text`
  do.
- Keyboard input other than typing into a focused field does not reach the app,
  so a keyboard shortcut and arrow-key navigation between tabs do not work.
- Following a link loads the next document. Soft navigation, which swaps the
  page in place and keeps the app running, is not wired up.
- A `<input>` is edited by the browser, so Lumen's own caret, selection and
  IME handling are not in play; what an app sees is the value after each edit.

## Reference

Every flag is in the [CLI reference](../reference/cli.md#web) and every key in
the [`lumen.toml` reference](../reference/lumen-toml.md#web).
