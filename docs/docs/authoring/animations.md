# Animations + transitions

Motion in Lumen comes from three places, in order of how often you reach
for them:

1. **CSS `transition:`** - declarative, driven by a class or state flip.
   This is what an app author writes.
2. **Built-in hover, press, and switch tweens** - always on, with their
   timing authorable through the same `transition:` declaration.
3. **The `Transition<T>` primitive** - a programmatic tween for a Rust
   plugin that animates something the CSS layer does not cover.

## CSS `transition:`

The CSS Transitions Level 1 shorthand. Class flips that change a
transitioned property tween the value over the declared duration
instead of snapping.

```css
.card {
  opacity: 0.5;
  transition: opacity 200ms ease-out;
}

.card.visible {
  opacity: 1.0;
}
```

```xml
<column class="card" id="info-card">
  <label text="Hover me to fade in" />
</column>
```

```candela
fn on_ready() {
    get_by_id("info-card").on("pointerenter", "fade_in");
}

fn fade_in(ev) {
    event(ev).target().class_add("visible");
}
```

Four properties tween: `opacity`, `background-color` (aliases
`background` and `bg`), `color` (alias `text-color`), and `border-color`.
Anything else in a `transition:` list, including `radius` and every layout
property, parses and is dropped with a warning. Layout properties are
excluded deliberately: tweening one would re-run layout every frame.

### Grammar

The CSS shorthand:

```text
transition := entry ("," entry)*
entry      := <property> <duration> [<easing>]
property   := "opacity" | "background-color" | "background" | "bg"
            | "color" | "text-color" | "border-color"
duration   := Nms | Ns
easing     := "linear" | "ease" | "ease-in" | "ease-out"
            | "ease-in-out" | "cubic-bezier(p1x, p1y, p2x, p2y)"
```

`easing` defaults to `ease-out` when omitted, where CSS itself defaults to
`ease`. `ease-out` is the closest named curve and skips a bezier sample in
the common case; write `ease` explicitly for the CSS default.

### Stacked transitions

Comma-separate multiple properties:

```css
.tile {
  transition: opacity 200ms ease-out,
              bg      300ms ease;
}
```

Each entry is independent; only the first matching property's spec
applies (CSS-style: first declaration wins). A property this grammar
does not recognize, or a layout property such as `width`, still parses
but is silently dropped, so a typo or an animated layout property fails
quietly rather than with a parse error.

## Transition primitive

For programmatic tweens, `lumen_primitives::Transition<T>` is the
generic one-shot tween component. It works against any
`Lerp + Send + Sync` type; the crate ships `Lerp` impls for `f32` and
`Color`.

```rust
use lumen_primitives::{Easing, Lerp, Transition};
use std::time::{Duration, Instant};

let t = Transition::<f32>::new(
    0.0,                          // from
    1.0,                          // to
    Duration::from_millis(200),
    Easing::EaseOut,
);

let v    = t.sample(Instant::now());    // current eased value
let done = t.done(Instant::now());      // true once elapsed >= duration
```

Or via `From<(T, T, Duration)>` for the common linear case:

```rust
let t: Transition<f32> = (0.0, 10.0, Duration::from_millis(100)).into();
// -> Easing::Linear
```

### Plugging into the ECS

`Transition<T>` is a `Component`. Each CSS-wired property has its own
thin wrapper component and sampler system, all registered by
`TransitionPlugin`: `OpacityTransition` /
`step_opacity_transitions` for `opacity`, `BackgroundTransition` /
`step_background_transitions` for `background-color` (`background`,
`bg`), `TextColorTransition` / `step_text_color_transitions` for
`color` (`text-color`), and `BorderColorTransition` /
`step_border_color_transitions` for `border-color`. You do not need to
write a sampler for any of these four; they run automatically whenever
a class flip changes the resolved value on an entity that declares a
matching `transition:` property.

For your own tween targets, register a sampler system:

```rust
use bevy_ecs::prelude::*;
use lumen_primitives::Transition;
use lumen_core::components::Color;
use std::time::Instant;

#[derive(Component)]
struct MyColorTween(Transition<Color>);

fn step_color_tweens(
    mut commands: Commands,
    mut q: Query<(Entity, &MyColorTween, &mut MyColorTarget)>,
) {
    let now = Instant::now();
    for (e, tween, mut target) in &mut q {
        target.color = tween.0.sample(now);
        if tween.0.done(now) {
            commands.entity(e).remove::<MyColorTween>();
        }
    }
}
```

