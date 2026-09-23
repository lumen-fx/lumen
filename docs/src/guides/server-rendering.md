# Rendering on a server

`lumen-ssr` renders a Lumen app to an HTML document once per request. The same
app, the same markup and the same scripts a build writes a static site from,
except that the state is read for the visitor asking: the address they asked
for, the language their browser wants, the record their path names, the data an
API answered a moment ago.

Reach for it when a page depends on who is asking or on data that changes
faster than you rebuild. A page whose content is the same for everybody wants
[a build](web.md) instead, which costs nothing to serve.

Two things ship. `lumen-server` is the server you deploy: point it at the
directory a build wrote and it serves the site, rendering every page. The
`lumen-ssr` crate is the renderer inside it, with no transport of its own: a
request goes in as a struct and a response comes back as one, so it also goes
inside a server you already have.

## What you serve

A render produces documents and nothing else. Everything else a page loads
still comes from `lumenc web`:

```
lumenc web myapp --render ssr
```

That writes the stylesheet, the compiled app, the compiled candela program,
the browser runtime pair, `lumen.web.json`, the catalogues, `assets/` and
`lumen.site.json`, and no documents: serve the files as static files and
answer everything else with a render. The runtime adopts a rendered document
the same way it adopts a built one.

`lumen.site.json` is the server's file. It names every other file, and it
carries what the build knew that the compiled app does not: the address and
canonical URL, each page's title and description, the locales and their
fallback chain, the size of every image, the `[web.seed]` values, and the
app's `[web.ssr]` policy. Read it and you hold the site `lumenc web --serve`
renders. The browser never loads it.

Every other file is named after its own contents, so the compiled app is
`app.<hash>.lmna` rather than `app.lmna`. Read the names out of
`lumen.site.json` rather than writing them into your server: they change
whenever the app does.

Add `--no-runtime` to render pages that carry none:

```
lumenc web myapp --render ssr --no-runtime
```

Each page is still produced for the request that asks, and now it is only a
document: no wasm, no boot script, and nothing that takes it over once it is
open. Links load the next page, which is another render. The compiled app and
`lumen.site.json` are still written, because that is what you render from; the
runtime files, the manifest and the catalogues are not, because nothing in a
browser loads them. The compiled app carries the catalogues itself, so
`lumen.site.json` names none and `SsrSite::from_build` renders every language
from the app alone when it is handed no catalogue.

Build with `--render csr` instead when you want documents to fall back to.
Which of the two answers a request is then yours to decide, and so is
rebuilding them when the app changes.

## Trying it without writing a server

```
lumenc web myapp --render ssr --serve
```

