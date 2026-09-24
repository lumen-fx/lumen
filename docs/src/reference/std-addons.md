# Standard browser add-ons

The [browser add-ons](web-addons.md) that ship with the toolchain. Each one
gives a page's scripts a piece of the browser a Lumen app does not otherwise
reach, and each is declared by name with `bundled = true`. They run only in a
page, so declare them for the web build:

```toml
[target.web.dependencies]
lumen-js = { bundled = true }
lumen-storage = { bundled = true }
```

| Dependency | Namespace | What it adds |
|------------|-----------|--------------|
| `lumen-js` | `js` | The page's own JavaScript: global functions and values, modules loaded at run time, DOM events. |
| `lumen-websocket` | `ws` | WebSocket connections, text frames. |
| `lumen-storage` | `storage` | Local and session storage, and changes made in another tab. |
| `lumen-cookie` | `cookie` | The cookies the page can see. |
| `lumen-browser` | `browser` | Popups and the messages they send back, sharing, downloads, the leave-page prompt, media queries, a file picker, fullscreen. |
| `lumen-svg` | `svg` | An element that shows SVG markup a script hands it. |
| `lumen-canvas` | `canvas` | The page implementation of the `lumen-canvas` runtime module. |

Every add-on follows the same naming. Functions are the snake_case of the DOM
method they wrap. A function that answers later takes a tag after its own
arguments and answers as `on_<namespace>_<function>(tag, value)`, or
`on_<namespace>_<function>_error(tag, reason)` when it fails. An event the
browser raises on its own arrives as `on_<namespace>_<what happened>`, with
the key it concerns first.

