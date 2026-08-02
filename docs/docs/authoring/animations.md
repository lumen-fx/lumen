# Animations + transitions

Lumen ships a small but composable animation surface. Three layers
stack:

1. **CSS `transition:`** - declarative, ties to class flips.
2. **`Transition<T>` primitive** - programmatic, generic over
   any `Lerp`-able type. Cargo crate authors compose against this.
3. **Bespoke hover / press tweens** - `HoverTint` / `PressTween` keep
   their own state machines because bidirectional snap-on-state-flip
   doesn't fit a one-shot transition.

The implementation lives in
[`lumen/primitives/src/transition.rs`](https://github.com/lumen-fx/lumen/blob/main/lumen/primitives/src/transition.rs)
and the CSS apply pass in
[`lumenc/src/parser_css.rs`](https://github.com/lumen-fx/lumen/blob/main/lumenc/src/parser_css.rs)
(`parse_transition`).

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
    let card = lumen::node_get_by_id("info-card");
    lumen::event_on(card, "pointerenter", "fade_in");
}

fn fade_in(ev) {
    lumen::node_class_add(lumen::event_target(ev), "visible");
}
```

> **What is wired today.** `opacity`, `background-color` (aliases
> `background` and `bg`), `color` (alias `text-color`), and
> `border-color` all drive a real tween on a class flip. `radius` (alias
> `border-radius`) and every layout property (`width`, `height`,
> `padding`, margins, ...) parse successfully but are silently dropped;
> layout properties are excluded on purpose, since tweening them would
> re-run layout every frame.

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

Defaults:

- `easing` defaults to `ease-out` when omitted. CSS' real default is
  `ease` (~ `cubic-bezier(0.25, 0.1, 0.25, 1)`); `ease-out` is the
  closest named curve and avoids the bezier sample for the common case.

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

The five `Easing` variants:

| Variant | Curve | When to use |
|---|---|---|
| `Linear` | `f(t) = t` | Tests; rare in UI. |
| `EaseIn` | `f(t) = t^3` | Slow start; departures from rest. |
| `EaseOut` | `f(t) = 1 - (1 - t)^3` | **Default.** Fast start, soft settle. Matches Cocoa AppKit / Material 3 short-transition feel. |
| `EaseInOut` | symmetric S-curve | Both endpoints feel slow; useful for back-and-forth motion. |
| `CubicBezier(p1x, p1y, p2x, p2y)` | CSS `cubic-bezier(...)` | Custom curves; anchors are implicit `(0, 0)` and `(1, 1)`. |

Sampled values at `t = 0.5`:

| Easing | `f(0.5)` | Visual feel |
|---|---|---|
| `Linear` | 0.500 | constant speed |
| `EaseIn` | 0.125 | back-loaded - most travel in second half |
| `EaseOut` | 0.875 | front-loaded - most travel in first half |
| `EaseInOut` | 0.500 | slow ends, fast middle |
| `cubic-bezier(0.25, 0.1, 0.25, 1)` (CSS `ease`) | ~0.802 | front-loaded, soft-out |

CSS `cubic-bezier` strings reach the same sampler:

```css
.card { transition: opacity 200ms cubic-bezier(0.25, 0.1, 0.25, 1.0); }
```

Sampling is Newton-Raphson (3 iterations) with a bisection fallback,
matching the CSS Transitions Level 1 sampling note. Stable for all
well-conditioned curves (the standard CSS keyword curves are
well-conditioned by construction).

## Bespoke hover / press tweens

`Interaction.hover_tint` and `Interaction.press_tint` keep their
`HoverTween` / `PressTween` state machines (not `Transition<T>`)
because their bidirectional snap-on-state-flip semantics - start
tweening into the hover color, then snap back to base on hover-out
mid-tween - don't fit the one-shot lifecycle of a `Transition`.

Authoring still uses the simple shorthand:

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

The pseudo-class apply pass routes `:hover` -> `hover-bg`, `:active` ->
`press-bg`. The blend's duration and easing are authorable: a
`transition: bg <duration> <easing>` declared on the base rule (not on
`:hover` / `:active`) sets both, the same declaration that would drive
a `background-color` class-flip tween anywhere else on the page. Leave
it off and the hover blend falls back to a built-in 120ms ease-out; the
press blend falls back to a built-in 60ms ease-out.

> **HoverTint over gradients.** The hover tween path only animates
> solid fills. Gradient `bg:` skips the tween - the gradient pops to
> its hover counterpart instantly. Tracked in TODO ("HoverTint over
> gradients").

## Switch thumb slide

`<switch>` glides its thumb between the off and on ends on every
`checked` flip, using the same bespoke-tween mechanism as the hover /
press blends above rather than a dedicated animation property:

```css
switch { transition: bg 140ms ease-out; }
```

A `transition: bg <duration> <easing>` declared on the `switch`
element itself sets the slide's duration and easing - the shipped
skins all declare this. Leave it off and the slide falls back to a
built-in 140ms ease-out, so an unstyled switch looks and moves exactly
as it always has.

This is worth calling out plainly: `bg` here does not mean the track's
own background color tweens. The track's checked / unchecked fill swap
is instant, a snap, not a tween; only the thumb's position glides.
`transition: bg` is reused as the switch's one authorable timing slot
because it is the same slot that already carries hover / press tint
timing everywhere else in the framework, not because a background color
is actually changing. If you are looking for "how fast does the thumb
move," this is the property, even though its name says otherwise.

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
The same pattern works today for `opacity`, `background-color` (`bg`),
`color` (`text-color`), and `border-color`; the property list is
expected to grow over time.

## Why no `@keyframes` yet?

Two reasons:

1. **Limited demand.** CSS `transition:` covers the 80% case
   (class-flip-driven property tweens). Keyframed multi-stop
   animations are most useful for hero / loading scenes; Lumen's
   pre-1.0 surface biases to forms / dashboards.

2. **It's additive.** A future `@keyframes` parser would build atop
   `Transition<T>` by chaining tweens (or compiling to a single
   composite Bezier). Nothing in the current design precludes the
   addition.

If you need keyframed motion today, drive `Transition<T>` from your
own plugin: spawn the next segment when the previous one's `done()`
returns true.
