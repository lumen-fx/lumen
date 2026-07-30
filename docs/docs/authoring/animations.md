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
[`lumen/primitives/src/transition.rs`](https://github.com/lumen-ui/lumen/blob/main/lumen/primitives/src/transition.rs)
and the CSS apply pass in
[`lumenc/src/parser_css.rs`](https://github.com/lumen-ui/lumen/blob/main/lumenc/src/parser_css.rs)
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

```rhai
on("hover", "info-card", "fade_in");
fn fade_in(id) { set_class(id, "card visible"); }
```

> **Only `opacity` is wired.** Other property names parse successfully
> but get silently dropped - drivers for `bg`, `color`, and
> `border-radius` are not yet wired. Until then, treat the CSS path as
> opacity-only.

### Grammar

The CSS shorthand:

```text
transition := entry ("," entry)*
entry      := <property> <duration> [<easing>]
property   := "opacity" | ...    (v1: opacity only)
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
              bg      300ms ease;   /* parses but no-op in v1 */
}
```

Each entry is independent; only the first matching property's spec
applies (CSS-style: first declaration wins).

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

`Transition<T>` is a `Component`. The CSS opacity path uses it via
the shipped `OpacityTransition(Transition<f32>)` wrapper +
`step_opacity_transitions` system in `TransitionPlugin`.

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
}
button:hover  { bg: #3344ff; }
button:active { bg: #1122dd; }
```

The pseudo-class apply pass routes `:hover` -> `hover-bg`, `:active` ->
`press-bg`. The tween durations are baked into the `HoverTween` /
`PressTween` impl; they're not authorable via CSS today.

> **HoverTint over gradients.** The hover tween path only animates
> solid fills. Gradient `bg:` skips the tween - the gradient pops to
> its hover counterpart instantly. Tracked in TODO ("HoverTint over
> gradients").

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

```rhai
on("click", "save", "show_toast");

fn show_toast(_id) {
    set_class("toast", "toast visible");
    set_timeout("hide-toast", 2000);
}

fn on_timer(name) {
    if name == "hide-toast" {
        set_class("toast", "toast");        // .visible removed -> fade-out
    }
}
```

The class flip is the trigger; the CSS `transition:` is the policy.
Same pattern works for any opacity tween today; the property list is
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
