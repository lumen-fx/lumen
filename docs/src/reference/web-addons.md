# Web halves of modules

A module an app depends on can carry a web half: files a site loads beside the
Lumen runtime in place of the module's library, which a browser cannot open.
A web half is a JavaScript module, and optionally stylesheets, a script that
runs before the page paints, and files the module reads. It gives an app's
scripts functions backed by that JavaScript, and markup elements it draws. An
app depends on the module in `lumen.toml`; [the web guide](../guides/web.md#modules-in-a-page)
shows the app's side. This page is the format a web half is written in.

## Layout

```
echo/
  web/
    lumen-addon.toml   what the web half offers
    echo.js            the JavaScript module
    echo.css           stylesheets, if any
```

A module's web half is the `web/` directory under its root, and the
descriptor sits at the root of `web/`. Every path it names is relative to
`web/` and inside it. The module's root is found through the app's
dependencies: a `path` source names it, a `version` source names a registry
package on the `lumen` platform, and `bundled = true` names the toolchain's
own copy, `modules/<name>/`, looked for under `--lib-dir`, beside `lumenc`,
then under `LUMEN_LIB_DIR`. A `lumenc` built from a checkout also finds the
crate under its `std/` whose package is `<name>`.

A module with a library too keeps its sources beside `web/`, as the ones under
`std/` do: `Cargo.toml` and `src/` build the library a desktop app loads, and
the two halves offer the same functions. A module with a web half alone is for
pages only. The first-party modules and what each offers are listed under
[`[dependencies]`](lumen-toml.md#dependencies).

## lumen-addon.toml

```toml
[addon]
namespace = "echo"
module = "echo.js"
styles = ["echo.css"]

[[function]]
name = "shout"
params = ["text: string"]
returns = "string"
doc = "The text in capitals."

[[function]]
name = "later"
params = ["text: string", "ms: int"]
async = "on_echo"

[[element]]
tag = "echo-view"
html = "div"
```

An unknown key is an error, and every error names the module.

### [addon]

| Key | Type | Effect |
|-----|------|--------|
| `namespace` | string | The script namespace the functions live in: `echo::shout` in candela. Lowercase letters, digits and underscores, starting with a letter. `lumen`, `native` and `fs` are taken. Required. |
| `module` | string | The JavaScript module, a `.js` or `.mjs` file. Required. |
| `styles` | array of strings | Stylesheets every page links, in this order, after the app's own. |
| `head` | string | A classic script every page runs in its head, before it paints. It blocks the page while it runs, so keep it to what has to happen first, such as applying a stored theme. |
| `files` | array of strings | Other files the module reads at run time. A directory stands for everything under it. |

### [[function]]

| Key | Type | Effect |
|-----|------|--------|
| `name` | string | The name a script calls it by, and the name of the module export that answers it. Lowercase letters, digits and underscores. `install`, `elements` and `default` are taken by the module's own hooks. Required. |
| `params` | array of strings | Its parameters, each written `name: type`. |
| `returns` | string | What it returns, as a type. Default `null`. |
| `async` | string | Makes the function asynchronous and names the event its result arrives as. A function with `async` takes no `returns`. |
| `doc` | string | One line describing it, for editor tooling. |

### [[element]]

| Key | Type | Effect |
|-----|------|--------|
| `tag` | string | The markup tag. Lowercase letters, digits and dashes; a tag Lumen already has cannot be taken. Required. |
| `html` | string | The HTML element it is written as, such as `div`, `canvas` or a custom element name. Required. |
| `void` | boolean | True when that element takes no children and no end tag. Default false. |

### Types

Types are written the way candela writes them:

| Type | Is |
|------|----|
| `int` | A whole number. |
| `float` | A number. A whole number is accepted where one is declared. |
| `bool` | `true` or `false`. |
| `string` | Text. |
| `null` | Nothing; only useful as `returns`. |
| `any` | Any value. |
| `T[]` | A list of `T`. |
| `{string: T}` | A map from text to `T`. |

A script's call is checked against these the way a call to any other host
function is, before the module sees it.

## The module

The page imports the module and hands it to the runtime. Everything the
runtime reads from it is a named export:

```js
let host;

export function install(given, config) {
  host = given;
}

export function shout(text) {
  return text.toUpperCase();
}

export function later(text, ms) {
  return new Promise((resolve) => setTimeout(() => resolve(text), ms));
}

export const elements = {
  "echo-view": {
    mount(element) { element.textContent = "drawn by echo"; },
    update(element, name, value) { element.dataset[name] = value; },
    unmount(element) {},
  },
};
```

| Export | Called |
|--------|--------|
| `install(host, config)` | Once, when the app starts, before its scripts run, with the `config` table the app's dependency entry gave the module as an object (empty when it gave none). Optional. |
| one function per `[[function]]` | When a script calls that function, with the script's arguments. |
| `elements` | An object keyed by tag, holding each element's hooks. Optional. |

A function the descriptor names and the module does not export raises in the
script that calls it. An exception thrown from `install` is written to the
console and the app starts anyway.

### The host

`install` receives, first, the object the module reaches the app through,
outside a call:

| Member | Does |
|--------|------|
| `host.emit(event, key, ...values)` | Calls the script's `event` handler with `key` first and up to six values after it. A handler registered for that key with `on(event, key, fn)` is called instead. |
| `host.setSignal(name, value)` | Writes a global signal, on the next tick. |
| `host.getSignal(name)` | Reads a global signal as of the last tick, or as this module last set it, or `undefined`. |

`setSignal` stores text as text, a whole number as an integer, another number
as a float, a boolean as a boolean, and anything else as its JSON text.
`getSignal` gives back text, numbers and booleans, a color as an `rgba(...)`
string, and a vector as a two-element array.

## Values

Arguments and results cross as JSON: a script's value becomes the plain
JavaScript value JSON would give, and a value coming back is read as JSON
would write it.

| Script | JavaScript |
|--------|------------|
| integer | number |
| float | number |
| string | string |
| bool | boolean |
| null | `null` |
| list | array |
| map | object |

A number without a fractional part comes back as an integer. An integer past
2^53 does not survive the trip exactly. `undefined` and a function come back
as nothing, and a value JSON cannot write, such as one that contains itself or
a `BigInt`, fails the call. Handed to `host.emit`, such a value is reported to
the console and the event is not sent. An object that stands for something only
JavaScript can hold, such as a DOM node or a connection, stays in the module:
hand the script a number or a string to name it by.

## Answering now or later

A function without `async` answers before it returns. Its return value is the
script's result, and an exception it throws raises in the script with the
exception's message. One that returns a promise raises too, because the
script is not waiting for it: declare it `async` instead.

A function with `async` takes a tag after its declared parameters, and the
script's call returns at once with nothing. The module's function is called
with the declared arguments only, and returns a promise or a value. When it
settles, the script's handler is called:

| Outcome | Handler | Arguments |
|---------|---------|-----------|
| resolved | the `async` event, such as `on_echo` | the tag, then the value |
| rejected, or thrown | the event with `_error` on the end, such as `on_echo_error` | the tag, then the reason as text |

```
fn on_ready() {
    echo::later("world", 5, "first");
}

fn on_echo(tag: string, value: any) {
    lumen::signal_set("answer", tag + ": " + str(value));
}

fn on_echo_error(tag: string, reason: any) {
    lumen::signal_set("answer", "failed: " + str(reason));
}
```

## Elements

An element the web half declares is written in markup like any other tag,
with the text and children a visitor should see until the web half takes it
over. The
page writes it as the `html` element, carrying `data-lm-foreign` with its tag,
and puts that content inside.

From there the element is the web half's. The runtime binds it to its node, so a
script reaches it by `id` and its classes, attributes and inline style are
written onto it, but nothing inside it is a node the runtime keeps, and no text
is written into it. The hooks under `elements[tag]`, each optional, hear the
rest:

| Hook | Called |
|------|--------|
| `mount(element)` | Once the element is in the page and bound, including one built after the page loaded. |
| `update(element, name, value)` | When an attribute the app sets changes, and with `name` set to `text` for the node's text: once when the element is bound, and again whenever it changes. |
| `unmount(element)` | Before the element leaves the page. |

A hook that throws is written to the console and the app carries on.

## Where a web half runs

Only in a page. A build that runs the app on the machine that builds it still
has to compile and run the app, so there each function is bound to a body
that raises `<namespace>::<function> runs only in a browser` in the script
that calls it:

- A build that runs the app, `prerender = "run"`, warns naming each function
  called, and writes the page with the state the app had without it.
- A server rendering a page puts the same warning in the render's warnings.

An element shows the content the markup gives it wherever the web half is not
running.

A desktop build does not run a web half: it loads the module's library. It
reads the descriptor, as every compile does, so the app's scripts compile
against the functions it declares and its markup against the elements; at run
time those calls bind to the functions the library registers. A
module whose desktop half has nothing to do but say "runs only in a browser"
still ships a library that says it, so a script calling it runs on every
target; `lumen_module::BrowserOnly` builds that library's functions from the
module's own descriptor. A module with a web half and no library belongs
under [`[target.web.dependencies]`](lumen-toml.md#targetweb-and-targetdesktop).

## In the site

`lumenc web` copies the module, the stylesheets, the head script and the files
into `addons/<name>.<hash>/`, keeping their paths, so the module's relative
imports and fetches resolve as they did in the package. The hash is taken from
every file in it, so changing any file is a new address.

Every page links the stylesheets, runs the head script and preloads the module,
and holds each of them to a SHA-384 Subresource Integrity hash of the bytes the
build copied. An import map carries the same hash for the module import. The
boot script imports every web half's module and hands them to the runtime in the
order `lumen.web.json` lists them, so a module that fails its check, or fails
to load at all, keeps the app from starting: the page reads as it was written,
and the browser console names the file. A page built without the runtime links
the stylesheets alone.

The `config` table a web half is given travels in `lumen.web.json`, where any
visitor can read it, so it is no place for a secret.

## Candela sugar

A web half that is also a candela package, with a `candela.toml` or a
`src/main.cdl` inside `web/`, is an import root under the module's dependency name too, so
it can ship candela wrappers over its functions:

```
import "echo";
```
