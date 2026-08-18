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
lumenc web myapp
```

Serve `styles.css`, `app.lmna`, `app.cdlb`, `lumen-web.wasm`, `lumen-web.js`
and `assets/` as static files, and answer everything else with a render. The
documents `lumenc web` wrote are the fallback for anything you do not render,
and the runtime adopts a rendered document the same way it adopts a built one.

## Trying it without writing a server

```
lumenc web myapp --ssr
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

A renderer holds one site and a site is in one language, so a build emitted in
several is rendered in the first one and the trees of the others are answered by
the documents beside them.

A render reaches no host unless you name it, the same policy an embedder sets
in `FetchPolicy`:

```
lumenc web myapp --ssr --allow-host api.example.com
```

Warnings a render comes back with are printed once each, so a page reloaded
twenty times does not bury a new one.

## Rendering

```rust,no_run
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

```rust,no_run
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

```rust,no_run
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

The rest of the limits are the emitter's, and a rendered page has the same ones
[a built page](web.md) has.

## Reporting

Every response carries `warnings`: a value a document cannot hold, a header that
was refused, an engine this build has no host for, an app that ran out of
budget. None of them stops a page from being served, and all of them are worth a
line in your log.
