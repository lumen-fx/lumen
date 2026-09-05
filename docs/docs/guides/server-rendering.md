# Rendering on a server

`lumen-ssr` renders a Lumen app to an HTML document once per request. The same
app, the same markup and the same scripts a build writes a static site from,
except that the state is read for the visitor asking: the address they asked
for, the language their browser wants, the record their path names, the data an
API answered a moment ago.

Reach for it when a page depends on who is asking or on data that changes
faster than you rebuild. A page whose content is the same for everybody wants
[a build](web.md) instead, which costs nothing to serve.

There is no transport in the crate. No sockets, no HTTP parsing, no async
runtime: a request goes in as a struct and a response comes back as one, so it
goes inside the server you already have, whichever it is.

## What you serve

A render produces documents and nothing else. Everything else a page loads
still comes from `lumenc web`:

```
lumenc web myapp --render ssr
```

That writes `styles.css`, `app.lmna`, `app.cdlb`, `lumen-web.wasm`,
`lumen-web.js` and `assets/`, and no documents: serve those as static files
and answer everything else with a render. The runtime adopts a rendered
document the same way it adopts a built one.

Add `--no-runtime` to render pages that carry none:

```
lumenc web myapp --render ssr --no-runtime
```

Each page is still produced for the request that asks, and now it is only a
document: no wasm, no boot script, and nothing that takes it over once it is
open. Links load the next page, which is another render. `app.lmna` is still
written, because that is what you render from; the runtime files and the
manifest are not, because nothing loads them.

Build with `--render csr` instead when you want documents to fall back to.
Which of the two answers a request is then yours to decide, and so is
rebuilding them when the app changes.

## Trying it without writing a server

```
lumenc web myapp --render ssr --serve
```

That emits the site, then serves it with every page coming from a render of the
app for the request that asked. It is the same renderer this page documents,
with a socket in front of it, and it is for developing against and for hosting
a site yourself: it listens on 127.0.0.1, `--host <addr>` is what widens that
and says so, and anything the public reaches belongs behind a reverse proxy
such as nginx. A production deployment embeds `lumen-ssr` in a server of your
own, which is what the rest of this page is about.

Files keep their own path through it: a stylesheet, an artifact and the wasm
module are read from the directory the build wrote while a page is being
rendered, so nothing a page needs waits behind the page. Renders queue, because
a process renders one at a time.

A render reaches no host unless you name it, the same policy an embedder sets
in `FetchPolicy`:

```
lumenc web myapp --render ssr --serve --allow-host api.example.com
```

Warnings a render comes back with are printed once each, so a page reloaded
twenty times does not bury a new one. Whether anything is reading them is not
the server's business: a log line that cannot be written is dropped, rather
than ending a process in the middle of answering somebody.

## Rendering

```rust
use std::sync::Arc;
use lumen_ssr::{FetchPolicy, RenderOptions, Renderer, SsrRequest, SsrSite};
use lumen_web::WebSpec;

let compiled = lumen_ir::artifact::read("dist/web/app.lmna".as_ref())?;
let site = SsrSite::new(compiled, WebSpec::default())?;
let options = RenderOptions {
    fetch: FetchPolicy::default().allow_host("api.example.com"),
    ..RenderOptions::default()
};
let renderer = Renderer::start(Arc::new(site), options)?;

let response = renderer.render(
    SsrRequest::get("/user/42?tab=posts").with_header("Accept-Language", "en-GB"),
)?;
// response.status, response.headers and response.body are what to send.
```

`WebSpec` says where the files above live and what the pages are called. Give
it the same values `lumenc web` was given, or the documents will point at files
your server does not have.

`render` blocks until the document is written, and it is safe to call from any
thread: calls queue.

A render that panics takes the renderer with it, and the requests after it are
answered with what happened and a 500. Start the server again once you have
fixed what panicked.

## More than one language

A site holds one tree per language it answers in, and a request picks one. The
trees are the app already translated: `translatable` text is resolved into the
markup before a document is written from it, and text carrying a `format` is
written for the tree's locale as the document is, so a page arrives in its
language with nothing running.

`lumenc web` builds them from the `locale/*.ftl` catalogues beside your markup,
one per `[web] locales` entry. An embedder builds one the same way and hands it
over:

