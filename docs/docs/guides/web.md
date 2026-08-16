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

The document holds the page as it looks on arrival, including the rows of a
list: a `<for>` over a list the build knows is written out row by row, with
each row's values already substituted in, so a crawler and a reader with no
scripting both get the list itself rather than an empty box. The runtime then
adopts what is already there: it binds itself to the existing elements instead
of building them again, so nothing moves when it starts. From then on the app
behaves as it does on the desktop, and a row or a branch that appears later is
built from the same compiled app the desktop reads.

An app that starts from a different list than the one the page was built with
is put right on the first frame: rows the app does not have are taken out of
the page, and rows it has and the page does not are built. Neither is
something to configure; it is what keeps a stale document from showing a row
that is gone.

Input comes from the browser and behaviour stays Lumen's. A click on a tab, a
toggle or a checkbox reaches the same widget code a desktop app runs, and what
it changes reaches the page as an attribute the stylesheet already matches.
Typing in a bound `<input>` writes its signal, and so does moving a bound
`<slider>`, so a `bind-text` label next to either follows along.

## What the browser does itself

Where a browser already implements a behaviour Lumen gives the same meaning
to, the page gets the browser's rather than a copy driven from the runtime.

A `<dialog>` is the clearest case. It opens as a real modal: it sits over the
whole page with no stacking order to arrange, the rest of the document stops
answering clicks and tab stops while it is up, focus lands on the element
marked `autofocus` when it opens, and Escape dismisses it. A dismissal writes
the signal named in `open="..."`, so a script sees the same close it would see
from a Cancel button and the dialog reports the same rejected verdict it
reports on the desktop.

The browser's own dialog chrome is taken back off. A native dialog is a
bordered card with its own fill and an inch of padding; a Lumen dialog is the
whole window with the app's surface centred inside it, and that is what the
page shows.

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

That covers handlers, `derive()` bodies and lifecycle functions. Annotate a
parameter with the type it arrives as rather than `any` where the body does
arithmetic on it or joins it to a string; a value typed `any` cannot do
either, and `str(n)` is how a number joins a string in any case.

`lumenc web` names every function the app calls by name that the compiled
program does not export, so a handler that would have done nothing is a
warning at build time rather than a blank in the page.

The browser runtime is not published yet. Until it is, a build says so and
emits the site without it: the pages read, the links work, and nothing runs.
Point `--lib-dir` at a directory holding `lumen-web.wasm` and `lumen-web.js`
to use a copy you built yourself.

Which page a document shows is decided at build time. State comes from
`[web.seed]` and from the defaults the markup declares; set `[web] prerender
= "none"` to render the markup alone, with no branch taken and no rows.

A list is state like any other, so `[web.seed]` can name its rows. Each row is
a table of the fields the row template reads:

```toml
[[web.seed.todos]]
id    = "1"
title = "write it down"

[[web.seed.todos]]
id    = "2"
title = "do it"
```

A `<for each="todos">` is then emitted with those two rows in it, and the app
starts from the same list.

## What the pages carry

`[web] render` says whether the documents load the runtime. Both values write
the whole markup tree, so what a reader and a crawler get does not change; what
changes is what happens once the page is open.

`csr`, the default, writes the runtime, the compiled app and the manifest
beside the pages, and the pages load them. The runtime adopts the markup the
page arrived with and runs the app from there.

`static` writes the pages, the stylesheet and the assets, and nothing else. No
runtime, no compiled app, no manifest, and no boot script in the documents.

Either way a link is an ordinary `<a href>`, so a browser that does not run the
runtime, or a site that carries none, follows links by loading the next
document. That needs no configuration.

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
- A value bound with `bind-*`, or interpolated into text with `{name}` outside
  a list row, is written as the markup wrote it. The seeded value replaces it
  when the app runs; a branch taken with `<if>` is decided at build time. A
  placeholder inside a `<for>` row is different: it is resolved against the
  row while the page is written.
- Elements a script creates appear when the runtime starts, not in the
  document, so a crawler does not see them.
- A `<for virtualized="true">` emits no rows. Which rows a virtualized list
  shows comes from how far its scroll container has been scrolled, which a
  build cannot know, so the runtime mounts them when the page opens. The
  build warns when it emits one.
- A list whose rows only exist once a script has run is emitted empty, the way
  every other piece of script-made state is. `[web.seed]` is how a list
  reaches the document without anything running.
- A script written in Rhai or Lua does not run in the browser. candela does.
  An app written in one of them is still emitted and still reads: the pages
  show the state they were built with, and nothing runs.
- A `<checkbox>` or a `<radio>` written with a `label` shows its box without
  the caption. The caption is a second element on the desktop and an HTML
  checkbox takes no children.
- An element a script creates does not appear, and neither does a class it
  sets with `set_class` or `set_root_class`. Signals, arrays and `set_text`
  do.
- Keyboard input other than typing into a focused field does not reach the app,
  so a keyboard shortcut and arrow-key navigation between tabs do not work.
  Escape on an open dialog is the exception; the browser closes it.
- Following a link loads the next document. Soft navigation, which swaps the
  page in place and keeps the app running, is not wired up.
- A `<input>` is edited by the browser, so Lumen's own caret, selection and
  IME handling are not in play; what an app sees is the value after each edit.

## Reference

Every flag is in the [CLI reference](../reference/cli.md#web) and every key in
the [`lumen.toml` reference](../reference/lumen-toml.md#web).
