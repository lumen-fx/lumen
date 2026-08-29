# Animation

Lumen animates through CSS transitions. A transition says how long a value
takes to reach its new setting, and the runtime tweens it from wherever it is:

```css
button { transition: bg 130ms ease; }
.panel  { transition: opacity 150ms ease-out; }
```

## Writing a transition

Each entry is a property, a duration, and an optional easing:

```css
transition: opacity 200ms ease-out, border-color 120ms linear;
```

The property comes first and a duration is required. A duration needs its
unit, `ms` or `s`; `0` alone is not a duration, `0ms` is. Leave the easing out
and you get `ease`.

Easings: `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, and
`cubic-bezier(a, b, c, d)`.

Four properties animate:

- `opacity`
- `bg`, also spelled `background-color` or `background`
- `text-color`, also spelled `color`
- `border-color`

`all` covers all four:

```css
.card { transition: all 150ms ease-out; }
```

Naming anything else drops that entry with a warning and the value snaps
instead. Size, position, spacing, and radius do not animate.

Longhands work too, and cycle their lists across the properties the way CSS
does:

```css
transition-property: opacity, bg, border-color;
transition-duration: 100ms, 200ms;
transition-timing-function: ease-out;
```

A `transition` shorthand on the same element replaces the longhands entirely.

A transition starts on the tick its value changes. Delays are not supported:
`transition-delay`, and a second duration in a `transition` entry, are warned
about and ignored.

There is no `@keyframes`. Such a block is skipped with a warning and the rest
of the stylesheet applies.

## Starting one

A transition runs whenever the value it watches is recomputed to something new.

**A state change.** Pseudo-class rules are the most common trigger, and need no
script at all:

```css
.chip           { bg: #2a2f3a; transition: bg 140ms ease-out; }
.chip:hover     { bg: #37405020; }
.chip:disabled  { opacity: 0.4; }
```

**A class change from a script.** Add, remove, or toggle a class on a node and
the cascade re-runs for it:

```rhai
fn on_click(id) {
    get_by_id("panel").toggle_class("open");
}
```

`add_class`, `remove_class`, and `toggle_class` are the names in every host;
Lua calls them with the colon form, `node:toggle_class(name)`. candela also
reaches the same effect on a raw handle through `lumen::node_class_toggle(n,
name)` and its siblings. See [scripting](scripting.md).

Replacing the whole list with `set_class` triggers the same re-run, whether you
call it on a node or by element id.

**An inline style change from a script.** `set_style` writes the element's own
style layer, which sits above every rule, and tweens the same way a class
change does:

```rhai
fn on_click(id) {
    get_by_id("panel").set_style("bg", "#37405020");
}
```

Setting `disabled`, `class`, or `id` from a script re-runs the cascade. Other
attributes do not, so writing a colour through `set_attr` is a snap; write it
with `set_style` to tween it.

**Appearing.** An element mounted by `<if>` or `<for>` fades in if it has an
`opacity` transition:

```css
.dropdown-panel { transition: opacity 120ms ease; }
```

This is entry only, and opacity only. The tree that exists at startup does not
fade in, nodes a script spawns do not fade in, and a transition on `bg` does
not fade a background in.

Nothing animates on the way out. Hiding or removing an element is immediate, in
the same way CSS alone cannot animate a removal.

## Interrupting one

Change the target mid-flight and the new tween starts from the value on screen
rather than jumping, and runs for its full duration. Setting a value to what it
already is does nothing.

Transitions do not advance while the window is unfocused or hidden; values
settle immediately instead.

## Built-in motion

Some widgets animate on their own, and take their timing from the `transition`
you write on them.

**Hover and press tints.** Buttons and other interactive elements fade their
background toward the hover and press colours, and fade back when the pointer
leaves. This is the one motion in Lumen that reverses. Give the element a
`transition: bg <duration> <easing>` to set the pace:

```css
button { bg: var(--lumen-surface); transition: bg 130ms ease; }
```

Keyboard focus tints the same way hover does, so a focused control reads as
active.

**Switch thumb.** A `switch` slides its thumb between off and on, timed by the
`transition: bg` on the switch.

**Scrollbars** fade out after a pause; tune it with `scrollbar-fade-delay` and
`scrollbar-fade-duration`.

**Indeterminate progress.** A `progress` with no value sweeps continuously.

**Text carets** blink at the rate set by `caret-blink`.

These properties are listed in the [CSS reference](../reference/css.md).

## Frame callbacks

A transition moves a property from one setting to another. Something that
moves continuously - a clock hand, a game loop, a chart that redraws as data
arrives - needs a callback per frame instead, and `request_frame()` is it:

```rhai
fn on_ready() { request_frame(); }

fn on_frame(dt) {
    signals.angle.set(signals.angle.get() + dt);
    request_frame();
}
```

One request buys one callback. A handler that wants to keep going asks again;
one that is finished stops asking, and the app parks with no frames running.
That is what makes an idle animation cost nothing.

`dt` is the seconds since the previous callback, so multiply anything that
moves by it and the speed stays the same on a fast machine and a slow one. It
is zero on the frame that starts a loop, because no time has passed yet, and
it is capped, so a stalled tick slows the animation down rather than jumping
it forward.

Reach for this rather than `set_interval`: a timer fires on a wall-clock
schedule that has nothing to do with when the app draws, so an animation
driven by one stutters against the frames it is trying to land on.

A frame callback pairs naturally with a [`<canvas>`](../reference/tags.md#canvas),
which is where a script draws something CSS cannot describe:

```lmn
<root>
  <canvas id="dial" width="120" height="120" />
  <script src="main.rhai" />
</root>
```

## What does not animate

- A gradient background. Gradients snap; only a solid `bg` tweens.
- A border whose width changes at the same time as its colour.
- The selected state of a `tabs` strip, a `toggle`, and a slider thumb, which
  set their own colours and snap.

CSS transitions do not take a script API for tweening a value directly; a
frame callback is what a script drives motion from.

`@media (prefers-reduced-motion: reduce)` parses but never matches yet, so give
people their own way to turn off anything heavily animated. See
[accessibility](accessibility.md).

The built-in skins already set transitions for every control, with timings that
match each platform's conventions, so a themed app moves correctly before you
write any of this. See [styling](styling.md).
