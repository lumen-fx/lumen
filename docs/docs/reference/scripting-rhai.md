# Rhai scripting reference

Every builtin the Rhai script host registers, with its signature, parameters,
and behaviour. For the task-oriented introduction see
[Scripting](../guides/scripting.md).

The same surface in the other hosts, under the same names:
[candela](scripting-candela.md), [lua](scripting-lua.md). One structural
difference shapes those names: a candela host function is keyed by name alone
and cannot be overloaded on arity, so a call with two forms gets two names on
every host: `page(path)` and `page_current()`, `computed_style(prop)` and
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
engine = "rhai"   # "candela" (default) | "rhai" | "lua"
```

## Reaching the builtins

Builtins are bare globals; there is no import or declaration step.

```rhai
fn on_start() {
    let count = signal("count", 0);
    count.set(1);
}
```

Custom types (`Signal`, `ArraySignal`, `Node`, `NodeQuery`, `Event`,
`SignalRef`) carry methods dispatched on the receiver. Three scope constants are
pre-bound: `signals`, `window`, `document`, and `history`.

One Rhai-specific spelling differs from the other hosts: a function-pointer
value is invoked through `.call()`, so an unbind token returned by `n.on(...)`
is used as `off.call()`.

Parse depth and call depth are raised well past the defaults so a large
generated script parses; the runtime safety limits stay off, because a Lumen
script is the app author's own code.

`print(...)` is captured into the script command stream instead of writing to
stdout.

## Lifecycle hooks

Define these as free functions. Each is optional; a missing hook is a no-op.

| Hook | Fires |
| --- | --- |
| `on_start()` | Once at app construction, before the first tick. No element is queryable yet: `get_by_id` returns `()`. |
| `on_ready()` | Once per mount, on the first tick after the element tree is published. Queries resolve here. Re-armed after a hot reload, so it runs again on the fresh tree. |
| `on_close()` | On an OS close request, before teardown. Return `false` to veto the close and keep the window open. |
| `on_audio_end()` | When the audio transport reaches the end of a track. |

Top-level statements run once when the script loads, against a scope that
persists for the app's lifetime, so a top-level `let` is state every later
handler can read and write. Across a hot reload the signal values, the `on(...)`
routing table, and live event bindings are preserved.

## Event handlers

Each event dispatches to a global handler function named below. Handlers are
optional.

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
| `on_fetch_error(tag, message)` | Request tag, error text. |
| `on_http(tag, response)` | Request tag, the response map described under [Networking](#networking). |

### Per-element routing

```rhai
on(event: string, id: string, handler: string)
```

Routes one `(event, id)` pair to the script function named `handler`, bypassing
the global handler for that pair only. `event` is the name without the `on_`
prefix (`"click"`, `"toggle"`, `"timer"`, `"file_picked"`, ...). A handler
registered for `save` also matches template-instance ids ending in `:save`.

## Signals

### Handles

```rhai
signal(name: string, default: any) -> Signal
signal_array(name: string) -> ArraySignal
```

`signal` returns a handle to the named scalar signal, seeding it with `default`
the first time that name is seen. A value pushed in before the script loaded
wins over the default. `signal_array` returns a handle to the named reactive
array that drives `<for each="name">`; the array is created on the first `set`
or `push`.

| Method | Returns | Behaviour |
| --- | --- | --- |
| `Signal.get()` | `any` | Current value. |
| `Signal.set(value)` | | Replace the value. |
| `Signal.value` | `any` | Property form of `get` / `set`. |
| `ArraySignal.set(array)` | | Replace all rows. |
| `ArraySignal.push(item)` | | Append one row. |
| `ArraySignal.len()` | `int` | Row count. |
| `ArraySignal.get(index)` | `any` | One row; zero-based. |
| `ArraySignal.all()` | `array` | Every row. |

Rows are maps; their fields become the values a `<for>` block binds to.

### Chained access

`signals` is a pre-bound root. Each `.name` or `[index]` step extends the
property path, and a terminal method commits:

```rhai
signals.count.set(5);
signals.user.name.set("Alice");
signals.users[0].name.set("Bo");
signals.bg.set_color("#ff8800");
let n = signals.count.get();
```

The runtime type of the argument picks the stored type: an integer literal
stores an integer, a float literal a float, a string a string. Hex colours need
the explicit `set_color` method.

### Typed accessors

These predate the chained form and remain available.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `signal_get_int(name)` | `int` | Read as an integer; `()` on miss. |
| `signal_set_int(name, value)` | | Write an integer. |
| `signal_get_float(name)` | `float` | Read as a float; `()` on miss. |
| `signal_set_float(name, value)` | | Write a float. |
| `signal_get_bool(name)` | `bool` | Read as a boolean; `()` on miss. |
| `signal_set_bool(name, value)` | | Write a boolean. |
| `signal_get_color(name)` | `map` | Read as `#{ r, g, b, a }` with integer channels; `()` on miss. |
| `signal_set_color(name, hex)` | | Write a `#rrggbb` or `#rrggbbaa` colour. An unparseable value is ignored. |
| `signals_all()` | `map` | The whole signal set as a name-to-value map. |
| `is_valid(id)` | `bool` | Whether the element with that `id` currently passes validation. `true` for an element with no validation state. |