```rust
use lumen_web::{LocaleSpec, PageSpec, SiteSpec};

// `catalogue` is a `SharedI18n` holding the German messages, loaded from
// wherever your deployment keeps them.
let german = SiteSpec {
    pages: vec![PageSpec::new(
        "index",
        lumen_web::translate_ir(&compiled.ir, &catalogue),
    )],
    locale: LocaleSpec {
        default_locale: "en-US".to_string(),
        ..LocaleSpec::new("de-DE")
    },
    ..site.spec().clone()
};
let site = site.with_locale(german)?;
```

Every tree has to answer for every page the site has, because a request
resolves to a page before it resolves to a language; a tree missing one is
refused rather than answered from another language.

Which tree answers is decided in this order:

1. `SsrRequest::with_locale("de-DE")`, for a proxy or a language cookie that
   has already decided. A tag the site holds no tree for falls through to the
   rest, and the response says so in its warnings.
2. A locale prefix on the path: `/de-DE/settings.html` is the `settings` page
   of the German tree, which is what an `hreflang` link points at. The tree at
   the site root has no prefix.
3. `Accept-Language`, matched against the tags the site holds. A range reaches
   a tag that continues it, so `de-AT` reaches a site that holds `de-DE`, and
   `q=0` is a refusal rather than a low preference. This reads the header as it
   arrived: which document the server sends is the server's decision, so a
   `HeaderPolicy` that keeps `accept-language` from the app still negotiates.
4. The site's default locale, which is the tree at the root.

Every response names the language it is in with `Content-Language`, and a site
holding more than one tree also sends `Vary: Accept-Language` so a shared cache
does not hand one visitor's language to the next. Both are set before the app's
own headers, so a page that sets either itself is the one that is sent.

Under `render = "ssr"` a build writes no documents for any tree: the renderer
answers `/de-DE/settings.html` itself, and a file beside it would be a second
answer for one address.

## One render at a time, per process

`Renderer::start` fails with `AlreadyRunning` if the process already has a
renderer. An app reads what its scripts write through buses that belong to the
process rather than to the app, so two apps ticking at once take each other's
writes and one visitor's data lands in another visitor's page. The check turns
that into an error at startup.

Serve more requests at once by running more processes behind whatever balances
them. That is the scaling story, and it is the one to plan for.

## A request is a whole life

Every request builds an app, ticks it, reads it and drops it. Nothing survives
to the next one: `on_start` and `on_ready` run every time, and a signal written
for one visitor is gone before the next arrives. Anything that has to outlive a
request lives in your database.

The boot is what that costs. It is small enough to pay per request and does
not grow with the number of requests a process has served; `cargo bench -p
lumen-portable --bench boot` measures it on your own app.

## What the app reads of the request

The address arrives as reserved signals, which markup binds to by name and
every script host reads with `signal_get`:

| Signal | Holds |
| --- | --- |
| `request.method` | The method, uppercased. |
| `request.path` | The path, without the query string. |
| `request.query` | The query string, without the leading `?`. |
| `request.hash` | The fragment, without the leading `#`, normally empty. |
| `request.secure` | Whether the request arrived over TLS. |

They are written before the app's first script runs, so `on_start` can decide
what to publish from the address it is being asked for.

Routing is the desktop's: a path resolves to the page with the longest matching
key, and the rest of it lands in `route.segment`. A request for `/user/42` in an
app with `user.lmn` renders the `user` page with `/42` on `route.segment`, which
is the page's to parse.

A link inside a built site points at the document that build wrote, so
`/settings.html` is a request for the `settings` page and `/index.html` for the
entry page, whatever it is keyed as. `request.path` still holds the address as
it arrived.

### An address no page answers for

`/nowhere` matches no page key and is no document a build wrote, so nothing is
rendered for it: the response is a 404 carrying the app shell, which is the
same `404.html` a static build writes for a path its host has no file for. A
site answers such an address the same way whether it is rendered or built.

The shell holds no state, so it is written once and reused, and the app is not
built for it. An address anyone can guess would otherwise cost a whole app boot
to arrive at a document that is the same every time.

A deep path is not this case. `/user/42` in an app with `user.lmn` names the
`user` page, so it renders as that page with `/42` on `route.segment`.

To have the app answer such an address itself, ask which page it names and
render one the app does have:

```rust
let response = match site.page_for(path) {
    Some(_) => renderer.render(SsrRequest::get(path))?,
    // `notfound.lmn` renders it, reading the address off `route.segment`.
    None => {
        let mut own = renderer.render(SsrRequest::get(&format!("/notfound{path}")))?;
        own.status = 404;
        own
    }
};
```

The headers, the cookies and the body are read one at a time, because a page has
no business holding all of them:

