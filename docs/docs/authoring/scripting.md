# Scripting

Behavior lives in script, not markup: event handlers, reactive state,
timers, native dialogs, hotkeys, and the dynamic DOM API. Lumen ships three
script hosts - candela, Rhai, and Lua - all exposing the same lifecycle
callbacks and close to the same function surface, so the choice is mostly
about which language you or your team want to write app logic in.

This page is the overview: how to attach a script, how the host is chosen,
what concepts are shared across all three, and where they differ.
It is not the API list - each host has its own exhaustive reference:

- [Candela scripting reference](../reference/scripting-candela.md)
- [Rhai scripting API](../reference/scripting-rhai.md)
- [Lua scripting API](../reference/scripting-lua.md)

The candela language itself (syntax, types, standard library, outside of
what Lumen adds) is documented at <https://candela.lumenfx.dev/>.

## Choosing a host

candela is the default. Absent any configuration, an app's script runs on
candela.

The selection order, checked in this sequence:

1. An explicit `[script] engine` in `lumen.toml` wins outright.
2. Otherwise the app directory's script file extensions decide: a `.cdl`
   file selects candela, else a `.lua` file selects Lua, else a `.rhai`
   file selects Rhai.
3. An app with no script file extension to read at all (for example, a
   script written entirely as an inline `<script>` body in markup) falls
   back to candela.