### Derived signals

```rhai
derive(name: string, deps: array, f: fn) -> Signal
```

Registers a computed signal recomputed whenever any dependency changes. `deps`
holds `Signal` handles or signal-name strings; `f` receives the dependency
values in `deps` order and returns the new value. Returns a `Signal` handle to
the result:

```rhai
let a = signal("a", 1);
let b = signal("b", 2);
let sum = derive("sum", [a, b], |a, b| a + b);
```

A derivation runs once after registration, then on every change to a
dependency. Derived-of-derived chains settle within the same tick. A derivation
that errors is retried on the next tick.

## Timers

| Builtin | Behaviour |
| --- | --- |
| `set_timeout(name, ms)` | One-shot timer; fires `on_timer(name)` after `ms` milliseconds. |
| `set_interval(name, ms)` | Repeating timer; fires `on_timer(name)` every `ms` milliseconds. |
| `cancel_timer(name)` | Cancel the named timer. |

Timer names are unique: setting a timer with an existing name replaces it.
Negative delays clamp to zero. Cancelling from inside the timer's own handler
takes effect before the next fire.

## Query and traverse the element tree

`Node` is a cheap handle. Reads resolve against the snapshot rebuilt each tick,
so a handle to a removed element stops resolving.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `query(selector)` | `NodeQuery` | Every element matching a CSS selector, document order. A malformed selector raises. |
| `get_by_id(id)` | `Node` or `()` | Element with that `id`. |
| `document()` | `Node` or `()` | The document root. |

| `NodeQuery` method | Returns | Behaviour |
| --- | --- | --- |
| `len()` | `int` | Match count. |
| `is_empty()` | `bool` | Whether there are no matches. |
| `first()` | `Node` or `()` | First match. |
| `nth(i)` | `Node` or `()` | Match at zero-based `i`. |
| `iter()` | `array` | Every match. |
| `collect()` | `array` | Every match. |
| `single()` | `Node` | The one match; raises when the count is not exactly one. |
| `get_single()` | `Node` or `()` | The one match, or `()` for any other count. |

| `Node` method | Returns | Behaviour |
| --- | --- | --- |
| `parent()` | `Node` or `()` | Parent. |
| `first_child()` / `last_child()` | `Node` or `()` | Bounding child. |
| `next()` / `prev()` | `Node` or `()` | Sibling. |
| `children()` | `array` | Children in document order. |
| `closest(selector)` | `Node` or `()` | Nearest ancestor-or-self matching the selector. |
| `exists()` / `valid()` | `bool` | Whether the handle is in the current snapshot. |
| `handle()` | `int` | The raw packed handle. |

