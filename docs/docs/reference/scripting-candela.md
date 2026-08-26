# candela scripting reference

Every builtin the candela script host registers, with its signature, parameters,
and behaviour. candela is Lumen's default script language; for the task-oriented
introduction see [Scripting](../guides/scripting.md). The candela language itself
is documented separately at [/candela/](https://docs.lumenfx.dev/candela/); this
page covers only the Lumen surface.

The same surface in the other hosts, under the same names:
[rhai](scripting-rhai.md), [lua](scripting-lua.md). One structural difference
shapes those names: a candela host function is keyed by name alone and cannot
be overloaded on arity, so a call with two forms gets two names on every host:
`page(path)` and `page_current()`, `computed_style(prop)` and
`computed_style_all()`.

## Selecting the host

Each script file picks its host from its own extension: `.cdl` runs under
candela, `.lua` under Lua, `.rhai` under Rhai. Files of one language combine
into a single program; an app that ships two languages runs both hosts, sharing
signals but not functions.

An inline `<script>` block has no extension. It joins the app's one external
language when there is exactly one, and candela otherwise.

`lumen.toml` overrides all of it and puts every script on one engine:

```toml
[script]
engine = "candela"   # "candela" (default) | "rhai" | "lua"
```

## Reaching the builtins

candela resolves host functions through a typed `host "lumen" { ... }` block. One
import line declares the whole Lumen surface:

```rust
import "lumen.cdl";

fn on_start() {
    lumen::signal_set("greeting", "hi");
}

fn main() {}
```

The import is replaced with the declarations before compilation, so no
`lumen.cdl` file is read from disk. Without the import (or a hand-written `host`
block), the source still compiles, and calling a builtin fails at run time with
`lumen is not a valid namespace`.

Write the import in every `.cdl` file that uses the surface. An app's candela
files join into one program and the declarations land once for the whole
program, so a repeated import costs nothing.

The prelude also declares the `window`, `document`, and `history` namespaces and
defines the `Node`, `Event`, `Signal`, and `ArraySignal` method wrappers
described below.

Types in signatures are candela types: `int`, `float`, `bool`, `string`, arrays
(`int[]`), and maps (`{string: float}`). A builtin with no return type returns
null.

### Dynamically-shaped builtins

A few builtins carry a value with no single concrete type: an array signal's
records, an `http` request, a `parse_json` result. Those are declared with a
`...` argument list and, where they return a value, the `any` return type. The
tables below give what each one takes and returns; the prelude declares them, so
an app calls them like any other builtin.

Read an `any` result with candela's `as_map`, `as_list`, `as_str`, `as_int`,
`as_float`, and `as_bool` downcasts, and test it with `is_map`, `is_list`,
`is_null`, and their siblings.

### Writing arguments

Three candela rules shape how you write a call. Each has the same remedy: bind
the value to a variable first.

- A map literal holds one value type. `{"id": "a", "n": 1}` is rejected; write
  `{"id": "a", "n": "1"}`, or build the value from `parse_json`.
- A collection literal passed directly to a script-level function, including an
  `impl` method, aborts the compiler. A literal passed straight to a `lumen::`
  builtin is fine.
- A builtin call nested inside another builtin's argument list mislays its
  arguments.

```rust
let rows = signal_array("rows");
let row = {"id": "a", "title": "First"};
rows.push(row);                                  // literal through a variable
lumen::signal_array_push("rows", {"id": "b"});   // literal straight to a builtin

let n = lumen::signal_array_len("rows");         // not nested in the next call
lumen::signal_set_int("row_count", n);
```

## The candela standard library

The toolchain carries candela's own standard library, so a script imports a
module by name and gets the functions the candela documentation describes:

```rust
import "std/time";

fn on_start() {
    lumen::signal_set("started", str(now()));
}

fn main() {}
```

The array methods (`arr.map(f)`, `arr.sum()`, and the rest) come from
`std/list`, which the compiler loads whether or not a program imports anything.

The modules are read from disk when the program compiles, which puts two limits
on where they reach. A packaged app compiles its scripts as it starts and
carries no library beside it, so an import that resolves under `lumenc run`
fails from `lumenc package` output. A web build compiles ahead of time, so the
text modules travel into the browser inside the compiled image; `std/math`,
`std/random`, and `std/time` bind a C library the browser cannot open, and an
image naming one is refused whole.

## Lifecycle hooks

Define these as free functions. Each is optional; a missing hook is a no-op.

| Hook | Fires |
| --- | --- |
| `on_start()` | Once at app construction, before the first tick. No element is queryable yet: `node_get_by_id` returns `0`. |
| `on_ready()` | Once per mount, on the first tick after the element tree is published. Queries resolve here. Re-armed after a hot reload, so it runs again on the fresh tree. |
| `on_close()` | On an OS close request, before teardown. Return `false` to veto the close and keep the window open. |
| `on_audio_end(path: string)` | When a track played through the `lumen-audio` module reaches its end; `path` is the path passed to `audio_play`. |
| `main()` | candela's module entry point, run once at compile time. Keep it empty unless the app needs module-level setup. |

Across a hot reload the signal values, the `on(...)` routing table, and live
event bindings are preserved; the recompiled program picks them up.

## Event handlers

Each event dispatches to a global handler function named below. Handlers are
optional.

Annotate a handler's parameters with the types the table gives:

```rust
fn on_toggle(id: string, checked: bool) { }
```

Running from source works either way, because the compiler takes a bare
parameter's type from the first call. A compiled app has no compiler to do
that, so it records a call trampoline for each handler at build time and only
records one where every parameter is annotated. A handler left bare compiles,
ships, and is then never called.

| Handler | Arguments |
| --- | --- |
| `on_click(id)` | Element id. Suppressed when a double-click fires on the same element in the same tick. |
| `on_double_click(id)` | Element id. |
| `on_long_press(id)` | Element id. |
| `on_toggle(id, checked)` | Element id, `bool`. |
| `on_slider(id, value)` | Element id, `float`. |
| `on_text_input(id, text)` | Element id, current text. Fires on every edit that changes the text, and once more on Enter commit. |
| `on_drop(target_id, payload)` | Drop-zone id, the source's text payload. |
| `on_drag_start(source_id, payload)` | Drag-source id, its text payload. |
| `on_file_dropped(id, path)` | Element id, dropped file path. |
| `on_file_picked(tag, path)` | Dialog tag, chosen path. Empty path on cancel. |
| `on_files_picked(tag, paths)` | Dialog tag, paths joined with `\|`. |
| `on_folder_picked(tag, path)` | Dialog tag, chosen folder. |
| `on_hotkey(name)` | Registered hotkey name. |
| `on_hotkey_release(name)` | Registered hotkey name, on release of the chord. |
| `on_notification_action(id, action_id)` | Notification id, pressed button's action id. |
| `on_clipboard(tag, text)` | Read tag, clipboard text. Empty when the clipboard holds no text. |
| `on_menu(id)` | Menu item id. |
| `on_tray(id)` | Tray icon id. |
| `on_dialog_accepted(id)` | Dialog id. |
| `on_dialog_rejected(id)` | Dialog id. |
| `on_timer(name)` | Timer name. |
| `on_fetch(tag, body)` | Request tag, response body (2xx only). |
| `on_fetch_error(tag, message)` | Request tag, error text (transport failure or non-2xx). |

### Per-element routing

```rust
lumen::on(event: string, id: string, handler: string)
```

Routes one `(event, id)` pair to the script function named `handler`, bypassing
the global handler for that pair only. `event` is the name without the `on_`
prefix (`"click"`, `"toggle"`, `"timer"`, `"file_picked"`, ...). A handler
registered for `save` also matches template-instance ids ending in `:save`.

```rust
lumen::local_id(source: string, suffix: string) -> string
```

Returns the id of the sibling `suffix` inside the same template instance as
`source`: `local_id("user-card:save", "status")` is `"user-card:status"`. A
source with no instance prefix returns `suffix` unchanged. Use it inside a
handler to reach the other elements of the instance that fired it.

## Signals

Signals are the named reactive cells markup binds to with `bind-text` and
friends.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::signal_get(name: string)` | `string` | Read as a string. Empty when never written. |
| `lumen::signal_set(name: string, value: string)` | | Write a string. |
| `lumen::signal_get_int(name: string)` | `int` | Read as an integer. `0` on miss or a non-numeric value. |
| `lumen::signal_set_int(name: string, value: int)` | | Write an integer. |
| `lumen::signal_get_float(name: string)` | `float` | Read as a float. `0.0` on miss or a non-numeric value. |
| `lumen::signal_set_float(name: string, value: float)` | | Write a float. |
| `lumen::signal_get_bool(name: string)` | `bool` | Read as a boolean. `false` on miss or an unparseable value. |
| `lumen::signal_set_bool(name: string, value: bool)` | | Write a boolean. |
| `lumen::signal_set_color(name: string, hex: string)` | | Write a `#rrggbb` or `#rrggbbaa` color. The six-digit form is opaque. Unparseable input is ignored. |
| `lumen::signal_get_color(name: string)` | `{string: int}` | Read a color as an `{ r, g, b, a }` map of 0-255 channels. Empty when the signal holds no color. |
| `lumen::signals_all()` | `{string: string}` | The whole signal set as a name-to-value map. |

A getter converts across the scalar types: an integer cell read through
`signal_get_float` yields the same number as a float, and a string cell parses.

A color signal is a typed cell, not a string: CSS reads it as a color, so
`signal_set_color("accent", "#ff8800")` recolors everything bound to `accent`.

`signal(name)` wraps the name so the same calls read as methods:

```rust
import "lumen.cdl";

fn on_start() {
    let clicks = signal("clicks");
    clicks.set_int(0);
}

fn bump(ev) {
    let clicks = signal("clicks");
    clicks.set_int(clicks.get_int() + 1);
}

fn main() {}
```

Methods: `get`, `set`, `get_int`, `set_int`, `get_float`, `set_float`,
`get_bool`, `set_bool`, `get_color`, `set_color`. The handle holds only the
name and calls the builtins above, so it reaches the same cells they do. There
is no default value: seed a cell by writing it once.

### Array signals

An array signal is the reactive list `<for each="name">` renders, one element
per record. A record is a string-keyed map whose fields `<for>` binds by name;
an item that is not a map is carried as a one-field `value` record.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::signal_array_set(name: string, items: any)` | | Replace the whole array with a list of records. |
| `lumen::signal_array_push(name: string, item: any)` | | Append one record. |
| `lumen::signal_array_get(name: string, index: int)` | `any` | One record by zero-based index; null when out of range. |
| `lumen::signal_array_all(name: string)` | `any` | Every record, as a list. |
| `lumen::signal_array_len(name: string)` | `int` | Record count. |
| `lumen::signal_array_remove(name: string, index: int)` | | Drop the record at `index`. An out-of-range index does nothing. |
| `lumen::signal_array_clear(name: string)` | | Empty the array. |

`signal_array(name)` wraps the name so the same calls read as methods:

```rust
import "lumen.cdl";

fn on_start() {
    let rows = signal_array("rows");
    let first = {"id": "a", "title": "Alpha"};
    rows.push(first);
    let back = as_map(rows.get(0));
}

fn main() {}
```

Methods: `set`, `push`, `get`, `all`, `len`, `remove`, `clear`.

Field values are stringified on the way into the rendered row, so a record built
from a map literal carries strings. A record from `parse_json`, a response body,
or the host side keeps every field's own type when you read it back with
`signal_array_get`.

### Validation state

```rust
lumen::is_valid(id: string) -> bool
```

Whether the element with that `id` currently passes validation. The runtime
writes the backing `valid:<id>` signal each tick from the element's validation
rules; an element that carries none reads as valid.

### Derived signals

```rust
lumen::derive(name: string, deps: string[], f: string)
```

Registers a computed signal `name`, recomputed by the script function named `f`
whenever any signal in `deps` changes. `f` receives the dependency values in
`deps` order and returns the new value. candela has no closure value, so the
recompute body is referenced by function name.

Declare each parameter as the type the cell holds, or as `any`. A cell written
through `signal_set_int` arrives as an `int`, `signal_set_float` as a `float`,
`signal_set_bool` as a `bool`, and `signal_set` as a `string`; the conversions
a getter performs do not apply here, so `fn f(n: float)` on an `int` cell is a
type error. `any` takes whatever the cell holds and is the safe choice when a
signal's type is not fixed:

```rust
fn calc_label(n: any) {
    return "clicks: " + str(n);
}
```

A derivation runs once after registration, then on every change to a
dependency. Derived-of-derived chains settle within the same tick. A derivation
that errors is retried on the next tick, so a parameter type that never matches
fails and logs on every tick for the life of the app.

## Timers

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_timeout(name: string, ms: int)` | One-shot timer; fires `on_timer(name)` after `ms` milliseconds. |
| `lumen::set_interval(name: string, ms: int)` | Repeating timer; fires `on_timer(name)` every `ms` milliseconds. |
| `lumen::cancel_timer(name: string)` | Cancel the named timer. |

Timer names are unique: setting a timer with an existing name replaces it.
Negative delays clamp to zero. Cancelling from inside the timer's own handler
takes effect before the next fire.

## Query and traverse the element tree

Node handles are `int` ids; `0` means "no node". Reads resolve against the
snapshot rebuilt each tick, so a handle to a removed element reads as invalid.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_query(selector: string)` | `int[]` | Every element matching a CSS selector, document order. |
| `lumen::node_get_by_id(id: string)` | `int` | Element with that `id`, or `0`. |
| `lumen::node_document()` | `int` | The document root. |
| `lumen::node_parent(node: int)` | `int` | Parent, or `0`. |
| `lumen::node_first_child(node: int)` | `int` | First child, or `0`. |
| `lumen::node_last_child(node: int)` | `int` | Last child, or `0`. |
| `lumen::node_next(node: int)` | `int` | Next sibling, or `0`. |
| `lumen::node_prev(node: int)` | `int` | Previous sibling, or `0`. |
| `lumen::node_children(node: int)` | `int[]` | Children in document order. |
| `lumen::node_closest(node: int, selector: string)` | `int` | Nearest ancestor-or-self matching the selector, or `0`. |
| `lumen::node_valid(node: int)` | `bool` | Whether the handle is in the current snapshot. |

## Mutate the element tree

Mutations queue a command applied later in the same tick.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_spawn(tag: string)` | `int` | Create a detached element. The handle is valid for the rest of the tick; attach it before the tick ends. |
| `lumen::node_clone_deep(source: int)` | `int` | Deep-clone a subtree into a fresh detached element. |
| `lumen::node_set_attr(node: int, name: string, value: string)` | | Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map. |
| `lumen::node_remove_attr(node: int, name: string)` | | Remove an attribute. |
| `lumen::node_set_id(node: int, id: string)` | | Set the `id` attribute. |
| `lumen::node_set_text(node: int, text: string)` | | Replace the text content. |
| `lumen::node_set_inner_markup(node: int, markup: string)` | | Replace the children with a parsed markup fragment. Do not feed untrusted content. A no-op when the app runs from a precompiled artifact, which links no parser. |
| `lumen::node_class_add(node: int, class: string)` | | Add one class. |
| `lumen::node_class_remove(node: int, class: string)` | | Remove one class. |
| `lumen::node_class_toggle(node: int, class: string)` | | Toggle one class. |
| `lumen::node_set_class(node: int, classes: string)` | | Replace the whole class list. |
| `lumen::node_set_style(node: int, name: string, value: string)` | | Set one inline style property. |
| `lumen::node_style_remove(node: int, name: string)` | | Remove one inline style property. |
| `lumen::node_append(parent: int, child: int)` | | Append `child` under `parent`. |
| `lumen::node_insert_before(parent: int, child: int, reference: int)` | | Insert `child` before `reference` under `parent`. A `reference` of `0` appends. |
| `lumen::node_set_parent(node: int, parent: int)` | | Reparent `node` under `parent`. |
| `lumen::node_move_to(node: int, parent: int)` | | Same as `node_set_parent`. |
| `lumen::node_replace_with(old: int, new: int)` | | Replace `old` with `new`, despawning `old`'s subtree. |
| `lumen::node_remove(node: int)` | | Detach and despawn the element and its subtree. |

## Markup blocks

An `lmn!( ... )` block is markup a candela function returns. It compiles to a
fragment when the app is built, and the call instantiates that fragment, so a
running app parses no markup. For what goes inside a block and how components
compose, see [composition](../guides/composition.md#components).

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::fragment_spawn(key: string, args: string[], children: int[])` | `int` | Instantiate the compiled fragment `key` into a detached node. `args` is flattened name/value pairs; `children` are the nodes its slots take, in slot order. This is what an `lmn!` block expands to; call it directly only against a key you know. |
| `lumen::mount(node: int)` | | Put `node` at the app root. |

The block itself is the surface to write against:

```rust
fn Home(name) {
    return lmn!(<label class="home" text="home for $name"/>);
}

fn on_ready() {
    lumen::mount(Home("bob"));
}

fn main() {}
```

A block obeys three rules the compiler enforces:

- **One root element.** A call returns one node.
- **Arguments are static.** `$name` substitutes once, when the instance is
  built. Something that changes while the app runs is a `bind-*` attribute
  inside the block.
- **A handle is valid for the tick it was minted in.** Attach what a component
  returns before the tick ends, the same as a `node_spawn` handle.

A component element inside a block is a use site, not a call: the build
resolves it against the component it names. Where that component returns its
block and nothing else, and reads only its own parameters, the block is put in
the tree at build time. Where it works a value out or picks between blocks, the
build leaves a marker and the runtime fills it by calling the function on the
first tick, before the tree is drawn. A `.lmn` file writes the same element and means the same thing; see
[composition](../guides/composition.md#when-the-subtree-appears).

`lumenc check` and `lumenc build` both read every block in the app's scripts, so
a malformed one fails ahead of the run.

## Read element state

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_get_attr(node: int, name: string)` | `string` | One attribute value; empty when absent. |
| `lumen::node_text(node: int)` | `string` | Text content. |
| `lumen::node_id(node: int)` | `string` | The `id` attribute. |
| `lumen::node_class_contains(node: int, class: string)` | `bool` | Whether the class list contains `class`. |
| `lumen::node_style_get(node: int, prop: string)` | `string` | One inline style override. |
| `lumen::node_computed_style(node: int, prop: string)` | `string` | One resolved style property after the cascade. |
| `lumen::node_computed_style_all(node: int)` | `{string: string}` | Every resolved style property. Spelled apart from `node_computed_style` because a candela host function takes one arity per name. |
| `lumen::node_inline_style(node: int)` | `{string: string}` | Every inline style override. |
| `lumen::node_attrs(node: int)` | `{string: string}` | Every attribute. |
| `lumen::node_classes(node: int)` | `string[]` | The class list. |

## Introspection

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::node_rect(node: int)` | `{string: float}` | Post-layout border box: `x`, `y`, `width`, `height`, `client_x`, `client_y`. Local `x` / `y` are relative to the parent; `client_*` are window coordinates. |
| `lumen::node_content_rect(node: int)` | `{string: float}` | Same keys, for the content box (padding and border removed). |
| `lumen::node_scroll(node: int)` | `{string: float}` | `x`, `y`, `max_x`, `max_y`. |
| `lumen::node_is_visible(node: int)` | `bool` | Effective visibility. |
| `lumen::node_z_index(node: int)` | `int` | Resolved stacking order. |
| `lumen::node_entity_id(node: int)` | `{string: int}` | `index` and `generation` of the backing entity. |
| `lumen::node_components(node: int)` | `string[]` | Names of the introspectable components on the element. |
| `lumen::node_component(node: int, name: string)` | `{string: string}` | Field map of one component. Empty for an absent or non-introspectable name. |
| `lumen::node_outer_markup(node: int)` | `string` | The subtree serialized to markup text. |
| `lumen::node_inner_markup(node: int)` | `string` | The children serialized to markup text. |
| `lumen::matched_rules(node: int)` | `any` | The stylesheet rules that matched the node, ascending in cascade order (last wins). Each record is `{ selector, specificity, source, source_order, declarations }`, where `specificity` is a three-int list and `declarations` a property-to-value map. |
| `lumen::dump_tree()` | `string` | Whole-tree structural dump. |
| `lumen::pointer_state()` | `{string: string}` | `x`, `y`, `inside`, `buttons`, `shift`, `ctrl`, `alt`, `super`. Values are stringified. |
| `lumen::frame_info()` | `{string: float}` | `frame`, `dt_ms`, `dirty_count`. |

The introspectable components are `LayoutBox`, `Visuals`, `Opacity`, `ZIndex`,
`Visible`, `TextContent`, `LumenClasses`, `LumenAttributes`, `InlineStyle`, and
`Style`.

## Element events

Bind a handler to one element and one event type. Binding returns a token; pass
it to `event_off` to unbind.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::event_on(node: int, event_type: string, handler: string)` | `int` | Bind `handler` (a function name) for the bubble phase. Returns the off token, or `0` for an unknown node. |
| `lumen::event_on_capture(node: int, event_type: string, handler: string)` | `int` | Same, for the capture phase. |
| `lumen::event_off(token: int)` | | Unbind. |

The handler receives the event id as its only argument and reads the event
through the accessors below:

| Accessor | Returns | Value |
| --- | --- | --- |
| `lumen::event_target(ev: int)` | `int` | The element the event originated on. |
| `lumen::event_current_target(ev: int)` | `int` | The element whose handler is running. |
| `lumen::event_type(ev: int)` | `string` | Event type name. |
| `lumen::event_key(ev: int)` | `string` | Key name for keyboard events. |
| `lumen::event_value(ev: int)` | `string` | Text value for `input` / `change` / `submit`. |
| `lumen::event_button(ev: int)` | `int` | `0` primary, `1` middle, `2` secondary. |
| `lumen::event_x(ev: int)` / `lumen::event_y(ev: int)` | `float` | Pointer position relative to the target. |
| `lumen::event_client_x(ev: int)` / `lumen::event_client_y(ev: int)` | `float` | Pointer position in window coordinates. |
| `lumen::event_delta_x(ev: int)` / `lumen::event_delta_y(ev: int)` | `float` | Wheel delta. |
| `lumen::event_shift(ev: int)` / `_ctrl` / `_alt` / `_super` | `bool` | Modifier state. |
| `lumen::event_prevent_default(ev: int)` | | Cancel the default action. |
| `lumen::event_stop_propagation(ev: int)` | | Stop the event reaching further elements. |
| `lumen::event_stop_immediate_propagation(ev: int)` | | Stop the event entirely, including other handlers on this element. |

### Event types

`click`, `dblclick`, `pointerdown`, `pointerup`, `pointermove`, `pointerenter`,
`pointerleave`, `wheel`, `keydown`, `keyup`, `input`, `change`, `focus`, `blur`,
`submit`, `scroll`.

Dispatch runs capture (root down to the target), then the target, then bubble
(target up to the root). `focus`, `blur`, `pointerenter`, `pointerleave`, and
`scroll` do not bubble.

`input` fires per keystroke: every edit that changes the text raises one,
carrying the buffer as it stands after that edit. A caret move raises nothing.
`change` and `submit` come from the commit signal instead, so they fire when the
field is committed with Enter. Only `click` has a default action (link
navigation); `prevent_default` on a click skips it.

## Method sugar

The prelude wraps a raw handle in a `Node` or `Event` struct so calls read as
methods. The free functions above keep working on raw handles.

```rust
import "lumen.cdl";

fn on_ready() {
    let list = get_by_id("list");
    let row = create("row");
    row.add_class("item");
    row.set_text("hello");
    list.append(row);
    row.on("click", "handle_row");
}

fn handle_row(id) {
    let ev = event(id);
    ev.prevent_default();
}

fn main() {}
```

Constructors: `node(handle)`, `event(handle)`, `wrap_nodes(handles)`,
`create(tag)`, `get_by_id(id)`, `document_node()`, `query(selector)`.

`Node` methods mirror the `node_*` builtins with the prefix dropped, except
for the class calls and the whole-map style read, which carry the DOM-style
names every host uses: `exists`, `valid`, `parent`, `first_child`,
`last_child`, `next`, `prev`, `children`, `closest`, `clone_deep`, `set_attr`,
`remove_attr`, `set_id`, `set_text`, `set_inner_markup`, `add_class`,
`remove_class`, `toggle_class`, `set_class`, `set_style`, `style_remove`,
`remove`, `append`, `insert_before`, `set_parent`, `move_to`, `replace_with`,
`get_attr`, `text`, `id`, `has_class`, `style_get`, `computed_style`,
`computed_style_all`, `is_visible`, `z_index`, `classes`, `components`,
`outer_markup`, `inner_markup`, `on`, `on_capture`. `exists()` tests the
handle against `0`; `valid()` tests it against the current snapshot.

`ArraySignal` methods mirror the `signal_array_*` builtins with the prefix
dropped: `set`, `push`, `get`, `all`, `len`, `remove`, `clear`. Construct one
with `signal_array(name)`.

`Signal` methods mirror the `signal_*` builtins the same way: `get`, `set`,
`get_int`, `set_int`, `get_float`, `set_float`, `get_bool`, `set_bool`,
`get_color`, `set_color`. Construct one with `signal(name)`.

`Event` methods: `target`, `current_target`, `event_type`, `key`, `value`,
`button`, `x`, `y`, `client_x`, `client_y`, `delta_x`, `delta_y`, `shift`,
`ctrl`, `alt`, `super_key`, `prevent_default`, `stop_propagation`,
`stop_immediate_propagation`, `off`.

One more helper builds a list row in a single call:

```rust
lm_append(parent, tag, cls, text) -> int
```

It creates a `tag` element, applies `cls` and `text` when they are non-empty,
appends the result under `parent`, and returns the new handle.

## window, document, history

Each is its own host namespace, declared by the same prelude import.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `window::set_href(path: string)` | | Navigate to a page path. |
| `window::href()` | `string` | The current page path. |
| `window::reload()` | | Re-navigate to the current page. |
| `window::title()` | `string` | Window title. |
| `window::set_title(title: string)` | | Set the window title. |
| `window::dpr()` | `float` | Device pixel ratio. |
| `window::size()` | `float[]` | `[width, height]` in logical pixels. |
| `window::set_size(width: float, height: float)` | | Resize the window, in logical pixels. |
| `window::location_path()` | `string` | The current page path. |
| `window::location_query()` | `string` | The query string of the request the document is being rendered for, without the leading `?`. |
| `window::location_hash()` | `string` | The fragment of the request the document is being rendered for, without the leading `#`. |
| `history::back()` | | Step one entry back. |
| `history::forward()` | | Step one entry forward. |
| `history::go(delta: int)` | | Step `delta` entries; negative goes back. |
| `document::root()` | `int` | The document root. |
| `document::query(selector: string)` | `int[]` | Matching elements, document order. |
| `document::get_by_id(id: string)` | `int` | Element with that `id`, or `0`. |
| `document::focused()` | `int` | The focused element, or `0`. |
| `document::hovered()` | `int` | The hovered element, or `0`. |
| `document::create(tag: string)` | `int` | Create a detached element. |

## Page navigation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::page(path: string)` | | Navigate to a page path (`"settings"`, `"/user/7"`, `"/"`). |
| `lumen::page_current()` | `string` | The active page key. Spelled apart from `page` because a candela host function takes one arity per name. |
| `lumen::page_back()` | | Step one entry back in the page history. |
| `lumen::page_forward()` | | Step one entry forward. |

See [Pages](../guides/pages.md) for the file layout these paths resolve against.

## Dialogs

Each opens a native dialog and delivers the result to the matching handler,
keyed by `tag`. A cancelled dialog still fires once, with an empty path.

| Builtin | Behaviour |
| --- | --- |
| `lumen::pick_file(tag: string)` | Open-file dialog; fires `on_file_picked(tag, path)`. |
| `lumen::pick_files(tag: string)` | Multi-select dialog; fires `on_files_picked(tag, paths)` with paths joined by `\|`. |
| `lumen::pick_folder(tag: string)` | Folder picker; fires `on_folder_picked(tag, path)`. |
| `lumen::save_file(tag: string, default_name: string)` | Save dialog seeded with `default_name`; fires `on_file_picked(tag, path)`. |
| `lumen::pick_file_filtered(tag: string, spec: string)` | Open-file dialog with extension filters. `spec` is pipe-separated `Label:ext1,ext2` groups; a `*` extension means no filter. |

## OS integration

| Builtin | Behaviour |
| --- | --- |
| `lumen::notify(title: string, body: string)` | Show an OS notification. |
| `lumen::notify_ex(id: string, title: string, body: string, options: string, actions: string)` | Show an OS notification. `options` is pipe-separated `key:value` entries, where `icon` takes a themed name or path and `urgency` takes `"low"`, `"normal"`, or `"critical"`. `actions` is pipe-separated `id:Label` buttons; a press fires `on_notification_action(id, action_id)`. An empty string in either position means the defaults. |
| `lumen::clipboard_write(text: string)` | Put `text` on the system clipboard. |
| `lumen::clipboard_read(tag: string)` | Request the clipboard text; fires `on_clipboard(tag, text)` on the next tick. |
| `lumen::copy_image(path: string)` | Copy the image at `path` to the system clipboard. |
| `lumen::save_clipboard_image(path: string)` | Write the clipboard image to `path` as PNG. |
| `lumen::tray_icon(id: string, icon_path: string, tooltip: string)` | Register or replace a tray icon; clicks fire `on_tray(id)`. An empty tooltip disables it. |
| `lumen::tray_icon_menu(id: string, icon_path: string, tooltip: string, menu: string, template: bool)` | Register a tray icon with a context menu, given as pipe-separated `id:Label` entries where `-` is a separator; a pick fires `on_menu(id)`. `template` is the macOS monochrome-icon flag, ignored elsewhere. |
| `lumen::unregister_tray(id: string)` | Remove a tray icon. |
| `lumen::register_hotkey(name: string, accelerator: string)` | Register a global hotkey (`"CommandOrControl+S"`, `"Alt+Space"`, `"F11"`); fires `on_hotkey(name)`. |
| `lumen::unregister_hotkey(name: string)` | Remove a global hotkey. |
| `lumen::open_url(url: string)` | Open `url` with the default browser, or the mail client for `mailto:`. |
| `lumen::open_path(path: string)` | Open `path` with the platform's default application. Relative paths resolve against the app directory. |
| `lumen::reveal_path(path: string)` | Show `path` in the platform's file manager. |
| `lumen::keep_awake(name: string, reason: string)` | Hold off the screensaver and system sleep under `name`. Repeating a live name replaces its request. |
| `lumen::allow_sleep(name: string)` | Release the inhibit registered under `name`. |
| `lumen::open_menu(id: string)` | Open menu `id` by setting the `__menu_open:id` signal to true. |
| `lumen::close_menu(id: string)` | Close menu `id`. |

See [OS integration](../guides/os-integration.md) for the markup these pair with.

## Styling and theming

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_class(id: string, classes: string)` | Replace the class list on the element with that `id`. |
| `lumen::set_root_class(classes: string)` | Replace the class list on the root element, which drives theme-token selectors. |
| `lumen::set_color_scheme(name: string)` | Switch the color scheme: `"default"` (follow the OS), `"force-light"`, `"force-dark"`, `"prefer-light"`, `"prefer-dark"`. An unknown name is ignored with a warning. |

`lumen::set_class` takes an element id and `Node.set_class` takes none because
it already has the element. They share a name and do the same thing through
different routes: reach for the free function when all you have is an id, and
the method when you are holding a handle.

## Audio

These functions come from the `lumen-audio` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`; see
[OS integration](../guides/os-integration.md#audio).

| Builtin | Behaviour |
| --- | --- |
| `lumen::audio_play(path: string)` | Load and play the track at `path` (app-relative wav or ogg, resolved through the app's asset sources, so a packed archive and `lumen://app/...` URIs work); resets position to zero. |
| `lumen::audio_pause()` | Pause, holding position. |
| `lumen::audio_resume()` | Resume a paused transport. |
| `lumen::audio_stop()` | Stop and rewind. |
| `lumen::audio_seek(secs: float)` | Seek to `secs`, clamped to the track duration. |
| `lumen::audio_volume(level: float)` | Set output volume in `0.0` to `1.0`. |

The module writes the `audio_position`, `audio_duration`, and `audio_playing`
signals each tick, so markup binds to them directly.

## Networking

```rust
lumen::fetch(url: string, tag: string)
```

Issues an HTTP GET without holding up a tick. A 2xx reply fires `on_fetch(tag, body)`;
a transport failure or non-2xx fires `on_fetch_error(tag, message)`. The reply
is delivered on the tick thread, so a handler may touch signals and the element
tree freely.

```rust
lumen::http(request: any)
```

Issues any HTTP request. The request is a map; only `url` and `tag` are
required. `method` defaults to `GET`, and `body` and `timeout_ms` are optional.
Each header rides on a `header:<Name>` key, because a candela map literal holds
one value type:

```rust
lumen::http({
    "method": "POST",
    "url": "https://example.test/items",
    "header:Content-Type": "application/json",
    "body": "{\"title\":\"First\"}",
    "timeout_ms": "2500",
    "tag": "create"
});
```

A request built from `parse_json` or handed in from the host may instead carry a
nested `headers` map and an integer `timeout_ms`; both forms are accepted.

Every completed request fires `on_http(tag, response)`, including a 4xx or 5xx.
The response is a map:

| Field | Type | Value |
| --- | --- | --- |
| `ok` | `bool` | True for a 2xx status. |
| `status` | `int` | HTTP status, `0` on a transport failure. |
| `headers` | map | Response headers, names lowercased. |
| `body` | `string` | Response body. |
| `error` | `string` | Transport error text; empty when the request completed. |

```rust
fn on_http(tag, response) {
    let r = as_map(response);
    if as_bool(r.get("ok")) {
        lumen::signal_set("status", as_str(r.get("body")));
    }
}
```

## Request and response

The readers give back what arrived with the request the document is being
rendered for, and an empty string when there is none to read; a desktop app has
none. The three writers queue an answer that only a server render applies.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::request_header(name: string)` | `string` | The named request header, matched without regard to case. |
| `lumen::request_cookie(name: string)` | `string` | The named request cookie. |
| `lumen::request_body()` | `string` | The request body. |
| `lumen::response_status(status: int)` | | Answer with HTTP status `status`, clamped to 100..=599. |
| `lumen::response_header(name: string, value: string)` | | Set a response header; setting the same name twice replaces the value. |
| `lumen::redirect(location: string)` | | Answer with a redirect to `location`, a path or an absolute URL, instead of a document. |

## Parsing

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::parse_json(json: string)` | `any` | Parse JSON into a map, list, or scalar. Null on a parse error. |
| `lumen::parse_markdown(src: string)` | `any` | Parse markdown into a list of block records. |

`parse_json` returns candela values, not text, so numbers stay numbers and
nesting survives:

```rust
let root = as_map(lumen::parse_json(body));
let geo = as_map(root.get("geo"));
let city = as_str(geo.get("city_name"));
```

candela's own `json_parse` builtin parses JSON too. Prefer `lumen::parse_json`:
it interns the keys it produces, so a key longer than six characters is still
reachable with `map.get(...)`.

A `parse_markdown` block record carries `id`, `kind`, `level`, `text`, and
`lang`. `kind` is `h`, `p`, `code`, `li`, or `hr`; `level` is the heading depth
and `0` elsewhere; `lang` is the code fence's language and empty elsewhere.
Feed the list straight to an array signal to render a document with `<for>`.
Inline emphasis keeps its markdown delimiters in `text`, since a label renders
plain text.

## Translation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::t(key: string)` | `string` | The active locale's string for `key`, or `key` itself when untranslated. |
| `lumen::tr(key: string)` | `string` | Alias for `t`. |

See [Translation](../guides/i18n.md) for the catalogue format.

## Filesystem

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `lumen::read_file(path: string)` | `string` | File contents; empty string on error. Relative paths resolve against the app directory. |
| `lumen::write_file(path: string, contents: string)` | `bool` | `true` on success. Relative paths resolve against the app directory; the write is atomic (temp file + rename), so a reader never sees a truncated file. |
| `lumen::data_dir()` | `string` | The directory this app saves data in, created when missing. |

A relative path names a file the app ships, so it reads the same wherever the
app was started from. Saved state goes under `data_dir()` instead, because the
app directory is read-only once the app is installed:

```rust
lumen::write_file(lumen::data_dir() + "/session.json", state);
```

`data_dir()` follows the platform convention for user data (`$XDG_DATA_HOME`,
else `~/.local/share`, on Linux; `~/Library/Application Support` on macOS;
`%APPDATA%` on Windows) and names one directory per app from
[`[app] id`](lumen-toml.md), so two apps on a machine keep their saves apart.

## Diagnostics

```rust
lumen::print(args: any)
```

Stringifies its arguments, joins them with a space, and emits the line through
the script command stream, where `lumenc run` prints it prefixed with
`[script]`. candela's own `print` writes to process stdout instead and bypasses
that stream.

## Embedder commands

Two builtins emit a command with no built-in effect under `lumenc run`. They
exist for embedders that read the script command stream directly.

| Builtin | Behaviour |
| --- | --- |
| `lumen::add_clicks(n: int)` | Emit an add-clicks command carrying `n`. |
| `lumen::set_string(key: string, value: string)` | Emit a set-string command carrying `key` and `value`. |

Two more write directly to the element tree by id, without a node handle:

| Builtin | Behaviour |
| --- | --- |
| `lumen::set_text(target_id: string, text: string)` | Replace the text content of the element with that `id`. |
| `lumen::set_src(target_id: string, path: string)` | Swap the asset path of an `<image>` at run time. Paths are app-relative. |

## Native functions from the embedder

An embedder exposes its own functions to the app's script: over the C ABI with
`lumen_app_expose` (see [C ABI](ffi.md)), from Rust with the SDK, or from a Rust
plugin. Call them by namespace:

```rust
fn on_start() {
    let t = native::now_ms();
    print(as_int(t));
}
```

Functions the C ABI and the SDK expose land in the `native` namespace. A plugin
can choose a name of its own instead, and the script calls it there
(`gpio::level(21)`).

You do not declare them. candela resolves a host call through a
`host "<ns>" { .. }` block, and the host writes one for every namespace it bound
from the signatures it was given. A script that spells the block itself keeps
it: the host leaves a namespace the source already declares alone, so an app
written against an earlier release compiles unchanged.

A function described with parameter and return types is declared with them,
whatever those types are, and the result is typed as declared, so it composes
with operators the way a value of that type does. A call passing the wrong
types or the wrong number of arguments is refused when it runs, naming the
parameter.

A function with an untyped parameter, a variadic argument list, or an optional
trailing argument has no such spelling: it is declared `any name(...)`, takes
any arguments, and returns `any`. Read that result with the `as_int` /
`as_str` / `as_map` downcasts, the same way a `parse_json` result is read.

The name is the plugin's to choose, including one candela's own library uses: a
namespaced call takes its type from the block that declares it.

A native function may fail. It raises where it was called, under the kind
`host_fn_error` and naming the function, so a script handles one the way it
handles any other runtime error:

```rust
fn level(pin) {
    try {
        return gpio::read(pin);
    } catch "host_fn_error" {
        return -1;
    }
}
```

A plugin can also ship candela source of its own, compiled ahead of the app, to
offer method syntax over its functions. What that looks like is the plugin's
choice; `pin(21).level()` in place of `gpio::level(21)` is the shape to expect.

Two things follow from where the declarations come from. An app calling a
function nothing registered fails the compile, naming it, so `lumenc check` and
`lumenc run` reject a call meant for an embedder they do not carry. And a script
compiled ahead of time to a `.cdlb` gets no synthesized declarations, because the
build compiles the script alone: write the `host` block by hand in a script you
will build to an artifact.

Rhai and Lua receive the same functions as plain globals (`now_ms()`), or as a
module or table for a named namespace, because neither needs a declaration to
resolve a call.