The runtime applies this selection rule to `lumenc run` itself as well as
to what a static `--bundle` compiles in: a `.rhai` file
sitting in an app directory with no `[script]` block runs on
Rhai, and an app with a `.cdl` file and no `[script]` block runs
on candela. Set `[script] engine` explicitly whenever you want the choice
to be unambiguous regardless of what files happen to be in the directory.
See [Per-app config](./lumen-toml.md#script) for the full key reference.

```toml
[script]
engine = "candela"   # "candela" (default) | "rhai" | "lua"
```

## Attaching a script

Reference the file from markup, or write the script body inline:

```xml
<script src="main.cdl" />
```

An inline `<script>...</script>` body works too; every inline body and
every `src` file concatenate, in document order, into one combined source
compiled by whichever single host the app selected. There is no way to mix
hosts within one app.

A candela script opts into the whole Lumen surface with one import line:

```candela
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "ready");
    lumen::on("click", "bump", "handle_bump");
}

fn handle_bump(id) {
    lumen::signal_set("greeting", "clicked");
}

fn main() {}
```

`import "lumen.cdl";` declares the whole host surface in one line; without
it a candela script gets no builtins at all, which is the point - the
import is the opt-in. Rhai and Lua need no such import: their builtins are
plain global functions, always present.

`main` runs once when the program loads; keep it empty and do setup in
`on_start` or `on_ready`. `on_start` runs before the first tick, when no
element has been laid out or queried yet; `on_ready` runs on the first
tick, after the element tree is mounted and queryable, which is where you
build an initial dynamic-DOM tree.

## Lifecycle callbacks

These are functions you write; the runtime calls them by name when the
matching thing happens, and a script that omits one just never gets that
event. All three hosts share the same set: `on_start`, `on_ready`,
`on_click` / `on_double_click` / `on_long_press`, `on_toggle`, `on_slider`,
`on_text_input`, drag-and-drop and file-dialog callbacks, `on_fetch` /
`on_fetch_error`, `on_timer`, `on_menu`, `on_hotkey`, `on_tray`, dialog
callbacks, and `on_close`. `lumen::on(event, id, handler)` (candela) /
`on(event, id, handler)` (Rhai, Lua) routes one element's event to a named
function instead of the matching global dispatcher, for every event in that
list. See the per-host reference for the full event table and each
callback's exact arguments.

## Signals

Signals are the named reactive values `bind-text`, `bind-value`, `<if>`,
and `<for>` read. Every host can read and write them; the spelling differs
more here than almost anywhere else in the surface - see
[What differs between hosts](#what-differs-between-hosts) below.

```candela
lumen::signal_set("greeting", "hello");
let text = lumen::signal_get("greeting");
lumen::signal_set_int("count", 0);
```

`lumen::derive(name, deps, f)` (candela) / `derive(name, deps, f)` (Rhai,
Lua) registers a computed signal: `f` runs whenever a dep changes and its
return value lands in `name`. A derivation also runs once right after
registration, so the bound value is correct on the first frame even before
any dep has changed.

## The dynamic DOM API

Beyond signals, a script can reach into the live element tree: find nodes
with selectors, walk between them, spawn and move and remove elements, edit
classes and inline style, and bind event listeners with capture and bubble
phases. It is the DOM model, with the same vocabulary, on a node handle
that is an opaque token rather than a live reference - resolve it inside
the handler that uses it rather than caching it across ticks.

candela reaches this through method syntax. The shipped `lumen.cdl`
prelude wraps a raw node handle in a `Node` struct (and an event handle in
an `Event` struct) and declares `impl Node` / `impl Event` blocks over the
same host functions, so a script writes `node.set_text(...)` rather than
`lumen::node_set_text(node, ...)`:

```candela
fn add_row(container, title) {
    let row = spawn("row");
    row.set_attr("class", "list-row");
    container.append(row);

    let cell = spawn("label");
    cell.set_text(title);
    row.append(cell);
}

fn on_ready() {
    let list = get_by_id("todos-list");
    if list.exists() {
        add_row(list, "First row");
    }
}
```

`spawn(tag)`, `get_by_id(id)`, `query(selector)`, and `document_node()` are
the plain (non-namespaced) constructors that hand back a wrapped `Node`;
every `Node` method that returns another node (`.parent()`, `.next()`,
`.children()`, ...) stays wrapped too, so a chain like
`container.first_child().next()` never drops back to a bare handle. The
underlying `lumen::node_*` free functions the prelude wraps still work
directly on a raw handle (an `int`) when you need them - the method sugar
is a convenience layer over the same calls, not a replacement API. See the
[apps/notes](https://github.com/lumen-fx/lumen/blob/main/apps/notes/main.cdl)
and
[apps/widget-garden](https://github.com/lumen-fx/lumen/blob/main/apps/widget-garden/main.cdl)
example apps for this in a real, runnable app - both build list rows this
way instead of via `<for>` + `signal_array`.

Rhai and Lua expose the same operations differently: `query(selector)`
returns a result-set object and `Node` methods that mutate return the
receiver so calls chain. Lua spawns a fresh element with `spawn(tag)`, same
as candela; Rhai reserves `spawn` as a keyword in its own tokenizer, so Rhai
source spells the same operation `document.create(tag)` (or the global
`create(tag)`) instead. See the per-host reference for the exact method
list.

## What differs between hosts

The lifecycle, signals, and dynamic-DOM concepts above are the same
surface everywhere; these are the places the hosts diverge.

| | candela | Rhai / Lua |
|---|---|---|
| Default when unconfigured | Yes | No - needs `[script] engine` or a matching `.rhai` / `.lua` file present |
| Reading/writing a signal | Typed function pairs: `signal_get` / `signal_set`, `signal_get_int` / `signal_set_int`, ... | A `Signal` handle object: `signal(name, default)` then `.get()` / `.set(v)` / `.value` |
| Building a reactive list | No `signal_array`; build real DOM nodes one by one (see above) | `signal_array(name)` backs `<for each="name">` directly |
| `derive` / event handler reference | By function name (candela has no closure value) | A closure literal, or a function name |
| DOM node calls | Method syntax on a wrapped `Node` / `Event` (see above) | Method syntax on a native `Node` object; mutators return the receiver so calls chain |
| Parsing JSON | Language builtin `json_parse` + `as_map` / `as_list` / `as_str` | `parse_json(s)` |
| HTTP | `fetch(url, tag)` only (GET) | `fetch(url, tag)` plus `http(request)` for method / headers / body |
| Page navigation | `window::set_href(path)` / `window::href()` | `page(path)` / `page()` |

candela's gaps here are not arbitrary: `parse_json`'s any-shaped return and
`http(request)`'s mixed-type request map both need a value shape a typed
host-function boundary cannot declare, and a closure argument has nowhere
to live without a first-class closure value in the language. See
[Limitations](../reference/scripting-candela.md#limitations) in the
candela reference for the complete list, including which low-level
introspection readers are registered on the Rust side but have no reachable
candela declaration.

## File-based pages

`page()` (Rhai, Lua) / `window::set_href(path)` (candela) is the scripting
side of file-based multi-page navigation; see [Pages](./pages.md) for the
full picture, including `<a href>`, the `route.path` / `route.segment`
signals, and the shared-layout convention.