## Mutate the element tree

Every mutator returns the receiver, so calls chain; read-backs end the chain.
Mutations queue a command applied later in the same tick.

| Builtin or method | Returns | Behaviour |
| --- | --- | --- |
| `create(tag)` / `document.create(tag)` | `Node` | Create a detached element. The handle is valid for the rest of the tick; attach it before the tick ends. |
| `Node.clone_deep()` | `Node` | Deep-clone the subtree into a fresh detached element. |
| `Node.set_attr(name, value)` | `Node` | Set an attribute. `id`, `class`, `text`, and `disabled` route to their typed component; anything else lands in the attribute map. |
| `Node.remove_attr(name)` | `Node` | Remove an attribute. |
| `Node.set_id(id)` | `Node` | Set the `id` attribute. |
| `Node.set_text(text)` | `Node` | Replace the text content. |
| `Node.set_inner_markup(markup)` | `Node` | Replace the children with a parsed markup fragment. Do not feed untrusted content. A no-op when the app runs from a precompiled artifact, which links no parser. |
| `Node.add_class(class)` | `Node` | Add one class. |
| `Node.remove_class(class)` | `Node` | Remove one class. |
| `Node.toggle_class(class)` | `Node` | Toggle one class. |
| `Node.set_class(classes)` | `Node` | Replace the whole class list. |
| `Node.set_style(name, value)` / `Node.style_set(name, value)` | `Node` | Set one inline style property. |
| `Node.style_remove(name)` | `Node` | Remove one inline style property. |
| `Node.append(child)` | `Node` | Append `child` under the receiver. |
| `Node.insert_before(child, reference)` | `Node` | Insert `child` before `reference` under the receiver. |
| `Node.set_parent(parent)` / `Node.move_to(parent)` | `Node` | Reparent the receiver under `parent`. |
| `Node.replace_with(new)` | `Node` | Replace the receiver with `new`, despawning the receiver's subtree. Returns `new`. |
| `Node.remove()` | | Detach and despawn the receiver and its subtree. Terminal. |

## Read element state

| Method | Returns | Behaviour |
| --- | --- | --- |
| `Node.get_attr(name)` | `string` or `()` | One attribute value. |
| `Node.text()` | `string` or `()` | Text content. |
| `Node.id()` | `string` or `()` | The `id` attribute. |
| `Node.has_class(class)` | `bool` | Whether the class list contains `class`. |
| `Node.style_get(name)` | `string` or `()` | One inline style override. |
| `Node.computed_style(name)` | `string` or `()` | One resolved style property after the cascade. |
| `Node.computed_style()` / `Node.computed_style_all()` | `map` | Every resolved style property. |
| `Node.inline_style()` | `map` | Every inline style override. |
| `Node.attrs()` | `map` | Every attribute. |
| `Node.classes()` | `array` | The class list. |

## Introspection

| Builtin or method | Returns | Behaviour |
| --- | --- | --- |
| `Node.rect()` | `map` or `()` | Post-layout border box: `x`, `y`, `width`, `height`, `client_x`, `client_y`. Local `x` / `y` are relative to the parent; `client_*` are window coordinates. |
| `Node.content_rect()` | `map` or `()` | Same keys, for the content box (padding and border removed). |
| `Node.scroll()` | `map` or `()` | `x`, `y`, `max_x`, `max_y`. |
| `Node.is_visible()` | `bool` | Effective visibility. |
| `Node.z_index()` | `int` | Resolved stacking order. |
| `Node.matched_rules()` | `array` | Every CSS rule that matched, each a map of `selector`, `specificity` (a three-element array), `source`, `source_order`, and `declarations`. |
| `Node.entity_id()` | `map` or `()` | `index` and `generation` of the backing entity. |
| `Node.components()` | `array` | Names of the introspectable components on the element. |
| `Node.component(name)` | `map` or `()` | Field map of one component. Raises for a name outside the introspectable set. |
| `Node.outer_markup()` | `string` | The subtree serialized to markup text. |
| `Node.inner_markup()` | `string` | The children serialized to markup text. |
| `dump_tree()` | `string` | Whole-tree structural dump. |
| `pointer_state()` | `map` | `x`, `y`, `inside`, `buttons`, and a nested `modifiers` map of `shift`, `ctrl`, `alt`, `super`. |
| `frame_info()` | `map` | `frame`, `dt_ms`, `dirty_count`. |