Add the system in `TickStage::Systems` from your plugin's `build`
method (see [Plugin author guide](../reference/plugins.md)).

## Easing curves

Five curves, the CSS Transitions Level 1 set:

| Keyword | Shape | When to use |
|---|---|---|
| `linear` | `f(t) = t`, constant speed | Tests; rare in UI. |
| `ease-in` | `f(t) = t^3`, back-loaded | Slow start; departures from rest. |
| `ease-out` | `f(t) = 1 - (1 - t)^3`, front-loaded | The default. Fast start, soft settle, matching the Cocoa and Material short-transition feel. |
| `ease-in-out` | symmetric S-curve | Slow at both ends; useful for back-and-forth motion. |
| `cubic-bezier(p1x, p1y, p2x, p2y)` | whatever you write | Custom curves. The anchors are an implicit `(0, 0)` and `(1, 1)`. |

`ease` is `cubic-bezier(0.25, 0.1, 0.25, 1)`, front-loaded with a soft
finish. Writing the bezier out reaches the same sampler:

```css
.card { transition: opacity 200ms cubic-bezier(0.25, 0.1, 0.25, 1.0); }
```

## Hover and press tweens

Hover and press blends run on their own, without a `transition:`
declaration, because they have to reverse mid-flight: the fill tweens
toward the hover color and snaps back to the base when the pointer leaves,
which a one-shot tween cannot express.

Author them with the shorthand attributes:

```xml
<button hover-bg="#3344ff" press-bg="#1122dd">Click</button>
```

Or via CSS:

```css
button {
  bg: #2233ee;
  transition: bg 130ms ease;
}
button:hover  { bg: #3344ff; }
button:active { bg: #1122dd; }
```

`:hover { bg }` and `hover-bg` are the same slot, as are `:active { bg }`
and `press-bg`. The blend's duration and easing are authorable: a
`transition: bg <duration> <easing>` on the base rule (not on `:hover` or
`:active`) sets both, the same declaration that drives a
`background-color` class-flip tween anywhere else. Leave it off and each
blend falls back to a short built-in ease-out, with the press blend
quicker than the hover one.

The blend interpolates two solid colors, which is also all a state rule
accepts: `bg` under `:hover` or `:active` takes a color, and a gradient
there is a parse error. To swap between two gradients, flip a class from
script instead.

## Switch thumb slide

`<switch>` glides its thumb between the off and on ends on every
`checked` flip, using the same bespoke-tween mechanism as the hover /
press blends above rather than a dedicated animation property:

```css
switch { transition: bg 140ms ease-out; }
```

A `transition: bg <duration> <easing>` on the `switch` element sets the
slide's duration and easing; the shipped skins all declare it. Leave it
off and the slide falls back to a built-in ease-out.

`bg` here does not mean the track's background color tweens. The track's
checked and unchecked fills swap instantly; only the thumb glides.
`transition: bg` is reused as the switch's one authorable timing slot
because it already carries hover and press timing everywhere else. If you
are looking for how fast the thumb moves, this is the property, despite
the name.

## A worked example - fade-in on appear

```css
.toast {
  opacity: 0.0;
  bg: #1a1d24;
  text-color: #f5f5f7;
  padding: 12 16 12 16;
  radius: 8;
  transition: opacity 300ms ease-out;
}

.toast.visible {
  opacity: 1.0;
}
```

```xml
<overlay class="toast-anchor">
  <column id="toast" class="toast">
    <label text="Saved." />
  </column>
</overlay>
```

```candela
fn on_start() {
    lumen::on("click", "save", "show_toast");
}

fn show_toast(id) {
    lumen::set_class("toast", "toast visible");
    lumen::set_timeout("hide-toast", 2000);
}

fn on_timer(name) {
    if name == "hide-toast" {
        lumen::set_class("toast", "toast");    // .visible removed -> fade-out
    }
}
```

The class flip is the trigger; the CSS `transition:` is the policy.

## Limits

`@keyframes` does not exist, and writing one is a parse error rather than
a skip. For keyframed motion, drive `Transition<T>` from a plugin and
start the next segment when the previous one's `done()` returns true.

Transitions run on entry, not on removal. An element that appears and
declares `transition: opacity` fades in; hiding or closing one is instant,
the same limit CSS has without JavaScript.