```candela
import "lumen.cdl";

fn main() {}

fn on_start() {
    lumen::signal_set("language", lumen::request_header("accept-language"));
    lumen::signal_set("session", lumen::request_cookie("session"));
    lumen::signal_set("submitted", lumen::request_body());
    lumen::signal_set("tab", window::location_query());
}
```

Each is empty when there is nothing to read, which is every desktop app and
every page in a browser. In a browser `window::location_query()` and
`window::location_hash()` read the address the page was opened at.

### Headers are allowed by name

An app runs somebody else's code against somebody else's request, so it reads
the headers that say what a browser is and none that say who is using it:
`accept`, `accept-encoding`, `accept-language`, `host`, `referer`,
`user-agent`, the `x-forwarded-*` family and `x-request-id`. Anything else,
including `Authorization`, `Cookie` and `Proxy-Authorization`, is named
explicitly or not read at all:

```rust
use lumen_ssr::{HeaderPolicy, RenderOptions};

let options = RenderOptions {
    headers: HeaderPolicy::default().allow("Authorization"),
    ..RenderOptions::default()
};
```

A cookie is different: `request_cookie(name)` reads one by name whether or not
the `Cookie` header itself is allowed, which is the granularity worth having.
An app that wants the whole jar as a string allows `cookie` like any other
header.

## What the app says about the response

Three builtins answer the request with something other than a plain document:

| Builtin | Does |
| --- | --- |
| `response_status(status)` | Answers with that status, clamped to 100..=599. |
| `response_header(name, value)` | Sets a response header; setting a name twice replaces it. |
| `redirect(location)` | Answers with a redirect instead of a document. |

A redirect stops the render: the response carries `Location`, a 302 unless the
app set a status of its own, and no body.

A header a script sets is checked before it is sent. A value carrying a line
break is refused, because it would end the header and start something else, and
so are `Content-Length` and `Transfer-Encoding`, because how the body is framed
is the server's to say. Both refusals arrive in `response.warnings` rather than
in silence.

## Waiting for the app's own requests

An app that fetches its data in `on_ready` has none of it on the tick the
request goes out, so a render waits: what the app asked for is counted, and the
document is written once nothing is outstanding and the app's state has stopped
changing. A page whose list comes from an API is rendered with the list in it.

An app still waiting when its budget runs out is answered with what it has, plus
an `X-Lumen-Render: partial` header and a warning saying so. A slow upstream
then gives a visitor a working page the browser finishes, rather than an error
page. A reply that arrives after that is dropped: it belonged to a request that
is over, and the next visitor's page is not the place for it.

Timers are not waited for. A page does not wait on a clock.

### The network is allowed by host

An app asking for an address is a visitor's request making your server make a
request, so a render reaches only the hosts you name and makes a bounded number
of requests:

```rust
use lumen_ssr::{FetchPolicy, RenderOptions};

let options = RenderOptions {
    fetch: FetchPolicy {
        max_requests: 4,
        ..FetchPolicy::default().allow_host("api.example.com")
    },
    ..RenderOptions::default()
};
```

Nothing is allowed by default. A request to anything else is answered with an
error the app reads as a failed reply, so the page renders the way it does in a
browser with no network. Subdomains are not implied; name each one.

The transport is yours if you want it: `RenderOptions::dispatch` takes any
`HttpDispatch`, so your own client, timeouts and certificates go there. Whatever
you pass is wrapped rather than replaced, so the counting and the policy still
apply.

## What a document does not carry

Nodes a script builds by hand with the DOM API arrive when the browser runs that
script. The render says so in `response.warnings` rather than leaving you to
find out, and the page is complete once the runtime has started.

Components are not among them, as long as you hand the renderer the app
`lumenc web` compiled. Component bodies are resolved when the site is built, so
they are already markup in the artifact the renderer reads and every response
carries them. An artifact compiled some other way still holds the markers, and
those reach the document as empty elements for the browser to fill.

A script's `t()` returns the key it was given, and its `format_*` calls return
their argument. The translator and the formatters are the desktop runtime's,
and a render installs neither; markup `translatable` and `format` are
unaffected, because both are resolved as the document is written.

The rest of the limits are the emitter's, and a rendered page has the same ones
[a built page](web.md) has.

## Reporting

Every response carries `warnings`: a value a document cannot hold, a header that
was refused, an engine this build has no host for, an app that ran out of
budget. None of them stops a page from being served, and all of them are worth a
line in your log.