The introspectable components are `LayoutBox`, `Visuals`, `Opacity`, `ZIndex`,
`Visible`, `TextContent`, `LumenClasses`, `LumenAttributes`, `InlineStyle`, and
`Style`.

## Element events

Bind a closure to one element and one event type. Binding returns a function
pointer that unbinds when called.

| Method | Returns | Behaviour |
| --- | --- | --- |
| `Node.on(event_type, handler)` | `fn` | Bind for the bubble phase. |
| `Node.on(event_type, handler, capture)` | `fn` | Bind for the capture phase when `capture` is `true`. |
| `Node.on_capture(event_type, handler)` | `fn` | Bind for the capture phase. |

```rhai
let off = get_by_id("save").on("click", |e| {
    e.prevent_default();
    print(e.event_type());
});
// later
off.call();
```

The handler receives an `Event`:

| `Event` method | Returns | Value |
| --- | --- | --- |
| `target()` | `Node` | The element the event originated on. |
| `current_target()` | `Node` | The element whose handler is running. |
| `event_type()` | `string` | Event type name. |
| `key()` | `string` | Key name for keyboard events. |
| `value()` | `string` | Text value for `input` / `change` / `submit`. |
| `button()` | `int` | `0` primary, `1` middle, `2` secondary. |
| `x()` / `y()` | `float` | Pointer position relative to the target. |
| `client_x()` / `client_y()` | `float` | Pointer position in window coordinates. |
| `delta_x()` / `delta_y()` | `float` | Wheel delta. |
| `position()` | `map` | `x`, `y`, `client_x`, `client_y`. |
| `modifiers()` | `map` | `shift`, `ctrl`, `alt`, `super`. |
| `prevent_default()` | | Cancel the default action. |
| `stop_propagation()` | | Stop the event reaching further elements. |
| `stop_immediate_propagation()` | | Stop the event entirely, including other handlers on this element. |

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

## window, document, history

Pre-bound scope constants.

| Call | Returns | Behaviour |
| --- | --- | --- |
| `window.set_href(path)` | | Navigate to a page path. |
| `window.href()` | `string` | The current page path. |
| `window.reload()` | | Re-navigate to the current page. |
| `window.title()` | `string` | Window title. |
| `window.set_title(title)` | | Set the window title. |
| `window.dpr()` | `float` | Device pixel ratio. |
| `window.size()` | `array` | `[width, height]` in logical pixels. |
| `window.set_size(width, height)` | | Resize the window, in logical pixels. |
| `window.location.path()` | `string` | The current page path. |
| `window.location.query()` | `string` | The query string of the request the document is being rendered for, without the leading `?`. |
| `window.location.hash()` | `string` | The fragment of the request the document is being rendered for, without the leading `#`. |
| `history.back()` | | Step one entry back. |
| `history.forward()` | | Step one entry forward. |
| `history.go(delta)` | | Step `delta` entries; negative goes back. |
| `document.root()` | `Node` or `()` | The document root. |
| `document.query(selector)` | `NodeQuery` | Matching elements, document order. |
| `document.get_by_id(id)` | `Node` or `()` | Element with that `id`. |
| `document.focused()` | `Node` or `()` | The focused element. |
| `document.hovered()` | `Node` or `()` | The hovered element. |
| `document.create(tag)` | `Node` | Create a detached element. |

