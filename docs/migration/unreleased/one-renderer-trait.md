# Render backends implement one `Renderer` trait for windows and offscreen images

This affects Rust code that implements a render backend, calls one directly,
or reads the render world's damage list: embedders, custom window backends,
and tests that drive `lumen-render-wgpu` or `lumen-render-cpu` by hand.

`lumen_core::traits::Renderer` is now the whole renderer seam. It absorbs
`SurfaceRenderer` and `OffscreenRenderer`, which are gone, and a window and an
offscreen image are two kinds of `FrameTarget` the same renderer attaches to.
`SurfaceError` is renamed `RenderError`. `RenderBackend` carries one
constructor, `renderer: fn() -> Box<dyn Renderer>`, in place of `surface` and
`offscreen`. `lumen_core::render_backend::install_offscreen` puts an attached
renderer into an app and drives it each frame.

Each backend has one renderer type: `WgpuSurfaceRenderer` folds into
`WgpuRenderer` and `CpuSurfaceRenderer` into `CpuRenderer`. `CpuRenderer::new`
now builds a detached renderer; `CpuRenderer::new_offscreen(width, height)`
builds one attached to an image. `WgpuRenderer::new_offscreen` returns a
`RenderError`, and `WgpuRenderer::adapter_info` and `WgpuRenderer::size`
return `Option`, `None` while detached.

The `FrameDamage` resource and `lumen_paint::damage_union` are removed, and
`lumen_paint::diff_retained_scenes` returns whether the tree changed instead
of filling a damage list.

Before:

```rust
use lumen_core::render_backend::RenderBackend;
use lumen_core::traits::{SurfaceError, SurfaceRenderer};

let renderer: Box<dyn SurfaceRenderer> = (backend.surface)();
renderer.attach(window)?;

let offscreen = (backend.offscreen)(800, 600)?;
offscreen.install(&mut app);

let mut damage = FrameDamage::default();
diff_retained_scenes(prev, curr, viewport, &mut damage);
let changed = !damage.is_empty();
```

After:

```rust
use lumen_core::render_backend::install_offscreen;
use lumen_core::traits::{FrameTarget, RenderError, Renderer};

let mut renderer: Box<dyn Renderer> = (backend.renderer)();
renderer.attach(FrameTarget::Window(window))?;

let mut offscreen = (backend.renderer)();
offscreen.attach(FrameTarget::Offscreen { width: 800, height: 600 })?;
install_offscreen(&mut app, offscreen);

let changed = diff_retained_scenes(prev, curr, viewport);
```
