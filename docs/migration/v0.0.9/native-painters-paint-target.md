# Native painters draw through `lumen_paint::PaintTarget`

This affects Rust runtime modules and plugins that paint their own pixels
through the native-paint seam, with the `paint` feature of `lumen-module`.

The `paint` feature re-exports `lumen_paint` instead of `lumen_render_wgpu`,
and every render backend hands a painter a `lumen_paint::PaintTarget`, a
boxed `Painter`, instead of a `vello::Scene`. A painter that downcasts the
target to `vello::Scene` now finds nothing and draws nothing, and a module
that names `lumen_module::lumen_render_wgpu` no longer compiles. Draw through
the `Painter` trait instead; the same painter then draws on the GPU and on the
CPU renderer alike.

Before:

```rust
use lumen_module::lumen_render_wgpu::vello::Scene;
use lumen_module::lumen_render_wgpu::vello::kurbo::{Affine, Rect};
use lumen_module::lumen_render_wgpu::vello::peniko::{Color, Fill};

fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
    let transform = Affine::new(ctx.device_transform().coeffs);
    let Some(scene) = ctx.target_as::<Scene>() else { return };
    scene.fill(Fill::NonZero, transform, Color::WHITE, None, &Rect::new(0.0, 0.0, 8.0, 8.0));
}
```

After:

```rust
use lumen_module::lumen_paint::kurbo::{Affine, Rect};
use lumen_module::lumen_paint::peniko::{Color, Fill};
use lumen_module::lumen_paint::{PaintTarget, Shape};

fn paint(&self, ctx: &mut NativePaintCtx<'_>) {
    let transform = Affine::new(ctx.device_transform().coeffs);
    let Some(target) = ctx.target_as::<PaintTarget>() else { return };
    target.fill(
        Fill::NonZero,
        transform,
        Color::WHITE.into(),
        None,
        &Shape::Rect(Rect::new(0.0, 0.0, 8.0, 8.0)),
    );
}
```

A drawing recorded ahead of the frame goes into a `lumen_paint::Recording`,
which the painter replays with `recording.replay(&mut **target, transform)`.