## Page navigation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `page(path)` | | Navigate to a page path (`"settings"`, `"/user/7"`, `"/"`). |
| `page()` / `page_current()` | `string` | The active page key. |
| `page_back()` | `bool` | Step one entry back in the page history. |
| `page_forward()` | `bool` | Step one entry forward. |

See [Pages](../guides/pages.md) for the file layout these paths resolve against.

## Dialogs

Each opens a native dialog and delivers the result to the matching handler,
keyed by `tag`. A cancelled dialog still fires once, with an empty path.

| Builtin | Behaviour |
| --- | --- |
| `pick_file(tag)` | Open-file dialog; fires `on_file_picked(tag, path)`. |
| `pick_files(tag)` | Multi-select dialog; fires `on_files_picked(tag, paths)` with paths joined by `\|`. |
| `pick_folder(tag)` | Folder picker; fires `on_folder_picked(tag, path)`. |
| `save_file(tag, default_name)` | Save dialog seeded with `default_name`; fires `on_file_picked(tag, path)`. |
| `pick_file_filtered(tag, spec)` | Open-file dialog with extension filters. `spec` is pipe-separated `Label:ext1,ext2` groups; a `*` extension means no filter. |

## OS integration

| Builtin | Behaviour |
| --- | --- |
| `notify(title, body)` | Show an OS notification. |
| `notify_ex(id, title, body, options, actions)` | Show an OS notification. `options` is pipe-separated `key:value` entries, where `icon` takes a themed name or path and `urgency` takes `"low"`, `"normal"`, or `"critical"`. `actions` is pipe-separated `id:Label` buttons; a press fires `on_notification_action(id, action_id)`. An empty string in either position means the defaults. |
| `clipboard_write(text)` | Put `text` on the system clipboard. |
| `clipboard_read(tag)` | Request the clipboard text; fires `on_clipboard(tag, text)` on the next tick. |
| `copy_image(path)` | Copy the image at `path` to the system clipboard. |
| `save_clipboard_image(path)` | Write the clipboard image to `path` as PNG. |
| `tray_icon(id, icon_path, tooltip)` | Register or replace a tray icon; clicks fire `on_tray(id)`. An empty tooltip disables it. |
| `tray_icon_menu(id, icon_path, tooltip, menu, template)` | Register a tray icon with a context menu, given as pipe-separated `id:Label` entries where `-` is a separator; a pick fires `on_menu(id)`. `template` is the macOS monochrome-icon flag, ignored elsewhere. |
| `unregister_tray(id)` | Remove a tray icon. |
| `register_hotkey(name, accelerator)` | Register a global hotkey (`"CommandOrControl+S"`, `"Alt+Space"`, `"F11"`); fires `on_hotkey(name)`. |
| `unregister_hotkey(name)` | Remove a global hotkey. |
| `open_url(url)` | Open `url` with the default browser, or the mail client for `mailto:`. |
| `open_path(path)` | Open `path` with the platform's default application. Relative paths resolve against the app directory. |
| `reveal_path(path)` | Show `path` in the platform's file manager. |
| `keep_awake(name, reason)` | Hold off the screensaver and system sleep under `name`. Repeating a live name replaces its request. |
| `allow_sleep(name)` | Release the inhibit registered under `name`. |
| `open_menu(id)` | Open menu `id` by setting the `__menu_open:id` signal to true. |
| `close_menu(id)` | Close menu `id`. |

See [OS integration](../guides/os-integration.md) for the markup these pair with.

## Styling and theming

| Builtin | Behaviour |
| --- | --- |
| `set_class(id, classes)` | Replace the class list on the element with that `id`. |
| `set_root_class(classes)` | Replace the class list on the root element, which drives theme-token selectors. |
| `set_color_scheme(name)` | Switch the color scheme: `"default"` (follow the OS), `"force-light"`, `"force-dark"`, `"prefer-light"`, `"prefer-dark"`. An unknown name is ignored with a warning. |

`set_class` takes an element id and `Node.set_class` takes none because it
already has the element. They share a name and do the same thing through
different routes: reach for the global when all you have is an id, and the
method when you are holding a handle.

