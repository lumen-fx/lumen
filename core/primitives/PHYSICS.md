# Per-OS physics constants

Each constant in `src/physics.rs` is selected via `cfg!(target_os = ...)` at
compile time. This document records what each value is, where it came
from, and how it is consumed.

## `INERTIA_DECAY: f32`

Exponential decay rate (1/s) applied to `ScrollVelocity` each frame by the
`integrate_scroll` system:

```rust
let decay = (-INERTIA_DECAY * dt).exp();
velocity *= decay;
```

| Target          | Value | Source                                                                                  |
|-----------------|-------|-----------------------------------------------------------------------------------------|
| macOS           | 6.0   | NSScrollView momentum phase ends ~700 ms after a fling: `e^(-6.0 * 0.7) ~ 0.015`.       |
| Windows         | 8.0   | `IInertiaProcessor.SetDesiredDeceleration` defaults to ~0.001 dip/ms^2 which decays      |
|                 |       | to near-zero in ~500 ms for typical flick velocities.                                   |
| Linux / other   | 8.0   | Matches Windows feel; X11/Wayland have no canonical inertia spec.                       |

## `RUBBER_BAND_STIFFNESS: f32`

Stiffness coefficient pulling `ScrollOffset` back toward content bounds
when the user has scrolled past the edge. Not yet wired in the
integrator (Phase 3 stops at decay).

| Target | Value | Source                                                                              |
|--------|-------|-------------------------------------------------------------------------------------|
| macOS  | 0.55  | NSScrollView elasticity (see `UIScrollView.bounces`). Pull-distance ~ log scale.    |
| other  | 0.0   | Windows / Linux clamp at the edge rather than rubber-banding.                       |

## `LINE_HEIGHT_PX: f32`

Logical pixels per scroll-wheel line for `MouseScrollDelta::LineDelta`
normalization. Equal to 32 across all targets.

Source: GTK 3.x `gtk-scroll-lines` defaults to 3 lines/notch; combined
with a typical 11pt UI font (~14.5 px line height) ~ 43 px per notch;
we round down to 32 to feel responsive without overshooting on
high-DPI displays. Apps tune per-container via `Scroll::sensitivity`.

## How to revise

When changing any constant:

1. Update the table above with new value + citation.
2. Update the corresponding `if cfg!(target_os = ...)` ladder in
   `src/physics.rs`.
3. Run `cargo test -p lumen-primitives` to ensure nothing in the
   integration relies on a specific decay rate.
