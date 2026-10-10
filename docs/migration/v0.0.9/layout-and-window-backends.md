# The layout engine and the window backend come from registries

This affects Rust code that installs the layout engine or runs the window
itself: embedders that assemble their own `App`, custom window or layout
backends, and tests that add `TaffyLayoutPlugin` or `WinitPlugin` by hand.
Apps, scripts, and `lumen.toml` are not affected.

`LayoutEngine` and `WindowBackend` in `lumen_core::traits` are now traits a
launch drives, not markers. `LayoutEngine::install(self: Box<Self>, app)`
adds the engine's systems; the ones that write `Transform` go in the new
`lumen_core::layout_backend::LayoutSolve` set, which a system reading this
tick's boxes orders itself after. `WindowBackend::run(self: Box<Self>, app,
options, renderers, a11y)` runs the app until the window closes and returns
`lumen_core::traits::WindowError`, which replaces `WinitError`.

Each kind has a registry: `LayoutBackends` of `LayoutBackend { name,
priority, engine }` and `WindowBackends` of `WindowBackendEntry { name,
priority, backend }`, beside `RenderBackends`. All three are
`lumen_core::backends::Backends<_>`, and a backend registers with
`lumen_core::backends::register_backend`, which replaces
`register_render_backend`. The taffy engine and the winit backend register
themselves as the `layout-taffy` and `window-winit` capabilities in the new
`Phase::Backends`, which runs before the core stack.

`TaffyLayout` and `WinitWindow` are gone; `TaffyLayoutPlugin` is the taffy
engine and `WinitBackend` the winit one. `lumen_window_winit::run` is no
longer public. `RedrawScheduler` moves to `lumen_core::window_backend`, and
`WinitPlugin` is replaced by `lumen_core::window_backend::WindowCorePlugin`,
the window-free half every window backend shares. The accessibility bridge
reaches a window backend as an opaque `lumen_core::traits::A11yBridgeFactory`;
`lumen_a11y_accesskit::bridge_factory()` builds the one the winit backend
accepts, in place of `lumen_a11y_accesskit::winit_bridge`.

Before:

```rust
use lumen_core::render_backend::register_render_backend;
use lumen_layout_taffy::TaffyLayoutPlugin;
use lumen_window_winit::run;

app.add_plugin(TaffyLayoutPlugin);
app.add_systems(TickStage::LayoutSync, my_system.after(lumen_layout_taffy::sync_layout));
register_render_backend(&mut app, my_renderer);

let a11y: lumen_window_winit::A11yBridgeFactory = Box::new(lumen_a11y_accesskit::winit_bridge);
run(app, options, renderers, Some(a11y))?;
```

After:

```rust
use lumen_core::backends::register_backend;
use lumen_core::layout_backend::LayoutSolve;
use lumen_core::traits::WindowBackend;
use lumen_layout_taffy::TaffyLayoutPlugin;
use lumen_window_winit::WinitBackend;

app.add_plugin(TaffyLayoutPlugin);
app.add_systems(TickStage::LayoutSync, my_system.after(LayoutSolve));
register_backend(&mut app, my_renderer);

Box::new(WinitBackend).run(app, options, renderers, Some(lumen_a11y_accesskit::bridge_factory()))?;
```
