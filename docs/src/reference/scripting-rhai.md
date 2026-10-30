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
engine = "rhai"
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
`SignalRef`) carry methods dispatched on the receiver. These scope constants
are pre-bound: `signals`, `window`, `document`, `history`, and `app`.

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
| `on_ready()` | Once per mount, on the first tick after the element tree is published. Queries resolve here, including inside the `<if>` and `<for>` bodies the signals `on_start` wrote open. Re-armed after a hot reload, so it runs again on the fresh tree. |
| `on_close()` | Before the window closes: on its close button, an interrupt signal, or the app's own window close or quit. Return `false` to veto the close and keep the window open. |
| `on_archive_done(tag, dest, count)` | When an extraction started through the `lumen-archive` module finishes; `count` is the number of files written. |
| `on_archive_error(tag, message)` | When an extraction started through the `lumen-archive` module is refused or fails. |
| `on_audio_end(path)` | When a track played through the `lumen-audio` module reaches its end; `path` is the path passed to `audio_play`. |

Top-level statements run once when the script loads, against a scope that
persists for the app's lifetime, so a top-level `let` is state every later
handler can read and write. Across a hot reload the signal values, the `on(...)`
routing table, and live event bindings are preserved.

## Event handlers

Each event dispatches to a global handler function named below. Handlers are
optional.