## Audio

| Builtin | Behaviour |
| --- | --- |
| `audio_play(path)` | Load and play the track at `path` (app-relative wav or ogg); resets position to zero. |
| `audio_pause()` | Pause, holding position. |
| `audio_resume()` | Resume a paused transport. |
| `audio_stop()` | Stop and rewind. |
| `audio_seek(secs)` | Seek to `secs`, clamped to the track duration. Accepts an integer or a float. |
| `audio_volume(level)` | Set output volume in `0.0` to `1.0`. Accepts an integer or a float. |

The transport writes the `audio_position`, `audio_duration`, and `audio_playing`
signals each tick, so markup binds to them directly.

## Networking

| Builtin | Behaviour |
| --- | --- |
| `fetch(url, tag)` | HTTP GET. A 2xx reply fires `on_fetch(tag, body)`; a transport failure or non-2xx fires `on_fetch_error(tag, message)`. |
| `http(request)` | General HTTP request. Fires `on_http(tag, response)` for every completed request. |

`request` is a map: `url` and `tag` are required; `method` defaults to `"GET"`;
`headers` is a map of header name to value; `body` is a string; `timeout_ms` is
a positive integer.

`response` is a map of `ok` (true for a 2xx status), `status` (`0` on a
transport failure), `headers` (names lowercased), `body`, and `error` (empty on
success).

Requests run off the UI thread; the reply is delivered on the tick thread, so a
handler may touch signals and the element tree freely.

## Request and response

The readers give back what arrived with the request the document is being
rendered for, and an empty string when there is none to read; a desktop app has
none. The three writers queue an answer that only a server render applies.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `request_header(name)` | `string` | The named request header, matched without regard to case. |
| `request_cookie(name)` | `string` | The named request cookie. |
| `request_body()` | `string` | The request body. |
| `response_status(status)` | | Answer with HTTP status `status`, clamped to 100..=599. |
| `response_header(name, value)` | | Set a response header; setting the same name twice replaces the value. |
| `redirect(location)` | | Answer with a redirect to `location`, a path or an absolute URL, instead of a document. |

## Data helpers

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `parse_json(json)` | `any` | Parse JSON into a map, array, or scalar. `()` on a parse error. |
| `parse_markdown(src)` | `array` | Parse markdown into block maps of `id`, `kind`, `level`, `text`, `lang`. `kind` is one of `h`, `p`, `code`, `li`, `hr`; `level` carries the heading level. Inline emphasis keeps its markdown delimiters. |
| `local_id(source, suffix)` | `string` | The sibling id `suffix` inside the same template instance as `source`. `local_id("user-card:btn", "label")` is `"user-card:label"`; a `source` without a `:` returns `suffix`. |

## Translation

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `t(key)` | `string` | The active locale's string for `key`, or `key` itself when untranslated. |
| `tr(key)` | `string` | Alias for `t`. |

See [Translation](../guides/i18n.md) for the catalogue format.

## Filesystem

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `read_file(path)` | `string` | File contents; empty string on error. |
| `write_file(path, contents)` | `bool` | `true` on success. |

## Embedder commands

Two builtins emit a command with no built-in effect under `lumenc run`. They
exist for embedders that read the script command stream directly.

| Builtin | Behaviour |
| --- | --- |
| `add_clicks(n)` | Emit an add-clicks command carrying `n`. |
| `set_string(key, value)` | Emit a set-string command carrying `key` and `value`. |

Two more write directly to the element tree by id, without a node handle:

| Builtin | Behaviour |
| --- | --- |
| `set_text(target_id, text)` | Replace the text content of the element with that `id`. |
| `set_src(target_id, path)` | Swap the asset path of an `<image>` at run time. Paths are app-relative. |

Native functions an embedder or a plugin registers appear as bare globals here,
and one registered under a namespace of its own appears as a module:
`gpio::level(21)`. See [FFI and SDKs](ffi.md).