Types are written the way candela writes them; see
[types](web-addons.md#types). A function listed with no result returns
nothing.

## js

A path is a dotted walk from the global object: `Math.max` is
`globalThis.Math.max`, and a function found there is called with the object
it was found on as `this`. A path is code, so build it from your own strings,
never from what a visitor typed.

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `js::call(path: string, args: any[])` | `any` | Call the function at `path` with `args` and return its result. Raises when it returns a promise; use `call_async`. |
| `js::call_async(path: string, args: any[], tag: string)` | | Call the function at `path` and answer with what its promise resolves to, as `on_js_call_async(tag, value)`. |
| `js::get(path: string)` | `any` | The value at `path`. |
| `js::set(path: string, value: any)` | | Set the value at `path`. |
| `js::load(url: string, tag: string)` | | Import the JavaScript module at `url` and answer with a handle to it, as `on_js_load(tag, handle)`. |
| `js::invoke(handle: int, method: string, args: any[])` | `any` | Call `method` on what `handle` holds. Raises when it returns a promise; use `invoke_async`. |
| `js::invoke_async(handle: int, method: string, args: any[], tag: string)` | | Call `method` on what `handle` holds and answer with its resolved value, as `on_js_invoke_async(tag, value)`. |
| `js::release(handle: int)` | `bool` | Let go of a handle; false when it held nothing. |
| `js::listen(target: string, event: string, key: string)` | `bool` | Turn every `event` on `target` into `on_js_event(key, event)`. `target` is `window`, `document`, or an element id; false when no element has it. Listening again under the same key replaces the listener. |
| `js::unlisten(key: string)` | `bool` | Stop the listener started under `key`; false when there was none. |

`load` resolves a relative URL against the page. It imports from the site's
own origin, and from any URL that starts with a prefix in the `allow` list of
the dependency's `config`; any other URL fails with a reason:

```toml
[target.web.dependencies]
lumen-js = { bundled = true, config = { allow = ["https://cdn.jsdelivr.net/npm/"] } }
```

The map `on_js_event` receives holds the event's `type`, and whichever of
`detail`, `key`, `code`, `button`, `clientX`, `clientY`, `deltaX`, `deltaY`,
`value` (the target's) and `target` (the target's id) the event has.

```
fn on_ready() {
    js::load("https://cdn.jsdelivr.net/npm/canvas-confetti@1/+esm", "confetti");
}

fn on_js_load(tag: string, handle: int) {
    js::invoke_async(handle, "default", [{"particleCount": 80}], "burst");
}
```

To hand a part of the page to your own JavaScript, write an add-on of your own
with an [element](web-addons.md#elements): its `mount` hook receives the
element, and a library such as KaTeX or a code editor draws into it.

## ws

A connection is named by a key the script picks, and every call and event
takes it first. A relative URL, or an `http` or `https` one, connects to the
same place over `ws` or `wss`.

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `ws::open(key: string, url: string)` | `bool` | Open a connection; false when one under `key` is still open. |
| `ws::send(key: string, text: string)` | `bool` | Send a text frame; false when the connection is not open. |
| `ws::close(key: string, code: int, reason: string)` | `bool` | Close the connection with a close code, 1000 for a normal close; false when there is none. |
| `ws::state(key: string)` | `string` | `connecting`, `open`, `closing` or `closed`, which is also the answer for a key with no connection. |

| Event | Arguments |
|-------|-----------|
| `on_ws_open` | `key` |
| `on_ws_message` | `key`, `text` |
| `on_ws_close` | `key`, `code`, `reason`, `clean` |
| `on_ws_error` | `key`, `message` |

Binary frames are not delivered; one arriving raises `on_ws_error`.

## storage

Text values under text keys, in local storage, which lasts, and session
storage, which lasts as long as the tab.

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `storage::get_item(key: string)` | `any` | The stored text, or null. |
| `storage::set_item(key: string, value: string)` | `bool` | Store text; false when the browser refused it, such as over its quota. |
| `storage::remove_item(key: string)` | | Remove a key. |
| `storage::keys()` | `string[]` | Every key. |
| `storage::clear()` | | Remove everything. |
| `storage::session_get_item(key: string)` | `any` | As `get_item`, in session storage. |
| `storage::session_set_item(key: string, value: string)` | `bool` | As `set_item`, in session storage. |
| `storage::session_remove_item(key: string)` | | As `remove_item`, in session storage. |
| `storage::session_keys()` | `string[]` | As `keys`, in session storage. |
| `storage::session_clear()` | | As `clear`, in session storage. |

Another tab of the same site changing local storage arrives as
`on_storage_change(key, new_value, old_value)`. A value that was removed, or
never there, is null; a `clear` in the other tab arrives with an empty key.
The tab that made the change is not told.

## cookie

The cookies in `document.cookie`. Names and values are percent-encoded on the
way in and decoded on the way out, so any text survives. A cookie the server
set `HttpOnly` is not visible to a page at all: `get` does not find it and
`remove` cannot reach it.

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `cookie::get(name: string)` | `any` | The cookie's value, or null. |
| `cookie::set(name: string, value: string, options: {string: any})` | `bool` | Set a cookie; false when the browser did not keep it, as it does not keep a `secure` cookie on an `http` page. |
| `cookie::remove(name: string, options: {string: any})` | | Remove a cookie. `path` and `domain` have to be the ones it was set with. |
| `cookie::keys()` | `string[]` | The name of every cookie the page can see. |

| Option | Value | Default |
|--------|-------|---------|
| `max_age` | seconds | until the browser closes |
| `path` | text | `/` |
| `domain` | text | the page's host |
| `same_site` | `lax`, `strict` or `none` | the browser's |
| `secure` | flag | off |
| `partitioned` | flag | off |

A candela map literal holds one type of value, so every option may be written
as text: `{"max_age": "3600", "secure": "true"}`. An option not in the table
raises, so a misspelled one is not quietly dropped.

## browser

What the browser offers a page outside its own document. Navigation is not
here; [pages](../guides/pages.md) and links own it.

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `browser::open_popup(url: string, name: string, features: string)` | `bool` | Open a popup window under `name`, with `window.open` features such as `"width=500,height=600"`; false when the browser blocked it. |
| `browser::close_popup(name: string)` | `bool` | Close the popup opened under `name`; false when there is none. |
| `browser::post_message(target: string, data: any, origin: string)` | `bool` | Send `data` to `opener`, `parent`, or a popup by name, for a window at `origin` (`"*"` for any); false when there is no such window. |
| `browser::close_window()` | | Close this window. The browser allows it for a window a script opened. |
| `browser::share(data: {string: any}, tag: string)` | | Open the system share sheet for a `title`, `text` and `url`, and answer with `on_browser_share(tag, true)`. |
| `browser::can_share(data: {string: any})` | `bool` | True when the browser can share this `title`, `text` and `url`. |
| `browser::download_text(filename: string, mime: string, text: string)` | | Save text as a file through the browser's download. |
| `browser::guard_unload(on: bool)` | | While on, leaving or reloading the page asks the visitor to confirm. |
| `browser::match_media(query: string)` | `bool` | Whether a media query matches, such as `"(prefers-color-scheme: dark)"`. Changes after this arrive as `on_browser_media_change(query, matches)`. |
| `browser::pick_file(accept: string, tag: string)` | | Ask the visitor for a text file, `accept` as in `<input accept>`, and answer with `on_browser_pick_file(tag, file)`: a map of `name`, `type`, `size` and `text`. A cancelled picker fails. |
| `browser::request_fullscreen(id: string, tag: string)` | | Show the element with this id fullscreen, or the whole page for `""`, and answer with `on_browser_request_fullscreen(tag, true)`. |
| `browser::exit_fullscreen()` | | Leave fullscreen. |
| `browser::is_fullscreen()` | `bool` | True while something is fullscreen. |

| Event | Arguments |
|-------|-----------|
| `on_browser_message` | `origin`, `data`, `source`: `opener`, `parent`, a popup's name, or `""` |
| `on_browser_fullscreen_change` | the fullscreen element's id, or `""`, then whether anything is fullscreen |
| `on_browser_media_change` | `query`, `matches` |

A browser opens a popup, a share sheet, a file picker or fullscreen only just
after the visitor clicked or pressed a key, so call these from a click or key
handler. Asked at any other time, `share`, `pick_file` and
`request_fullscreen` fail with that reason, and `open_popup` answers false.

Every message another window posts to this one arrives, whoever sent it, so a
handler checks `origin` before trusting `data`. A message whose data JSON
cannot write is not delivered. An OAuth flow reads:

```
fn on_click(id: string) {
    if id == "sign-in" {
        browser::open_popup("https://auth.example.com/authorize?...", "auth", "width=500,height=600");
    }
}

// The provider redirects the popup to a page of this site, whose script calls
// browser::post_message("opener", {"code": code}, "https://app.example.com").
fn on_browser_message(origin: string, data: {string: any}, source: string) {
    if origin == "https://app.example.com" && source == "auth" {
        lumen::signal_set("auth_code", str(data.get("code")));
        browser::close_popup("auth");
    }
}
```

## svg

The `<svg-view>` element shows an SVG drawing a script sets as markup. Until
it is given one, it shows the content the markup gives it.

```
<svg-view id="chart" text="Loading the chart" />
```

| Function | Returns | Behaviour |
|----------|---------|-----------|
| `svg::set_markup(id: string, markup: string)` | | Show this SVG document in the `<svg-view>` with the id, in place of what it showed. Raises when the markup is not well-formed SVG. |
| `svg::set_attribute(id: string, selector: string, name: string, value: string)` | `int` | Set an attribute on every element the CSS selector matches in the drawing; how many it matched. |

Markup is parsed as SVG, never as HTML, and cleaned before it reaches the
page: `<script>` and `<foreignObject>` elements, `on...` event attributes, and
any attribute naming a `javascript:` URL are taken out, and `set_attribute`
refuses to set one. A call naming an element that is not in the page yet is kept and applied
when the element arrives, and `set_attribute` then answers 0. The drawing
fills the element's box.

## canvas

`lumen-canvas` is a runtime module on the desktop and a browser add-on in a
page, so declare it once, in `[dependencies]`, and both builds get the same
[`canvas` functions](scripting-candela.md#canvas) and the same
[`<canvas>`](tags.md#canvas) element:

```toml
[dependencies]
lumen-canvas = { bundled = true, tags = ["canvas"] }
```

In a page it draws with the browser's Canvas 2D. What a script draws in one
turn reaches the canvas together, after the script returns. The canvas's
bitmap follows the screen's pixel density, and a canvas nothing in the app's
CSS sizes takes its drawing space as its box, as it does on the desktop. The
`config` caps apply the same way.

Two functions name files, and a page has no file system: `buffer_load_png`
answers 0 and `buffer_save_png` answers false, each with a console warning.
Load an image into a page with the `js` add-on or an add-on of your own.
