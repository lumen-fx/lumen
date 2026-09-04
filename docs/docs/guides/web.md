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

The server listens on 127.0.0.1, so it answers this machine and nobody else.
`--host <addr>` widens that, and says so when you use it: this is a server for
developing against and for hosting a site yourself, and anything the public
reaches belongs behind a reverse proxy.

To watch the app answer per request rather than serve what the build wrote, ask
for a render:

```
lumenc web myapp --render ssr --serve
```

Every page then comes from the app running for the request that asked for it,
and everything else still comes from the directory. That is
[rendering on a server](server-rendering.md) with a socket attached; a
production deployment embeds `lumen-ssr` in a server of your own, and
`lumenc web myapp --render ssr` on its own writes the directory that server
reads.

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

Nothing here is per-app code. The runtime is one prebuilt pair of files, the
same for every app and every platform, and it loads the compiled app the way
the desktop runtime loads it. A build never compiles Rust or WebAssembly, so
it takes about as long as `lumenc build`.

Under `render = "ssr"` that list holds everything except the documents. A page
is produced when it is asked for, so writing one here would leave a second
copy of it beside the one a visitor is sent. With `runtime = false` beside it,
`app.cdlb`, `lumen.web.json`, `lumen-web.wasm` and `lumen-web.js` go too:
nothing loads them. `app.lmna` stays, because the server renders from it.

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

## How the styling reaches the page

`styles.css` holds the whole cascade, in three parts. The reset and the app's
own stylesheet each sit in a layer, `lumen.reset` then `lumen.sheet`. A style
written on an element sits in neither: it becomes a class the element carries
and a rule at the end of the file, and an unlayered rule beats a layered one
whatever the selectors weigh. That is what keeps `bg="#101014"` on a `<tile>`
ahead of a `.card` rule, the way it is ahead on the desktop.

Nothing is written `!important`, which leaves that free for you. An important
declaration cannot be overridden by `:hover`, a media query or a keyframe, so
a page whose styling was written that way could not be animated at all. It is
also what lets a state written on the element, like `hover-bg`, reach the page:
a rule can carry `:hover` and an inline declaration cannot.

Two elements written the same way share one class, so a list of identically
styled rows costs one rule, and the class is in the compiled app rather than
in the document alone. A row the runtime builds after the page has loaded is
spawned wearing it.

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

