# Reactivity

Signals are the one piece of shared state between markup and script. A signal
is a named value; markup binds to the name, a script reads and writes the name,
and neither side holds a reference to the other. Change the value and every
place that reads it updates.

There is no separate state object per language. A Rhai, Lua, or candela script
in the same app writes into the same store the markup reads from.

## Binding an element to a signal

`bind-*` attributes wire an element's state to a signal:

```html
<label bind-text="$status" />
<input bind-text="$who" placeholder="Your name" />
<toggle bind-checked="$dark" />
<slider bind-value="$volume" min="0" max="100" />
<button bind-disabled="$busy" text="Save" />
<scroll bind-scroll="$offset"> ... </scroll>
```

The leading `$` marks the value as a signal name. It is optional, and
`bind-text="status"` means the same thing, but writing it makes signal
references greppable and `lumenc lint --signals` suggests it.

Bindings on controls run both ways. Typing in the `<input>` above updates
`who`; writing `who` from a script updates the field. The same holds for
`<toggle>`, `<switch>`, `<checkbox>`, `<slider>`, and `<scroll>`. `bind-text`
on a `<label>` and `bind-disabled` anywhere are display-only, because there is
nothing for the user to edit.

A control's own attributes seed the signal when it has no value yet, so
`<slider bind-value="$volume" value="42" />` starts at 42 without a script.

## Interpolation

Braces splice a signal into text and into string attribute values:

```html
<label text="Signed in as {$user}" />
<tile class="badge tier-{$plan}" />
```

The value is read once, when the tree the element belongs to is built, and the
string keeps it from then on. On the web that is the state the page was
rendered with, so the document a visitor is sent already reads `Signed in as
Ada` and carries `class="badge tier-gold"`, and the runtime that adopts the
page computes the same strings and leaves them alone.

A signal set after the tree was built does not reach a placeholder in it: the
text keeps the braces the author wrote, which is also what a misspelled name
looks like. `bind-text` is the form that follows a signal for as long as the
element is there, so a value a script keeps writing wants that:

```html
<label bind-text="$user" />
```

A `<for>` row is built per record, so it reads later and reads more: each row
resolves `{row.field}` against the record it is built for and its `{$signal}`
placeholders against the signals at that moment. An `<if>` body belongs to the
tree it arrived in, so its placeholders hold what that tree was built with,
whenever the branch is taken.

## Lists

`<for>` repeats its body once per row of an array signal:

```html
<for each="$cards" key="id" gap="10">
  <column class="card">
    <label class="title" text="{row.title}" />
    <label class="tag" text="{row.tag}" />
  </column>
</for>
```

- `each` names the array signal.
- `key` names the field that identifies a row.
- `{row.field}` reads a field of the current row; `{$index}` is its position.
- Any other attribute on `<for>` styles the generated container, which defaults
  to a column.

Appending rows spawns only the new ones, and dropping rows from the end
despawns only those, so existing rows keep their focus and scroll state.
Reordering the array or editing the middle of it rebuilds the block.

For a long list, add `virtualized="true"` and `row-height="56"` and only the
rows in the visible window are built.

A script fills the array by name. Every host exposes an array-signal handle for
this, with `set`, `push`, `get`, and `len` on it. See
[Scripting](scripting.md).

## Conditionals

`<if>` mounts its body while a signal is truthy:

```html
<if signal="$has_results">
  <column class="results"> ... </column>
</if>
```

A signal is falsy when it is unset, empty, `"false"`, or `"0"`, and truthy
otherwise. Compare against a specific value with `eq`:

```html
<if signal="$view" eq="grid"> ... </if>
```

Two modes decide what happens on the falsy side:

- `mode="render"`, the default, despawns the body. It costs nothing while
  hidden and loses whatever state the body held.
- `mode="hide"` mounts the body once and toggles its visibility. Scroll
  positions, typed text, and focus survive the round trip.

Use `render` for large branches that are rarely shown, `hide` for panels the
user flips between. `<dialog open="$show">` is the hide form with a
full-viewport backdrop already applied, and `<tabs>` uses it for tab bodies.

## Derived values

A derived signal recomputes itself when its inputs change, which keeps
formatting out of your event handlers:

```rhai
// Rhai
fn on_start() {
    let clicks = signal("clicks", 0);
    derive("counter_label", [clicks], |n| "clicks: " + n);
}
```

Declare the name, the signals it depends on, and the function that produces the
value. Nothing calls it; a write to any dependency does. Derived values may
depend on other derived values, and a whole chain settles within the tick that
started it.

In candela the function is named rather than written inline, and each parameter
carries a declared type that has to match the kind its signal holds. Declare it
`any` when you are not sure. See
[derived signals](../reference/scripting-candela.md#derived-signals).

## How a change travels

1. Something writes a signal: a script, a user editing a bound control, or host
   code through the C API.
2. The write lands in the shared store.
3. Derived signals whose dependencies changed recompute.
4. Bound elements, `<for>` bodies, and `<if>` branches update.

All of that happens on one tick, so a handler that writes three signals
produces one consistent frame rather than three. Elements bound to a signal
nobody wrote are not touched, so an idle app does no work.

One value edits at a time: while an `<input>` has focus, a signal write does not
overwrite what the user is typing.

## Where to look things up

- Writing signals, timers, and handlers: [Scripting](scripting.md)
- Per-host builtin names and signatures:
  [candela](../reference/scripting-candela.md),
  [Rhai](../reference/scripting-rhai.md), [Lua](../reference/scripting-lua.md)
- `bind-*`, `each`, `key`, and `<if>` attributes:
  [Tags and attributes](../reference/tags.md)