| Handler | Arguments |
| --- | --- |
| `on_click(id)` | The id of the nearest element at or above the click that has one. Fires on every click, the second of a quick pair included, as a DOM `click` does. |
| `on_double_click(id)` | Element id, as for `on_click`. Fires once for a quick pair of clicks, after the `on_click` of the second. |
| `on_long_press(id)` | Element id, as for `on_click`. |
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
| `on_recent_files(tag, paths)` | Request tag, recorded paths joined with `\|`, most recent first. |
| `on_autostart_enabled(tag)` | Request tag; the app is set to launch at login. |
| `on_autostart_disabled(tag)` | Request tag; the app is not set to launch at login. |
| `on_second_instance(args)` | A second launch's command-line arguments, joined with `\|`. |
| `on_dialog_accepted(id)` | Dialog id. |
| `on_dialog_rejected(id)` | Dialog id. |
| `on_timer(name)` | Timer name. |
| `on_fetch(tag, body)` | Request tag, response body (2xx only). |
| `on_fetch_error(tag, message)` | Request tag, error text. |
| `on_http(tag, response)` | Request tag, the response map described under [Networking](#networking). |
| `on_download_progress(tag, received, total)` | Download tag, bytes so far, the size the server declared or `-1`. Fires only for a transfer started by the `lumen-download` module. |
| `on_download_done(tag, path)` | Download tag, the path the downloaded file was written to. |
| `on_download_error(tag, message)` | Download tag, error text. |
| `on_process_stdout(tag, line)` | Tag given to `process::start`, one line the program wrote to standard output. |
| `on_process_stderr(tag, line)` | Tag, one line the program wrote to standard error. |
| `on_process_exit(tag, code)` | Tag, the program's exit code. Always the last event for a tag. |

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
| `signal_set_color(name, hex)` | | Write a hex colour: `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`. An unparseable value is ignored. |
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

## Frame callbacks

| Builtin | Behaviour |
| --- | --- |
| `request_frame()` | Ask for one `on_frame(dt)` call on the next tick. |

One request buys one callback. A handler that wants to keep animating asks
again from inside itself, and an animation that is finished simply stops
asking, which parks the app. `dt` is the seconds since the previous callback,
capped so a stalled tick slows an animation instead of teleporting it, and
zero on the frame that starts a loop because no interval has passed yet.

```rhai
fn on_ready() { request_frame(); }

fn on_frame(dt) {
    signals.angle.set(signals.angle.get() + dt * 2.0);
    request_frame();
}
```

Use this rather than a repeating timer for anything visual: a timer fires on a
wall-clock schedule that has nothing to do with when the app draws.

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
| `Node.set_pointer_capture(pointer_id)` | | Send that pointer's events to this element until its last button comes up. Does nothing while no button is held. |
| `Node.release_pointer_capture(pointer_id)` | | Give the pointer back to hit testing, when this element holds it. |
| `Node.has_pointer_capture(pointer_id)` | `bool` | Whether this element holds the pointer, counting a capture set in the running handler. |
| `Node.request_pointer_lock()` | | Lock the pointer to this element: the cursor hides and stays put, and pointer moves carry the motion. Granted only to the focused element, during or just after a key or button press; `pointerlockchange` or `pointerlockerror` on the element answers. |

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
| `code()` | `string` | Where the key sits, by the DOM `code` name (`"KeyW"`, `"ShiftLeft"`, `"Numpad1"`): the same key on every layout, which is what a game binds movement to. |
| `value()` | `string` | Text value for `input` / `change` / `submit`. |
| `detail()` | `map` | What a module's custom event carries; empty for a built-in event. |
| `button()` | `int` | The web `button` code: `0` primary, `1` middle, `2` secondary, `3` back, `4` forward, `5` pen eraser, `-1` none (a move). |
| `buttons()` | `int` | Buttons held, as a mask: `1` primary, `2` secondary, `4` middle, `8` back, `16` forward, `32` eraser. |
| `pointer_id()` | `int` | The pointer's id; the mouse is `1`, each touch and pen has its own. |
| `pointer_type()` | `string` | `"mouse"`, `"pen"`, or `"touch"`. |
| `is_primary()` | `bool` | Whether the pointer is the primary one of its kind. |
| `pressure()` | `float` | Normalized pressure, `0` to `1`; `0.5` while pressed on a device without a sensor. |
| `tangential_pressure()` | `float` | Normalized barrel pressure, `-1` to `1`. |
| `tilt_x()` / `tilt_y()` | `float` | Pen tilt toward the right and toward the user, `-90` to `90` degrees. |
| `twist()` | `float` | Pen rotation about its axis, `0` to `359` degrees. |
| `width()` / `height()` | `float` | Contact size in logical pixels; `1` for a point device. |
| `x()` / `y()` | `float` | Pointer position relative to the target. |
| `client_x()` / `client_y()` | `float` | Pointer position in window coordinates. |
| `movement_x()` / `movement_y()` | `float` | How far the pointer moved since its previous move, in logical pixels. Under a pointer lock the position stays put and these carry the device's motion. |
| `delta_x()` / `delta_y()` | `float` | Wheel delta in logical pixels. As in the DOM, positive `delta_y` scrolls down and positive `delta_x` scrolls right. |
| `delta_mode()` | `int` | The unit the wheel reported its delta in, the DOM `deltaMode`: `0` pixels (a touchpad), `1` lines (a wheel turning in notches), `2` pages. The delta itself is in pixels either way. |
| `position()` | `map` | `x`, `y`, `client_x`, `client_y`. |
| `modifiers()` | `map` | `shift`, `ctrl`, `alt`, `super`. |
| `prevent_default()` | | Cancel the default action. |
| `stop_propagation()` | | Stop the event reaching further elements. |
| `stop_immediate_propagation()` | | Stop the event entirely, including other handlers on this element. |

### Event types

`click`, `dblclick`, `contextmenu`, `pointerdown`, `pointerup`, `pointermove`,
`pointercancel`, `pointerenter`, `pointerleave`, `gotpointercapture`,
`lostpointercapture`, `wheel`, `keydown`, `keyup`, `input`, `change`, `focus`,
`blur`, `submit`, `scroll`, `pointerlockchange`, `pointerlockerror`.

A module adds event types of its own, under names it chooses (a dock's
`layoutchange`, `panelclose`), raised on its elements and dispatched the same
way; the module's documentation lists them and says which bubble and which a
handler can cancel with `prevent_default`. They carry a detail map, which a
built-in event leaves empty.

Dispatch runs capture (root down to the target), then the target, then bubble
(target up to the root). `focus`, `blur`, `pointerenter`, `pointerleave`, and
`scroll` do not bubble. `keydown` and `keyup` target the focused element, or
the document root when nothing is focused.

`pointerenter` fires on an element when the pointer reaches it or one of its
descendants, outermost element first, and `pointerleave` when the pointer is
over neither, innermost first, as on the web: moving from a button onto its
icon fires `pointerenter` on the icon and nothing on the button.

A pointer event targets the element under its pointer. After
`set_pointer_capture(pointer_id)` on an element, that pointer's events go to
the element until its last button comes up, wherever the pointer moves;
`gotpointercapture` fires on the element before the first of them and
`lostpointercapture` after the release. A touch is captured by the element it
went down on. `pointercancel` replaces `pointerup` when the system takes a
touch or pen over mid-gesture.

`request_pointer_lock()` asks for the pointer lock, the way a 3D view or a
game takes the mouse: the cursor hides and stays where it is, the mouse's
events go to the element wherever the pointer is, and `movement_x()` /
`movement_y()` carry the motion. The lock is granted only to the element that
has focus, during or just after a press of a key or a button (a click on the
element is both). Escape always ends it, and so does the window losing focus
or the element going away. `pointerlockchange` fires on the element that took
the lock and on the one that lost it; `pointerlockerror` fires on an element
that was refused. Pointer lock is not available in a web build yet.

`input` fires per keystroke: every edit that changes the text raises one,
carrying the buffer as it stands after that edit. Text an input method commits
is an edit like a keystroke. A caret move raises nothing.
A color field raises `input` for every color it moves to and `change` when its
panel closes on a new one, with the color in `value`: `#rrggbb`, or
`color(srgb r g b / a)` on a field with `alpha`. A `<checkbox>`, `<toggle>` or
`<switch>` the user flips raises `input` and then `change`, with `true` or
`false` in `value`. A `<slider>` raises `input` for every value it moves to and
`change` when the user settles on one: a click, a key, a wheel notch, or the
release that ends a drag. A `<dropdown>` raises `input` and then `change` when
the user picks an option other than the one it holds, with the option's value,
and a `<radio>` the user chooses raises them on itself, with its `value`.
A script or a binding that sets any of these raises nothing.
On a text field, `change` and `submit` come from the commit signal, so they fire
when the field is committed with Enter. Leaving a field after an edit that was
not committed raises `change` without `submit`, just before `blur`.

`contextmenu` fires when the user asks for a context menu: a right click on the
element under the pointer (on the press, or the release on Windows), the
ContextMenu key or Shift+F10 on the focused element, or an assistive
technology's request. Its position is the pointer for a right click and just
below the element's left edge otherwise, and `button` is `2` for a right click
and `0` otherwise. A desktop window opens no menu of its own, so open one from
the handler with `open_menu_at`.

Two types have a default action: `click` (link navigation) and `contextmenu`
(the browser's own menu, in a web build). `prevent_default` skips it.

## window, document, history, app

Pre-bound scope constants.

The window functions act on the current window: the window of the event
being handled, else the app's main window. The window setters ask for a
change, which the window takes after the tick:
the getters report what the window has, so a new title or size reads back on a
later tick. The window manager may refuse or adjust a size; a tiling window
manager sizes windows itself. A maximized window is restored to take a new
size.

| Call | Returns | Behaviour |
| --- | --- | --- |
| `window.set_href(path)` | | Navigate to a page path. |
| `window.href()` | `string` | The current page path. |
| `window.reload()` | | Re-navigate to the current page. |
| `window.title()` | `string` | The title the window has. |
| `window.set_title(title)` | | Ask for a new window title. |
| `window.dpr()` | `float` | Device pixel ratio. |
| `window.size()` | `array` | `[width, height]` the window has, in logical pixels. |
| `window.set_size(width, height)` | | Ask for a new window size, in logical pixels. A maximized window is restored first. Both sides must be positive. |
| `window.close()` | | Close the window. `on_close` runs first and can keep it open; closing the main window ends the app unless `[app] quit = "explicit"`. |
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
| `document.hovered()` | `Node` or `()` | The innermost element under the pointer. |
| `document.create(tag)` | `Node` | Create a detached element. |
| `document.exit_pointer_lock()` | | End the pointer lock, whoever holds it. |
| `document.pointer_lock_element()` | `Node` or `()` | The element holding the pointer lock. |
| `app.quit()` | | Quit the app. `on_close` runs first and can cancel the quit. |

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
| `copy_image(path)` | Copy the image at `path` to the system clipboard. Relative paths resolve against the app directory. |
| `save_clipboard_image(path)` | Write the clipboard image to `path` as PNG. Relative paths resolve against the app directory. |
| `tray_icon(id, icon_path, tooltip)` | Register or replace a tray icon; clicks fire `on_tray(id)`. An empty tooltip disables it. A relative `icon_path` resolves against the app directory. |
| `tray_icon_menu(id, icon_path, tooltip, menu, template)` | Register a tray icon with a context menu, given as pipe-separated `id:Label` entries where `-` is a separator; a pick fires `on_menu(id)`. `template` is the macOS monochrome-icon flag, ignored elsewhere. A relative `icon_path` resolves against the app directory. |
| `unregister_tray(id)` | Remove a tray icon. |
| `register_hotkey(name, accelerator)` | Register a global hotkey (`"CommandOrControl+S"`, `"Alt+Space"`, `"F11"`); fires `on_hotkey(name)`. |
| `unregister_hotkey(name)` | Remove a global hotkey. |
| `open_url(url)` | Open `url` with the default browser, or the mail client for `mailto:`. |
| `open_path(path)` | Open `path` with the platform's default application. Relative paths resolve against the app directory. |
| `reveal_path(path)` | Show `path` in the platform's file manager. |
| `keep_awake(name, reason)` | Hold off the screensaver and system sleep under `name`. Repeating a live name replaces its request. |
| `allow_sleep(name)` | Release the inhibit registered under `name`. |
| `add_recent_file(path, label)` | Record `path` as recently opened. An empty `label` derives one from the path's file name. |
| `list_recent_files(tag)` | Request the recent-files list; fires `on_recent_files(tag, paths)` with paths joined by `\|`, most recent first. |
| `clear_recent_files()` | Remove every entry from the recent-files list. |
| `set_autostart(on)` | Enable or disable launching this app at login. |
| `query_autostart(tag)` | Request the autostart state; fires `on_autostart_enabled(tag)` or `on_autostart_disabled(tag)`. |
| `open_menu(id)` | Open menu `id` where its styles place it, by setting the `__menu_open:id` signal to true. |
| `open_menu_at(id, x, y)` | Open menu `id` with its top-left corner at `(x, y)` in window coordinates. Near the right or bottom edge it opens to the left of or above the point instead. Pass a `contextmenu` event's client position to open a context menu at the pointer. |
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

These functions come from the `lumen-audio` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`; see
[OS integration](../guides/os-integration.md#audio).

| Builtin | Behaviour |
| --- | --- |
| `audio_play(path)` | Load and play the track at `path` (app-relative wav or ogg, resolved through the app's asset sources, so a packed archive and `lumen://app/...` URIs work); resets position to zero. |
| `audio_pause()` | Pause, holding position. |
| `audio_resume()` | Resume a paused transport. |
| `audio_stop()` | Stop and rewind. |
| `audio_seek(secs)` | Seek to `secs`, clamped to the track duration. Accepts an integer or a float. |
| `audio_volume(level)` | Set output volume in `0.0` to `1.0`. Accepts an integer or a float. |

The module writes the `audio_position`, `audio_duration`, and `audio_playing`
signals each tick, so markup binds to them directly.

## Networking

| Builtin | Behaviour |
| --- | --- |
| `fetch(url, tag)` | HTTP GET. A 2xx reply fires `on_fetch(tag, body)`; a transport failure or non-2xx fires `on_fetch_error(tag, message)`. |
| `http(request)` | General HTTP request. Fires `on_http(tag, response)` for every completed request. |

`request` is a map: `url` and `tag` are required; `method` defaults to `"GET"`;
`headers` is a map of header name to value; `body` is a string; `timeout_ms` is
a positive integer; `credentials` is a string.

`credentials` says whether a request carries the page's cookies and HTTP
authentication, with the values the browser's `fetch` takes: `"same-origin"`
(the default), `"include"`, or `"omit"`. It matters in a web build, where the
request is the page's own `fetch`; a cross-origin `"include"` also needs the
server to answer with `Access-Control-Allow-Credentials: true`. A desktop app
has no page cookies; with the [`lumen-cookie`](#cookies) module declared,
`"omit"` keeps a request out of its cookie jar, and the other two values let
it in. Any other value is a script error.

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
| `set_locale(tag)` | `()` | Switch the app to the BCP-47 locale `tag`. Marked text, placeholders, tooltip bodies, `format` output and the base writing direction follow on the same tick. A locale with no catalogue falls back to the authored text; a tag that is not BCP-47 is logged and ignored. |
| `locale()` | `string` | The BCP-47 tag of the locale the app is running in, including the one it started in when no script has switched it. |

See [Translation and formatting](../guides/i18n.md) for the catalogue format.

## Formatting

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `format_number(n)` | `string` | `n` written the way the active locale writes numbers. |
| `format_currency(amount, currency)` | `string` | `amount` as money in the ISO-4217 code `currency`, positioned and rounded the way the locale and the currency ask for. |
| `format_date(iso)` | `string` | The date in `iso` (`YYYY-MM-DD`, time optional) written for the locale. |
| `format_time(iso)` | `string` | The time in `iso` (`YYYY-MM-DDTHH:MM[:SS]`) written for the locale. |
| `format_datetime(iso)` | `string` | Date and time from `iso` written for the locale. |
| `format_relative(seconds)` | `string` | `seconds` from now as the locale says it: `-7200` is `2 hours ago`. |

A trailing zone on a timestamp (`Z`, `+02:00`) is read and dropped; nothing is
converted between time zones. A value the call cannot read comes back as it was
given, the way `t` returns an untranslated key.

See [Translation and formatting](../guides/i18n.md) for the markup attribute
that formats an element's text.

## Canvas

These functions come from the `lumen-canvas` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-canvas = { bundled = true }
```

Every call names a canvas by the `id` on its [`<canvas>`](tags.md#canvas)
element. Colours are components in 0..1, angles are radians, and coordinates
are canvas units, which are scaled onto the element's box whenever layout
gives it one the declaration did not ask for.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `canvas::width(id)` | `int` | The drawing width, in canvas units. |
| `canvas::height(id)` | `int` | The drawing height, in canvas units. |
| `canvas::resize(id, width, height)` | | Set the drawing space. This empties the canvas, as writing `width` on an HTML canvas does. |
| `canvas::clear(id)` | | Erase everything the canvas holds. |
| `canvas::begin_path(id)` | | Start a new path. |
| `canvas::move_to(id, x, y)` | | Start a subpath at a point. |
| `canvas::line_to(id, x, y)` | | Straight segment to a point. |
| `canvas::quad_to(id, cx, cy, x, y)` | | Quadratic segment through one control point. |
| `canvas::bezier_to(id, c1x, c1y, c2x, c2y, x, y)` | | Cubic segment through two control points. |
| `canvas::arc(id, x, y, radius, start, end)` | | Circular arc, angles in radians, joined to the current subpath. |
| `canvas::rect(id, x, y, width, height)` | | Add a closed rectangle to the path. |
| `canvas::close_path(id)` | | Close the current subpath back to its start. |
| `canvas::fill(id)` | | Fill the current path. |
| `canvas::fill_evenodd(id)` | | Fill the current path under the even-odd rule, so a region inside it twice is a hole. |
| `canvas::clip(id)` | | Intersect the clip with the current path. Every later draw shows only inside it, until a `restore` (or `clear`, or `resize`) widens it again. |
| `canvas::clip_evenodd(id)` | | The same under the even-odd rule. |
| `canvas::stroke(id)` | | Stroke the current path. |
| `canvas::fill_rect(id, x, y, width, height)` | | Fill one rectangle, leaving the path alone. |
| `canvas::stroke_rect(id, x, y, width, height)` | | Stroke one rectangle, leaving the path alone. |
| `canvas::clear_rect(id, x, y, width, height)` | | Make a rectangle transparent, through the transform and the clip. The alpha and the composite operation do not apply, and the path and state are kept. |
| `canvas::set_fill_rgba(id, r, g, b, a)` | | Set the fill colour from four components. |
| `canvas::set_fill_style(id, color)` | `bool` | Set the fill colour from CSS text (`#rgb`, `#rrggbbaa`, `rgb()`, `rgba()`, the CSS level 1 names, `transparent`). `false` when the text is none of those, and the colour is left alone. |
| `canvas::set_stroke_rgba(id, r, g, b, a)` | | Set the stroke colour from four components. |
| `canvas::set_stroke_style(id, color)` | `bool` | Set the stroke colour from CSS text, on the same terms. |
| `canvas::set_line_width(id, width)` | | Stroke width, in canvas units. |
| `canvas::set_line_cap(id, cap)` | `bool` | `butt`, `round`, or `square`. |
| `canvas::set_line_join(id, join)` | `bool` | `miter`, `round`, or `bevel`. |
| `canvas::set_global_alpha(id, alpha)` | | Multiply every later draw by this alpha. |
| `canvas::set_image_smoothing(id, enabled)` | | Whether a buffer drawn at another size is smoothed. On by default, as on the web; off samples the nearest pixel, which keeps zoomed pixel art sharp. |
| `canvas::set_global_composite_operation(id, operation)` | `bool` | How later draws combine with what the canvas holds, by the HTML canvas's `globalCompositeOperation` names: `source-over` (the default), `source-in`, `source-out`, `source-atop`, `destination-over`, `destination-in`, `destination-out`, `destination-atop`, `lighter`, `copy`, `xor`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`, `difference`, `exclusion`, `hue`, `saturation`, `color`, `luminosity`. `false` for any other name. |
| `canvas::save(id)` | | Push the drawing state: colours, line style, alpha, image smoothing, composite operation, clip, transform. |
| `canvas::restore(id)` | | Pop it. Popping past the bottom leaves the state alone. |
| `canvas::translate(id, x, y)` | | Move the origin. |
| `canvas::rotate(id, radians)` | | Rotate the transform. |
| `canvas::scale(id, x, y)` | | Scale the transform. |
| `canvas::reset_transform(id)` | | Drop back to the identity. |
| `canvas::set_transform(id, a, b, c, d, e, f)` | | Replace the transform outright. |
| `canvas::set_font(id, font)` | `bool` | `"[weight] <size>px [family]"`, as in `"bold 16px Inter"`. The size is required; `false` when it is missing. |
| `canvas::fill_text(id, text, x, y)` | | Draw text in the fill colour, with `(x, y)` on the alphabetic baseline, shaped with the app's own fonts. |

A path is stored in canvas units and placed by the transform at the moment it
is filled or stroked, so changing the transform between building a path and
filling it moves the result. The HTML canvas bakes the transform into each
segment as it is added; this is the one place the two differ.

A composite operation works on the canvas's own pixels, never on the page
behind the element: `destination-out` makes the canvas transparent where it
draws, and the page shows through. The operators that also clear what the
draw does not cover (`copy`, `source-in`, `source-out`, `destination-in`,
`destination-atop`) clear the whole canvas, or the whole clip when one is
set. On the GPU renderer, under a clip, they also clear a band up to 16
pixels wide just outside the clip's edge; the CPU renderer and a page clear
the clip exactly.

### Pixel buffers

Buffers are plain CPU pixels a script creates, edits, loads from a PNG, saves
to one, draws onto a canvas, and reads a canvas into. A pixel is one packed
`0xRRGGBBAA` integer with straight (not premultiplied) alpha, so what a script
writes is what it reads back.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `canvas::buffer_new(width, height)` | `int` | A new transparent buffer; `0` when it was refused, with the reason on stderr. |
| `canvas::buffer_free(buffer)` | `bool` | Release a buffer. `false` when the handle names none. |
| `canvas::buffer_width(buffer)` | `int` | Its width, or `0`. |
| `canvas::buffer_height(buffer)` | `int` | Its height, or `0`. |
| `canvas::buffer_resize(buffer, width, height)` | `bool` | Change its size, keeping the pixels both sizes share at the top left; new area is transparent. `false` when the handle names none or the size is over the cap. |
| `canvas::buffer_get_pixel(buffer, x, y)` | `int` | One pixel as `0xRRGGBBAA`. A pixel outside the buffer reads `0`. |
| `canvas::buffer_set_pixel(buffer, x, y, rgba)` | | Write one pixel. Outside the buffer, nothing happens. |
| `canvas::buffer_get_region(buffer, x, y, width, height)` | `int[]` | A rectangle of pixels, row-major. Pixels off the edge read `0`, so the result is always the size that was asked for. |
| `canvas::buffer_put_region(buffer, x, y, width, height, pixels)` | | Write a rectangle back. A short array is a partial write. |
| `canvas::buffer_fill_rect(buffer, x, y, width, height, rgba)` | | Fill a rectangle with one colour. |
| `canvas::buffer_load_png(path)` | `int` | Decode a PNG into a new buffer; `0` when it could not be read. The path resolves against the app directory. |
| `canvas::buffer_save_png(buffer, path)` | `bool` | Write a buffer out as a PNG. |
| `canvas::get_image_data(id, x, y, width, height)` | `int` | Read a region of the canvas, as composed, into a new buffer: `getImageData` on the web. One canvas unit is one pixel. `0` when the buffer was refused. See below for when it is filled. |
| `canvas::draw_buffer(id, buffer, x, y)` | | Draw a buffer onto a canvas at its own size. |
| `canvas::draw_buffer_scaled(id, buffer, x, y, width, height)` | | Draw a buffer stretched into a box. |

A draw takes the buffer's pixels at the call, as `drawImage` does on the web: a
write after it, even in the same handler, changes the buffer and not what was
drawn. To show a write, draw the buffer again. Drawing a buffer that has not
changed costs no upload, and drawing one after a write uploads only the area
the write touched, so redrawing a large buffer every frame stays cheap.

A read sees every call made on the canvas before it and none after. It is
answered when the canvas next encodes, after the handler that asked returns,
so the buffer is transparent until the next handler; a read in `on_ready`,
before the first frame, waits for that frame. A read rasterizes the canvas
again on the app's renderer, which on a GPU means a render and a copy back,
so reading every frame costs a second render every frame.

Three settings bound what one call may ask for, from the module's `config`
table: `region_cap` (pixels in one region call), `buffer_pixel_cap` (pixels in
one buffer), and `buffer_count_cap` (buffers at once). A call over a cap
answers empty or `0` and says why on stderr.

Refusals never raise. An unknown canvas id keeps the drawing, in case the
element has not been mounted yet, and reports once if no element ever matches;
an unknown buffer handle reads as an empty buffer. See
[`<canvas>`](tags.md#canvas) for the element, its sizing, and what is not
implemented yet.

## Surfaces

These functions come from the `lumen-surface` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-surface = { bundled = true }
```

Each names a [`<surface>`](tags.md#surface) element by its `id`.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `surface::endpoint(id)` | `string` | The endpoint string a producer connects with. Hand it to the producer, by the `LUMEN_SURFACE` environment variable by convention. Empty when no surface has the id, and on Windows, where a producer in another process cannot connect yet. |
| `surface::state(id)` | `string` | `waiting`, `live`, `stalled`, `lost` or `unsupported`. |
| `surface::frame_info(id)` | `map` | The frame on screen: `frame`, `width`, `height`, `generation`, `input_seq`, `meta_len`. Empty before the first frame. |
| `surface::frame_meta(id)` | `array` | The producer's bytes about the frame on screen, such as the camera it drew it with. |
| `surface::request_frame(id)` | | Ask a paced producer for a frame now, so a still scene shows an edit. |
| `surface::send(id, message)` | `bool` | Send a message to the producer's application; `false` when none is connected. |
| `surface::disconnect(id)` | | End the session with the producer. The surface keeps listening for another. |

What the producer sends back arrives as events on the element:
`surfacemessage` carries a message in its detail (`text`, and the raw
`bytes`). See [Showing another renderer's frames](../guides/surfaces.md).

## Docks and windows

These functions come from the `lumen-dock` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-dock = { bundled = true }
```

They name a [`<dock-panel>`](tags.md#dock-and-its-panels) by its `id`.
Every window but the main one is a panel in a window of its own, so these
also open, place and close the app's other windows. A call naming a panel the
module does not know does nothing and answers `false`, with one line on
stderr.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `dock::show(panel)` | `bool` | Bring the panel into view where it was hidden from, else where its `place` puts it, and make it its stack's showing tab. A panel outside any dock, or one that does not dock, opens in a window of its own. |
| `dock::hide(panel)` | `bool` | Take the panel out of view, remembering where it was. Its content stays mounted. |
| `dock::toggle(panel)` | `bool` | Hide the panel if it is shown, else show it; a pinned panel slides out and back in. Whether it is shown afterwards. |
| `dock::focus(panel)` | `bool` | Show the panel and move keyboard focus into it: to the element last focused there, else its first focusable element, else its tab. Raises its window. |
| `dock::activate(panel)` | `bool` | Make the panel its stack's showing tab. |
| `dock::close(panel)` | `bool` | Close the panel without asking: hide it, or remove its element when it has `destroy-on-close`. |
| `dock::move(panel, target, zone)` | `bool` | Move the panel beside `target`, a panel or a dock id. `zone` is `tab` (into the target's stack, or the dock's center stack) or `left`, `right`, `top`, `bottom` (a new stack on that side of the target's stack, or along that edge of the dock). |
| `dock::detach(panel)` | `bool` | Pop the panel out into a window of its own, over the place it has on screen. |
| `dock::detach_group(panel)` | `bool` | Pop the panel's whole stack out into a window of its own. |
| `dock::float(panel, x, y, w, h)` | `bool` | Float the panel over its dock at `x`, `y` from the dock's top-left corner, `w` by `h` pixels, in a group of its own; a panel already alone in a float moves there. `false` when it does not float (`floatable="false"`) or does not dock. |
| `dock::pin(panel, edge)` | `bool` | Pin the panel to an edge of its dock, `left`, `right`, `top` or `bottom`: it shows as a button along that edge that slides it out over the dock. `dock::show` slides it out too. |
| `dock::attach(panel)` | `bool` | Dock the panel back to where it came from: from its window, its float, or the edge it is pinned to. `false` when it is in a dock already, or does not dock. |
| `dock::window_of(panel)` | `int` | `0` for the main window, the window's number for a window of its own, `-1` when the panel is not shown. |
| `dock::windows()` | `array` | The numbers of the windows of their own that hold panels. |
| `dock::collapse(panel, collapsed)` | `bool` | Collapse the panel's stack to its header, or expand it. |
| `dock::maximize(panel)` | `bool` | Fill the panel's dock with its stack. `false` when the panel is not in a dock. |
| `dock::restore_maximized(dock)` | `bool` | Give the dock its whole layout back. `false` when nothing in it was maximized. |
| `dock::panels()` | `array` | Every panel id the module knows, shown or not. |
| `dock::is_shown(panel)` | `bool` | Whether the panel is in view: in a dock, floating, in a window of its own, or slid out of its edge. |
| `dock::save()` | `string` | The whole arrangement as JSON, for `dock::restore` in this run or another. |
| `dock::restore(layout)` | `bool` | Arrange everything as a saved layout says. `false`, with a `layouterror` event, for a layout of another format or `layout-version`, or a broken one. |
| `dock::layouts()` | `array` | The named layouts: the markup's `<dock-layout>`s and the user's own. |
| `dock::save_layout(name)` | `bool` | Keep the arrangement as the named layout `name`. |
| `dock::apply_layout(name)` | `bool` | Arrange everything as the named layout says. Panels it does not name keep their place. |
| `dock::delete_layout(name)` | `bool` | Delete a layout the user saved. |
| `dock::reset()` | `bool` | Go back to the named layout the arrangement came from, else to the markup's. |
| `dock::forget(panel)` | `bool` | Drop everything remembered about a panel: its place and what a restore would ask the app to recreate. |

The dock tells the app what changed with events on the panels and the
dock (`panelshow`, `panelclose`, `panelmove`, `layoutchange` and the rest)
and the `dock.active`, `dock.layout` and `dock.shown.<panel>` signals; see
[`<dock>`](tags.md#dock-and-its-panels) for each, and
[Docks and windows](../guides/docks.md) for the whole picture.

## Filesystem

These functions come from the `lumen-fs` runtime module and exist only when
the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-fs = { bundled = true }
```

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `files::exists(path)` | `bool` | Whether anything exists at `path`. Symlinks are followed. |
| `files::is_dir(path)` | `bool` | Whether `path` is a directory that exists. |
| `files::list(path)` | `array` | The entry names directly inside `path`, sorted. Names, not paths, and one level deep. A directory that cannot be read gives an empty array. |
| `files::mkdir(path)` | `bool` | Create `path` and every directory above it. A directory already there is success. |
| `files::remove(path)` | `bool` | Remove one file, or one directory that is already empty. A directory holding anything is refused; a path that is not there answers `false`. |
| `files::copy(src, dest)` | `bool` | Copy one file, creating the directories `dest` sits under. A directory source is refused. |
| `files::read(path)` | `string` | The utf-8 contents of `path`, or an empty string when it is not there. |
| `files::write(path, contents)` | `bool` | Write `contents` to `path`. The write is atomic (temp file + rename), so a reader never sees a truncated file. |
| `files::read_bytes(path)` | `array` | The bytes of `path` as integers of 0 to 255. A missing file, or one past the cap, gives an empty array. |
| `files::write_bytes(path, bytes)` | `bool` | Write an array of 0-to-255 integers as raw bytes, atomically. A value outside that range refuses the whole write. |
| `files::digest(path, algo)` | `string` | The digest of the file at `path` as lowercase hex, where `algo` is `"md5"`, `"sha1"` or `"sha256"`. A missing file gives an empty string; any other `algo` is refused. |
| `files::data_dir()` | `string` | The directory this app saves data in, created when missing. |

A relative path names a file the app ships, so it reads the same wherever the
app was started from; an absolute path is left alone. Saved state goes under
`files::data_dir()` instead, because the app directory is read-only once the
app is installed:

```rhai
files::write(files::data_dir() + "/session.json", state);
```

`data_dir()` follows the platform convention for user data (`$XDG_DATA_HOME`,
else `~/.local/share`, on Linux; `~/Library/Application Support` on macOS;
`%APPDATA%` on Windows) and names one directory per app from
[`[app] id`](lumen-toml.md), so two apps on a machine keep their saves apart.

A call that cannot do what it was asked answers `false` or an empty value and
prints one `lumen-fs:` line on stderr, so a script branches on the value it
got back. Two cases stay silent, because probing for state that has not been
saved yet is ordinary: reading a file that is not there, and removing one.

`files::digest` reads the file through in pieces on the calling thread, so
its size is not capped but the script waits while a large file hashes. md5
and sha1 are for ids and checksums, such as checking a download against a
published sum, not for security.

`files::read_bytes` reads up to 8 MiB by default. Raise or lower it with the
module's `read_bytes_cap` setting, in bytes, between 1 KiB and 256 MiB:

```toml
[dependencies]
lumen-fs = { bundled = true, config = { read_bytes_cap = 33554432 } }
```

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](../guides/packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

## Archives

These functions come from the `lumen-archive` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-archive = { bundled = true }
```

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `archive::extract(src, dest, tag, opts)` | `bool` | Unpack the archive at `src` into the directory `dest`, creating it. `true` when the job was taken; `false` when it was not, which also fires `archive_error`. |

Both paths resolve against the app directory, and the extraction runs off the
tick loop, so the call answers before any bytes are read. The outcome arrives
as an event keyed by `tag`:

| Event | Handler | Arguments |
| --- | --- | --- |
| `archive_done` | `on_archive_done` | `tag`, `dest` (the resolved destination), `count` (files written) |
| `archive_error` | `on_archive_error` | `tag`, `message` |

`on("archive_done", tag, fn)` registers a handler for one job and wins over
the fallback, the same as any other event.

```rhai
fn on_start() {
    archive::extract("themes.zip", "themes", "themes", #{});
}
fn on_archive_done(tag, dest, count) { signal("status", "").set(count + " files"); }
fn on_archive_error(tag, message) { signal("status", "").set(message); }
```

`opts` is a map of the options below; a key left out takes its default, so
`#{}` keeps every file. A key the options do not have, or a value of the
wrong type, raises before anything is read:

```rhai
archive::extract("natives.jar", "natives", "natives", #{ include: ["*.so", "*.dll"] });
```

| Field | Type | Default | Behaviour |
| --- | --- | --- | --- |
| `include` | array of strings | empty | Glob patterns for the files to write. Empty writes every file. |

`*` and `?` match inside one path segment and `**` matches any number of
segments. A pattern without a `/` is matched against the file name at any
depth, so `*.so` keeps `linux/x64/liblwjgl.so`; a pattern with one is matched
against the whole path inside the archive. Only the directories the kept files
need are created. Every entry still passes the path checks below, so a hostile
entry fails the extraction even when the filter would have dropped it.
`count` is the files written.

zip, tar, and gzip-compressed tar are read. The container is taken from the
file's leading bytes, so an archive saved under a name that disagrees with its
contents still unpacks; the extension decides only when the bytes say nothing.

What an archive may write is settled before anything is written. An entry
naming an absolute path, climbing out with `..`, carrying a Windows drive or
UNC prefix, or resolving outside `dest` ends the whole extraction with an
`archive_error` naming it, rather than being passed over. Entries written
before the refused one stay on disk, so a destination that took a failed
extraction is one to discard rather than keep using. Symbolic and hard links
are skipped, because a link inside the destination can point outside it once
extraction is over; `count` is the files written, so it leaves them out.
Existing files are overwritten and missing parent directories are created.

Four extractions run at once by default; a fifth is refused until one
finishes, as is a second job under a tag already running. Change the limit
with the module's `max_concurrent` setting:

```toml
[dependencies]
lumen-archive = { bundled = true, config = { max_concurrent = 2 } }
```

Stripping leading path components, per-entry progress, listing an archive
without unpacking it, and writing an archive are not part of this surface.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](../guides/packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

## Downloads

This function comes from the `lumen-download` runtime module and exists only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-download = { bundled = true }
```

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `download::to_file(url, path, tag, checksum)` | `bool` | Start downloading `url` to `path`, reporting under `tag`. True once the transfer is running, false when the call was refused. |
| `download::cancel(tag)` | `bool` | Cancel the download running under `tag`. False when nothing runs under it. |

Every argument is required. `checksum` is `sha256:` followed by 64 hex digits,
`sha1:` followed by 40, a bare hex string of either length, or an empty string
to check nothing; neither the prefix nor the digits are case sensitive. Any
other spelling fails the call. sha1 is accepted because it is what most package
and artifact registries publish, and the digest here checks a file against a
corrupt transfer rather than standing in for a signature.

The call answers as soon as the transfer starts, and the transfer reports
through three handlers keyed by the tag it was given:

| Handler | Arguments |
| --- | --- |
| `on_download_progress(tag, received, total)` | Download tag, bytes so far, the size the server declared or `-1`. |
| `on_download_done(tag, path)` | Download tag, the path the file was written to. |
| `on_download_error(tag, message)` | Download tag, error text. |

A per-tag `on("download_done", tag, handler)` registration wins over the
fallback, the same routing every event gets.

Every refusal arrives as `download_error` under the tag the call named, and
the call itself answers false.

`download::cancel(tag)` stops a running transfer, however quiet its server has gone,
removes the partial file, and leaves the destination as it was. The tag and its
`max_concurrent` slot are free as soon as the call returns, so the next
`download::to_file` under the same tag starts at once, and the cancel arrives as the
tag's last event: `download_error` with the message `cancelled`, which a
handler can compare against to tell a cancel from a failure. A transfer that
has already finished is not cancelled; its own outcome is reported and the call
answers false.

```rhai
fn on_start() {
    download::to_file("https://example.com/pack.zip", "cache/pack.zip", "pack", "");
}
fn on_download_progress(tag, received, total) {
    if total > 0 { signal("pack_progress", "0").set(received * 100 / total); }
}
fn on_download_done(tag, path) { signal("pack_path", "").set(path); }
fn on_download_error(tag, message) { print("pack failed: " + message); }
```

A relative `path` names a file beside the app and an absolute one is left
alone, the way every app path resolves; directories that do not exist yet are
created on the way. The bytes land in a sibling temp file and are renamed into
place only once the body has finished and the checksum has verified, so the
destination never holds a half file and a failed transfer never replaces what
was already there.

Progress is reported at most ten times a second per tag, so a handler that
writes a signal cannot become the transfer's bottleneck, and once more with the
final count just before the done handler runs.

A reply that is not 2xx fails the download, which is where this parts company
with `fetch`: `fetch` hands a script whatever came back so it can branch on the
status, and a download that did not get the file has nothing to write.

Redirects are followed, up to ten in a chain, and an `https` URL is verified
against the bundled web-PKI roots.

Three settings, all optional:

| Key | Default | Meaning |
| --- | --- | --- |
| `timeout_ms` | none | How long the transfer waits on a server that has gone quiet: for name resolution, for the connection, for the response headers, and then for each next piece of the body. A body that keeps arriving is never cut off, however long it takes; one that stops for longer than this fails the download. Unset, a stalled server is waited on forever. |
| `max_bytes` | none | The largest body accepted. Anything past it fails and writes nothing. |
| `max_concurrent` | `4` | How many transfers run at once, from 1 to 64. A call past the limit fails. |

```toml
[dependencies]
lumen-download = { bundled = true, config = { timeout_ms = 15000, max_concurrent = 2 } }
```

One tag means one download: a call naming a tag that is already downloading is
refused rather than replacing the transfer under way, because both would report
under the same key. A call with no tag has nowhere to report, so it answers
false and prints one `lumen-download:` line on stderr.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](../guides/packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

## Processes

These functions come from the `lumen-process` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-process = { bundled = true }
```

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `process::start(cmd, args, tag, opts)` | `bool` | Start `cmd` with the argument list `args`, reporting under `tag`. `true` once the program is running. |
| `process::stop(tag)` | `bool` | End the program running under `tag`. `false` when nothing runs under it. |

A `cmd` with a path separator in it names a program the app
ships and resolves against the app directory; a bare `cmd` is looked up on
`PATH`. The child reads end of file from its input and has both its output
streams captured.

`opts` is a map of the options below; a key left out takes its default, so
`#{}` is every default. A key the options do not have, or a value of the
wrong type, raises before anything starts:

```rhai
process::start("java", ["-jar", "game.jar"], "game", #{ cwd: "instances/a" });
```

| Field | Type | Default | Behaviour |
| --- | --- | --- | --- |
| `cwd` | string | `""` | The directory the child starts in, relative to the app directory. Empty is the app directory. |
| `env` | map of strings | empty | Variables laid over the environment the child inherits. |
| `end_at_exit` | bool | `false` | End the child when the app exits. |

The call answers as soon as the program is running. Everything after that
arrives as an event carrying the tag, so one handler serves several children:

| Event | Fallback handler |
| --- | --- |
| `process_stdout` | `on_process_stdout(tag, line)` |
| `process_stderr` | `on_process_stderr(tag, line)` |
| `process_exit` | `on_process_exit(tag, code)` |

A per-child `on("process_exit", tag, handler)` registration wins over the
fallback, like every other event.

`process_exit` is always the last event for a tag. Its `code` is the program's
own exit code, 128 plus the signal that killed it on Unix, or `-1` for an
ending the system does not describe.

A line is what the program wrote before a newline; the newline is not part of
it, and the last stretch before the program ends is a line even without one.
Bytes that are not utf-8 are replaced, and a line longer than 64 KiB arrives in
pieces. There is one handler call per line, so a chatty program calls the
handler a lot.

A program that cannot start answers `false` and prints one `lumen-process:`
line on stderr. It fires no event at all, because the tag never named a running
program, so branch on the value the call gave back rather than waiting for an
exit that never comes.

`process::stop(tag)` ends the program running under `tag`: `SIGTERM` first on
Linux and macOS, then a kill if it is still running two seconds later, and an
immediate end on Windows. The programs it started end with it, on every
platform. It answers `false` when nothing runs under the tag, ends every
child when several share it, and the child's `process_exit` still arrives as
the last event. A child started with `end_at_exit` is ended the same way when
the app closes; the app waits for it before it exits. An app that is killed
rather than closed leaves its children running.

There is no way to write to a child's input.

On Windows nothing loads a module beside the executable, so this surface
exists only in a
[`lumenc package --static`](../guides/packaging.md#one-self-contained-executable)
build, which compiles the declared modules in.

## Storage

These functions come from the `lumen-storage` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-storage = { bundled = true }
```

Text values under text keys: a local set, kept in `storage.json` under the
app's data directory and written on every change, and a session set that
lasts as long as the run. The same functions are the browser's local and
session storage in a web build; see the
[candela reference](scripting-candela.md#storage).

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `storage::get_item(key)` | `string` or `()` | The stored text, or `()`. |
| `storage::set_item(key, value)` | `bool` | Store text; false when the data directory cannot be written. |
| `storage::remove_item(key)` |  | Remove a key. |
| `storage::keys()` | `array` | Every key. |
| `storage::clear()` |  | Remove everything. |
| `storage::session_get_item(key)` | `string` or `()` | As `get_item`, in the session set. |
| `storage::session_set_item(key, value)` | `bool` | As `set_item`, in the session set. |
| `storage::session_remove_item(key)` |  | As `remove_item`, in the session set. |
| `storage::session_keys()` | `array` | As `keys`, in the session set. |
| `storage::session_clear()` |  | As `clear`, in the session set. |

## Cookies

These functions come from the `lumen-cookie` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-cookie = { bundled = true }
```

A cookie jar that the app's own [`http` and `fetch`](#networking) requests
use: a request carries the cookies whose domain, path and `secure` flag match
its URL, and a `Set-Cookie` in the reply goes into the jar, with the rules a
browser applies. A request with `credentials` set to `"omit"` neither sends
nor keeps any. Cookies with an expiry are kept in `cookies.json` under the
app's data directory; the rest last as long as the run. The options and what
they mean are in the [candela reference](scripting-candela.md#cookies).

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `cookie::get(name)` | `string` or `()` | The cookie's value, or `()`. A cookie a server set `HttpOnly` is not found. |
| `cookie::set(name, value, options)` | `bool` | Set a cookie. Without a `domain` option it stays in the jar for the script alone. |
| `cookie::remove(name, options)` |  | Remove a cookie. `path` and `domain` have to be the ones it was set with. |
| `cookie::keys()` | `array` | The name of every cookie the script can see. |

## WebSockets

These functions come from the `lumen-websocket` runtime module and exist only
when the app declares it under `[dependencies]` in `lumen.toml`:

```toml
[dependencies]
lumen-websocket = { bundled = true }
```

A connection is named by a key the script picks, and every call and event
takes it first. Each connection runs on a thread of its own, `wss` is TLS
against the Mozilla root certificates, and an `http` or `https` URL connects
to the same place over `ws` or `wss`.

| Builtin | Returns | Behaviour |
| --- | --- | --- |
| `ws::open(key, url)` | `bool` | Open a connection; false when one under `key` is still connecting or open. |
| `ws::send(key, text)` | `bool` | Send a text frame; false when the connection is not open. |
| `ws::close(key, code, reason)` | `bool` | Close the connection with code 1000, or 3000 to 4999; false when there is none. Another code raises. |
| `ws::state(key)` | `string` | `connecting`, `open`, `closing` or `closed`, which is also the answer for a key with no connection. |

| Event | Arguments |
| --- | --- |
| `on_ws_open` | `key` |
| `on_ws_message` | `key`, `text` |
| `on_ws_close` | `key`, `code`, `reason`, `clean` |
| `on_ws_error` | `key`, `message` |

Binary frames are not delivered; one arriving raises `on_ws_error`. A
connection that fails raises `on_ws_error`, then `on_ws_close` with code 1006
and `clean` false.

## Browser-only modules

`lumen-js`, `lumen-browser` and `lumen-svg` reach the page's JavaScript, the
browser's windows, and an `<svg-view>` element a browser draws. They do
something only in a web build, which runs candela; see the
[candela reference](scripting-candela.md#javascript). Declared for a
desktop app, their functions exist under this host too, with the same names
and parameters, and each raises `<namespace>::<function> runs only in a
browser`. An `<svg-view>` shows the content its markup gives it.

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
