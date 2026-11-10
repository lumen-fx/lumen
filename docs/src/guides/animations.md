# Animation

Lumen animates through CSS, the way a browser does. A transition says how long
a value takes to reach its new setting, and the runtime tweens it from wherever
it is:

```css
button { transition: bg 130ms ease; }
.panel  { transition: opacity 150ms ease-out; }
```

A `@keyframes` animation plays a sequence of values on its own, once or in a
loop; the [keyframes section](#keyframes) below covers it.

## Writing a transition

Each entry is a property, a duration, and an optional easing:

```css
transition: opacity 200ms ease-out, border-color 120ms linear;
```

The property comes first and a duration is required. A duration needs its
unit, `ms` or `s`; `0` alone is not a duration, `0ms` is. Leave the easing out
and you get `ease`.

Easings: `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`,
`cubic-bezier(a, b, c, d)`, `steps(n)`, `step-start`, `step-end`, and
`linear(...)`, with the curves CSS defines, so a transition moves the same
on the desktop and in a browser.

These properties animate:

- `opacity`
- `bg`, also spelled `background-color` or `background`
- `text-color`, also spelled `color`
- `border-color`
- `box-shadow`, also spelled `shadow`
- `radius`, also spelled `border-radius`
- `outline-color`, the color of a focus outline
- `transform`, `translate`, `rotate` and `scale`

`all` covers every one:

```css
.card { transition: all 150ms ease-out; }
```

Naming anything else drops that entry with a warning and the value snaps
instead. Size, position, and spacing do not animate.

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

```rust
fn on_click(id: string) {
    get_by_id("panel").toggle_class("open");
}
```

`add_class`, `remove_class`, and `toggle_class` are the `Node` methods the
prelude gives you; on a raw handle the same effect is
`lumen::node_class_toggle(n, name)` and its siblings. See
[scripting](scripting.md).

Replacing the whole list with `set_class` triggers the same re-run, whether you
call it on a node or by element id.

**An inline style change from a script.** `set_style` writes the element's own
style layer, which sits above every rule, and tweens the same way a class
change does:

```rust
fn on_click(id: string) {
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

## Keyframes

A `@keyframes` block names a sequence of values, and the `animation` property
plays it on an element:

```css
@keyframes pulse {
  0%, 100% { opacity: 1; }
  50%      { opacity: 0.4; transform: scale(0.96); }
}

.badge.new { animation: pulse 1.2s ease-in-out infinite; }
```

The shorthand takes a name, a duration, an easing, a delay, an iteration
count or `infinite`, a direction (`reverse`, `alternate`), a fill mode
(`forwards` keeps the last frame after it ends, `backwards` shows the first
frame during the delay, `both` does both) and a play state (`paused`), in any
order. The longhands (`animation-delay`, `animation-fill-mode`, ...) set one at
a time. A negative delay starts part-way in.

A keyframe body is written in browser CSS, not in Lumen's shortened names:
declare `background` and `border-radius`, not `bg` and `radius`. The web target
writes the block out as you wrote it, and the desktop reads the same names.
The desktop animates opacity, colors, borders' colors, shadows, radii,
outline colors and transforms; anything else in a body runs in a browser and
is skipped on the desktop, and `lumenc check` says so with the line it is on.

An animation starts when its name lands on the element and stops when the name
goes away, so a state rule starts and stops one:

```css
.card:hover { animation: lift 300ms ease-out forwards; }
.spinner:hover { animation-play-state: paused; }
```

Adding a class from a script starts one the same way. Hiding the element stops
its animations, and showing it again starts them from the beginning.

An animated value outranks the element's rules, its state rules and its inline
style, as in CSS. An `!important` declaration outranks the animation, and a
transition that starts while it runs plays over it.

Each animation raises events on its element that a script can bind:
`animationstart`, `animationiteration`, `animationend` and `animationcancel`.
See [scripting](scripting.md#handling-events) for what they carry.

Put a block inside `@media (prefers-reduced-motion: no-preference)` and it
applies only while the system asks for no less motion: the media query follows
the OS's accessibility setting on the desktop, and the browser's in a web
build.

### What an animation costs

An animation runs on the desktop's frame loop only while it moves something you
can see. A delay, a `steps()` easing, and a paused or finished animation wake
the app at the moment something changes, or not at all, instead of drawing
every frame. An element scrolled out of view keeps its place in time and draws
nothing until it comes back. An `infinite` animation on a visible element keeps
the app drawing for as long as it shows, so stop it when it is not needed.

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

```rust
fn on_ready() { lumen::request_frame(); }

fn on_frame(dt: float) {
    let angle = signal<float>("angle");
    angle.set(angle.get() + dt);
    lumen::request_frame();
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
  <script src="main.cdl" />
</root>
```

## What does not animate

- A gradient background. Gradients switch halfway; only a solid `bg` tweens.
- A border whose width changes at the same time as its colour.
- The selected state of a `tabs` strip, a `toggle`, and a slider thumb, which
  set their own colours and snap.

A frame callback is what a script drives continuous motion from.

`@media (prefers-reduced-motion: reduce)` follows the OS setting: GNOME's
"Animations" switch on Linux, "Reduce motion" on macOS, and "Show animations in
Windows" on Windows. See [accessibility](accessibility.md).

The built-in skins already set transitions for every control, with timings that
match each platform's conventions, so a themed app moves correctly before you
write any of this. See [styling](styling.md).
