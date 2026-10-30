# `Renderer` calls name the window they are for

This affects Rust code that implements `lumen_core::traits::Renderer`, drives
a renderer itself, or reads the render world's retained scene. One renderer
now holds a target per window, keyed by the window's main-world entity.

- `attach`, `resize`, `wants_present`, and `present` take the window entity
  first, and the trait gained `detach`, `detach_all`, `targets`, and
  `set_present_mode`.
- The offscreen constructors take the window too:
  `WgpuRenderer::new_offscreen(window, w, h)` and
  `CpuRenderer::new_offscreen(window, w, h)`. `read_rgba8` and `layer_stats`
  take the window whose frame to read; pass `primary_entity(&app.world)` for
  the main one.
- `lumen_paint`'s `wants_frame`, `capture_requested`, `clear_color`,
  `paint_frame`, `answer_capture`, and `scene_has_damage` take the window they
  paint. `walk_retained_scene` is gone.
- The `RetainedScene` and `PreviousScene` render-world resources are gone.
  Each window's tree and the tree its last frame painted are the `scene` and
  `previous` fields of its `RenderWindow` component; `render_window(&render,
  window)` finds it.
- `SurfaceFrame` carries the `window` it was captured from.

```rust
renderer.attach(FrameTarget::Offscreen { width, height })?;
renderer.attach(window, FrameTarget::Offscreen { width, height })?;

let rgba = renderer.read_rgba8();
let rgba = renderer.read_rgba8(primary_entity(&app.world));
```