The runtime is published with every Lumen release. A build uses the copy next
to `lumenc` when there is one, and otherwise downloads the pair from a
published release, checks it against the checksums published with it, and
keeps it in a cache so later builds do not fetch it again. Which release that
is comes from the releases page:
[the CLI reference](../reference/cli.md#which-release-toolchain-files-come-from)
has the detail. `--lib-dir` points at a directory holding `lumen-web.wasm` and
`lumen-web.js` to use a copy you built yourself instead. A build that finds
neither says which files it wanted and emits the site without them: the pages
read, the links work, and nothing runs.

Which page a document shows is decided at build time. State comes from
`[web.seed]` and from the defaults the markup declares, and `[web] prerender`
says so: set it to `"run"` to have the app itself supply the state (see
[Running the app during the build](#running-the-app-during-the-build)), or to
`"none"` to render the markup alone, with no branch taken and no rows.

That state is what the pages are written with. An `<if>` shows the branch it
resolves to, a `<for>` holds its rows, and an element bound with `bind-text`,
`bind-checked`, `bind-value` or `bind-disabled` is written showing the value
its signal holds. Where the state has no value for a signal, the element keeps
what the markup gave it, which is what an author writes a fallback for:

```
<label bind-text="name" text="(signing in)"/>
```

With `name` seeded, that label reads the name in the document itself, so a
crawler and a reader with no scripting get it. Without, it reads
`(signing in)` until the app writes the signal.

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

## Reaching a server

`fetch(url, tag)` and `http(...)` work in a page and mean what they mean on the
desktop: the request goes out without holding up a frame, and `on_fetch`,
`on_fetch_error` or `on_http` runs on the tick its reply arrives on. That is how
a page shows data the build could not know. What a site is built with goes into
the document, and what changes between visits is asked for once the app is
running.

The browser decides whether the request is allowed. Asking your own origin needs
nothing; reading a response from another origin needs that server to send
`Access-Control-Allow-Origin` for the origin your page is served from. That is
the server's setting, not something a build can turn on. A refusal, a request an
extension blocked and an unreachable host all arrive as the same failure, so the
message a script gets says to open the console, where the browser writes which
one it was.

Two things differ from the same call on a desktop. A header the browser reserves
for itself, such as `Host` or `Content-Length`, is dropped on the way out and
nothing reports it. And credentials follow the browser's rule rather than the
app's: cookies ride along to your own origin and not to another one.

## Where a page comes from

`[web] render` says where a document comes from. Every value writes the whole
markup tree, so what a reader and a crawler get does not change; what changes
is what produces the page and what happens once it is open.

`csr`, the default, writes the runtime, the compiled app and the manifest
beside the pages, and the pages load them. The runtime adopts the markup the
page arrived with and runs the app from there.

`static` writes the pages, the stylesheet and the assets, and nothing else. No
runtime, no compiled app, no manifest, and no boot script in the documents.

`ssr` produces each document for the request that asks for it, by running the
app for that request. The build writes what a render needs and leaves the
pages to it, so the app answers with what it knows now rather than with what
it knew when the site was built. A rendered page carries the runtime the way a
`csr` page does, and is adopted the same way. See
[rendering on a server](server-rendering.md).

Whichever it is, a link is an ordinary `<a href>`, so a browser that does not
run the runtime, or a site that carries none, follows links by loading the
next document. That needs no configuration.

### Whether the page carries the runtime

`[web] runtime`, or `--runtime` / `--no-runtime`, is the separate question of
whether a document loads the browser runtime at all. `static` and `csr` differ
about nothing else, so each already answers it and saying the opposite
alongside either is refused, naming the value that means it.

`ssr` is the one that leaves it open:

```toml
[web]
render  = "ssr"
runtime = false
```

Every page is then produced for the request that asks and carries no wasm and
no boot script. The visitor reads the document the app rendered for them and
nothing takes it over: links load the next page, and the next page is another
render. Reach for it when the page is a document rather than an application,
and you want it to depend on who is asking without shipping a runtime to say
so. The compiled app is still written beside the stylesheet, because that is
what the server renders from.

## Running the app during the build

An app usually knows things the markup does not. A list arrives from a script,
a total is derived from a handful of signals, a panel opens because a value
came out true. Written from the seeds alone, the page holds the shape of all
that and none of the answers.

`[web] prerender = "run"`, or `--prerender run`, fills them in. The build starts
the app, lets it settle, and writes each page with the state it settled into:

```toml
[web]
prerender = "run"
```

Reach for it when a page's content comes from the app rather than from the
markup: a list a script publishes, a branch a script decides, a value a
`derive()` computes. It costs a run of the app per page at build time and
nothing at all afterwards, and what it buys is a document that already holds
the list, the branch and the row values the app decided, and a runtime that
starts from them instead of working them out again.

Each page is run on its own, starting from the values `[web.seed]` and the
markup declare, so `on_start` sees the route it is being built for and can
publish something different per page. What the app writes wins over what was
declared, exactly as it does in a browser.

The build stops when the app's state stops changing, not when it stops drawing,
so an app with a spinner or a looping animation settles like any other. An app
whose state never stops changing runs out of budget instead; the build says so
and writes the state the app had reached by then, and `--strict` turns that
into a failure.

Two things keep a page the same wherever it is built. The build answers the
app's HTTP calls itself, with a refusal, so nothing is fetched and no page
depends on what a server said the day it was built; every address the app asked
for is reported. The entry page is also built twice and compared, and `--strict`
compares every page, so an app whose state depends on the clock or on the
machine is caught rather than shipped.

That leaves the network to the browser, which is where dynamic data belongs:
the page arrives complete with everything the app knew on its own, and a
`fetch()` fills in the part only a server can answer.

A page that depends on who is asking, or on data that changes faster than you
rebuild, wants a render per request instead. That is `render = "ssr"` and
[rendering on a server](server-rendering.md), and it runs the same app from the
same files. The two do not combine: a rendered page settles its own state for
the request that asked, so `prerender = "run"` alongside it is refused rather
than run and thrown away.

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
does, so what you see locally is what an unconfigured host does. Under
`--render ssr` the render answers them instead, with the page the path names
and a 200, which is what a server does; no `404.html` and no rewrite file is
written, because neither has anything to stand in for.

An address that names no page at all, like `/nowhere`, is a 404 either way. A
static host sends `404.html`, and a render sends the same shell with the same
status, so a site answers such an address the same way whichever half answers
it. Having the app render its own not-found page is
[the embedder's to arrange](server-rendering.md#an-address-no-page-answers-for).

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

Under `render = "ssr"` no documents are written for any locale. A render
answers every one of them: the request's own locale, then a `/de-DE/` prefix on
the path, then `Accept-Language`, then the locale at the site root. The
[server rendering guide](server-rendering.md) has the whole order.

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
- An author's `!important` rule wins over a style written on the element,
  where on the desktop the element wins. Normal declarations rank the way
  Lumen ranks them; this is the one case where the two differ.
- A value interpolated with `{name}` is read once, when the element is built.
  The document carries the value the page's state held then, and the runtime
  arrives at the same string from the same state; a name the state has nothing
  for keeps its braces. A value that changes while the page is open wants
  `bind-text`, which follows the signal instead of being built once.
- `bind-scroll` reaches the document in no form. How far a container has been
  scrolled is a position the browser keeps, not an attribute a document sets.
- Elements a script creates appear when the runtime starts, not in the
  document, so a crawler does not see them. Components are not among them: the
  build runs a component that has to run and writes its body into the HTML, so
  the document carries the whole markup tree. See
  [Composition](composition.md#a-component-on-the-web).
- A component that has to run must annotate its parameters, or the compiled
  program has no name to call it by, and it is emitted as an empty element. The
  build warns, naming the component.
- A component written inside a `<for>` is filled by the browser, not the build:
  what it renders depends on the row. The build names those too.
- A `<for virtualized="true">` emits no rows. Which rows a virtualized list
  shows comes from how far its scroll container has been scrolled, which a
  build cannot know, so the runtime mounts them when the page opens. The
  build warns when it emits one.
- A list whose rows only exist once a script has run is emitted empty under
  `prerender = "seeds"`. `[web.seed]` puts rows in the document without
  anything running, and `prerender = "run"` gets them from the app itself.
- A run captures signals and lists, which is what the markup is written from.
  It does not capture elements a script created, a property written on one
  entity rather than on a signal, or a vector or a live Rust value, none of
  which a document can carry; the build names any it found. Anything the app
  would have learned from the network is missing too, and so is a value that
  only appears after an animation longer than the run's budget.
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
- `[web] navigation = "soft"` (the default) swaps a same-page link's target
  page in without a reload, keeping the app running; `navigation = "hard"`
  lets every link load the next document, the same as an ordinary site. Soft
  navigation does not update the address bar, so reloading or copying the
  link while on a page reached that way returns to the page the document was
  first loaded as, and the browser's own back and forward buttons are not
  wired to it.
- A `<input>` is edited by the browser, so Lumen's own caret, selection and
  IME handling are not in play; what an app sees is the value after each edit.
- `:drag-over` on a `drop-target` lights up while a file is dragged in from
  the desktop, and clears on a drop, matching the desktop. `on_file_dropped`
  and the in-app `drag-payload` / `on_drop` pair, which read what was
  dropped, are desktop only: `draggable="true"` has no effect in a browser,
  so an element cannot start a drag there in the first place.
- On the desktop, `accept="..."` keeps `:drag-over` off a `drop-target` a
  drag's payload does not match. The web target does not read `accept` at
  all: `:drag-over` lights up for any drag over the element, matched payload
  or not. What the drop itself does with a payload is not wired up on the
  web target regardless, per the point above.

## Reference

Every flag is in the [CLI reference](../reference/cli.md#web) and every key in
the [`lumen.toml` reference](../reference/lumen-toml.md#web).