That emits the site, then runs `lumen-server --dev` on it, with every page
coming from a render of the app for the request that asked. Development mode
listens on 127.0.0.1, and `--host <addr>` is what widens that and says so; a
render that fails says why in the page; there are no worker processes, and
there is no render time limit. Anything the public reaches belongs on
[`lumen-server`](#running-in-production) without `--dev`.

Files keep their own path through it: a stylesheet, an artifact and the wasm
module are read from the directory the build wrote while a page is being
rendered, so nothing a page needs waits behind the page. Renders queue, because
a process renders one at a time.

A render reaches the hosts `[web.ssr] allow_hosts` lists and no others, the
same policy an embedder applies with `with_policy`. `--allow-host` adds one
for this run:

```
lumenc web myapp --render ssr --serve --allow-host api.example.com
```

Warnings a render comes back with are printed once each, so a page reloaded
twenty times does not bury a new one. Whether anything is reading them is not
the server's business: a log line that cannot be written is dropped, rather
than ending a process in the middle of answering somebody.

## Running in production

`lumen-server` serves a site built with `--render ssr`. It installs beside
`lumenc`, and it is published as a container image. It serves a site built
with `--render static` or `csr` too, as the files it holds.

```
lumenc web myapp --render ssr --out dist/web
lumen-server dist/web
```

It reads `lumen.site.json` and nothing else about the app: not `lumen.toml`,
not the app's source. What the app allows a render to do, its fetch hosts,
its request cap and the headers it reads, is the `[web.ssr]` policy the build
wrote into that file. How the server runs is yours to say, with flags or with
the `LUMEN_*` variable beside each one; a flag wins over its variable. The
[reference](../reference/cli.md#lumen-server) lists them all.

```
lumen-server dist/web --bind 0.0.0.0 --port 8080 --workers 4 --log-format json
LUMEN_BIND=0.0.0.0 LUMEN_WORKERS=4 lumen-server dist/web
```

It listens on 127.0.0.1 unless `--bind` says otherwise. TLS and compression
are left to the reverse proxy or load balancer in front of it.

### Workers

A process renders one page at a time, so `--workers N` runs N worker
processes that share the port. A supervisor binds the port, starts the
workers, and keeps the socket open while it replaces one that exits, so a
connection that arrives meanwhile waits rather than being refused. Files are
served while a page renders, on every worker.

Each worker holds a bounded number of connections and a short queue of
requests waiting for a render. A page asked for when the queue is full is
answered with a 503 and `Retry-After`, so a balancer sends it elsewhere rather
than letting it wait behind everyone else.

Windows has no supervisor: `lumen-server` runs as one process there, and
`--workers` above 1 is refused. Run one per port behind your balancer instead.

### Health

Two endpoints answer on every worker:

| Path | Answers |
| --- | --- |
| `/_lumen/healthz` | 200 while the process is running. For liveness. |
| `/_lumen/readyz` | 200 when a page asked for now would be rendered, and 503 while the server is stopping or its render queue is full. For readiness. |

`--health-path /ops` moves them to `/ops/healthz` and `/ops/readyz`, for an
app with a page of its own at `/_lumen`. `lumen-server probe` asks the liveness
endpoint of the server its own flags and variables describe, and exits 0 when
it answers; that is what the container image checks itself with.

### Logging

Every request is one access line on standard output: the time, the visitor's
address, the method and target, the status, the body size, and how long it
took. The server's own messages, and each warning a render comes back with,
go to standard error; a warning is written once however many renders repeat
it. `--log-format json` writes both as one JSON object per line for a log
collector.

No header value is ever written, so a `Cookie`, `Authorization` or
`Proxy-Authorization` never reaches a log.

A render that fails is logged with what went wrong, and the visitor gets the
status and nothing else. Only `--dev`, which `lumenc web --serve` runs, puts
the reason in the page.

### Stopping

On SIGTERM or Ctrl-C the server reports itself not ready, stops accepting,
closes idle connections, and lets the requests it is answering finish. It
exits once they have, or when `--shutdown-grace` runs out.

### Recycling and runaway renders

`--max-renders N` retires a worker after about N renders and the supervisor
starts a fresh one; each worker picks a slightly different number, so they do
not all restart at once. A process's memory grows slowly over many renders,
and a fresh process is how to give it back.

A render that ticks past `--render-timeout` cannot be stopped from inside the
process that is running it. Its visitor gets a 504, and the worker finishes
what else it is answering and exits so a fresh one takes over. Requests queued
behind that render are answered with a 503.

### Behind a proxy

Whether a request arrived over TLS, and from where, is what the proxy in
front says in `X-Forwarded-Proto` and `X-Forwarded-For`. Those headers are
believed only from the addresses `--trusted-proxy` names, and dropped from
every other request before the app sees them:

```
lumen-server dist/web --trusted-proxy 10.0.0.0/8
```

With a trusted proxy, `request.secure` reads what the proxy said about TLS and
the access log records the visitor's address rather than the proxy's.

A request that a proxy and the server could read two ways, such as one with a
folded header line, whitespace before a header's colon, or two
`Content-Length` headers, is answered with a 400 and its connection closed, so
the two never disagree about where one request ends and the next begins.

### Caching

A file whose name carries the hash of its contents, which is every file a
build writes except the documents, `lumen.web.json` and `lumen.site.json`, is
served with `Cache-Control: public, max-age=31536000, immutable`: a new build
is a new name, so a browser or CDN never has to ask again. Every other file is served
with `no-cache`. A rendered page is `no-store` unless the app sets its own
`Cache-Control` with `response_header`. `lumen.site.json` is never served.

### The container image

`ghcr.io/lumen-fx/lumen-server` carries the same binary for Linux on x86_64
and aarch64, tagged with each release version and `latest`, plus `nightly`
for the newest nightly build. It listens on port 8080 on every interface, runs
as a user without root, checks its own health with `lumen-server probe`, and
serves the site mounted at `/site`:

```
lumenc web myapp --render ssr --out dist/web
docker run --rm -p 8080:8080 -v "$PWD/dist/web:/site:ro" ghcr.io/lumen-fx/lumen-server
```

Flags go after the image name, and the variables work with `-e`:

```
docker run --rm -p 8080:8080 -v "$PWD/dist/web:/site:ro" \
  -e LUMEN_WORKERS=4 -e LUMEN_LOG_FORMAT=json \
  ghcr.io/lumen-fx/lumen-server --trusted-proxy 10.0.0.0/8
```

Move the port with `LUMEN_PORT` rather than `--port`, so the health check asks
the port the server listens on. To ship the site inside an image of your own:

```dockerfile
FROM ghcr.io/lumen-fx/lumen-server
COPY dist/web /site
```

## Rendering

```rust
use std::path::Path;
use std::sync::Arc;
use lumen_ssr::{RenderOptions, Renderer, SERVER_SPEC_FILE, ServerSpec, SsrRequest, SsrSite};

let dir = Path::new("dist/web");
let spec = ServerSpec::from_json(&std::fs::read(dir.join(SERVER_SPEC_FILE))?)?;
let artifact = std::fs::read(dir.join(&spec.web.artifact))?;
let mut catalogues = Vec::new();
for (tag, path) in &spec.web.catalogues {
    catalogues.push((tag.clone(), std::fs::read_to_string(dir.join(path))?));
}
let site = SsrSite::from_build(&artifact, &spec, catalogues)?;
let options = RenderOptions::default().with_policy(site.policy());
let renderer = Renderer::start(Arc::new(site), options)?;

let response = renderer.render(
    SsrRequest::get("/user/42?tab=posts").with_header("Accept-Language", "en-GB"),
)?;
// response.status, response.headers and response.body are what to send.
```

`SsrSite::from_build` takes the files already read, so where they live and
how they are read stays your server's business. The site it builds is the one
`lumenc web --serve` renders: a tree per locale, translated from the
catalogues, with the titles, image sizes and declared state the build wrote
down. `with_policy` applies the app's `[web.ssr]` policy; add to the options
after it for anything your deployment allows on top.

A file written by another version of `lumenc` is refused with a message
naming both versions, rather than read into a site that points at the wrong
files. Rebuild the site with the `lumenc` that matches your server.

This is what `lumen-server` does for every page. Embed it yourself when the
pages belong inside a server you already run.

`render` blocks until the document is written, and it is safe to call from any
thread: calls queue. A server facing the public bounds the queue with
`RenderOptions::queue` and calls `try_render` with a time limit instead: a
full queue answers `SsrError::Busy` at once, and a render still running past
the limit answers `SsrError::TimedOut`. A render past its limit cannot be
stopped, so the renderer answers nothing after it and `is_stopped` says so;
the process that owns it is the thing to restart.

A script failure stays inside the render it happened in. A handler that raises,
and the script engine itself giving up mid-call, both end that one call, are
reported, and leave the renderer serving; the document goes out with whatever
the script had written by then. A panic from the app's own Rust is the other
case: it takes the renderer with it, and the requests after it are answered
with what happened and a 500. Start the server again once you have fixed what
panicked.

## More than one language

A site holds one tree per language it answers in, and a request picks one. The
trees are the app already translated: every string an element marked
`translatable` shows is resolved into the markup before a document is written
from it, and text carrying a `format` is
written for the tree's locale as the document is, so a page arrives in its
language with nothing running.

`lumenc web` builds them from the `locale/*.ftl` catalogues beside your markup,
one per `[web] locales` entry, and `SsrSite::from_build` builds the same trees
from `lumen.site.json`. A server that builds its site by hand, from
`SsrSite::new`, builds a tree the same way and hands it over:

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

The app a render runs reads the catalogues too: a script's `t()` answers in the
language of the tree the request resolved to, and `locale()` names it. Hand the
renderer the catalogue sources, and the fallback chain `[app] fallback_locale`
names, with `with_catalogues`:

```rust
let site = site.with_catalogues(
    vec![
        ("en-US".to_string(), english_ftl),
        ("de-DE".to_string(), german_ftl),
    ],
    Vec::new(), // the default chain, which ends in en-US
)?;
```

A catalogue that will not load is refused here rather than on the first
request. A site with no catalogues renders with no translator, and `t()`
answers with its key.

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
them; `lumen-server --workers` does that on one machine. That is the scaling
story, and it is the one to plan for.

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

`[web.ssr] headers = ["authorization"]` in `lumen.toml` says the same thing
from the app's side, and `with_policy` applies it.

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

An app names its own hosts and cap in `lumen.toml`, and `with_policy` applies
them:

```toml
[web.ssr]
allow_hosts = ["api.example.com"]
max_requests = 4
```

The transport is yours if you want it: `RenderOptions::dispatch` takes any
`HttpDispatch`, so your own client, timeouts and certificates go there. Whatever
you pass is wrapped rather than replaced, so the counting and the policy still
apply.

## What a document does not carry

Nodes a script builds by hand with the DOM API arrive when the browser runs that
script. The render says so in `response.warnings` rather than leaving you to
find out, and the page is complete once the runtime has started. What a script
writes onto an element the markup declares is another matter: a class set with
`set_class` or `set_root_class`, an attribute, an inline style or text set
through a node handle is in the document the render returns, and the runtime
starts from the same values.

Components are not among them, as long as you hand the renderer the app
`lumenc web` compiled. Component bodies are resolved when the site is built, so
they are already markup in the artifact the renderer reads and every response
carries them. An artifact compiled some other way still holds the markers, and
those reach the document as empty elements for the browser to fill.

A component written inside a `<for>` is rendered per row from the state that
request settled into, so the rows and their bodies come from one run, and a
body reads in the language of the tree the request resolved to.

A script's `format_*` calls return their argument: a render installs no
formatter. Markup `format` is unaffected, because it is resolved as the
document is written.

The rest of the limits are the emitter's, and a rendered page has the same ones
[a built page](web.md) has.

## Reporting

Every response carries `warnings`: a value a document cannot hold, a header that
was refused, an engine this build has no host for, an app that ran out of
budget. None of them stops a page from being served, and all of them are worth a
line in your log.
