# `Window` says whether it is maximized or fullscreen

This affects Rust code that builds `lumen_core::window::Window` or
`lumen_core::window::WindowRuntime` with a struct literal. `Window` gains
`maximized: bool` and `fullscreen: bool`, which a window backend applies to
the platform window and updates when the user maximizes, restores or leaves
fullscreen from outside the app. `WindowRuntime` gains `fullscreen: bool`
beside `maximized`.

In a literal, give the new fields `false`, or finish it with
`..Window::default()`:

```rust
let window = Window { title: "Tools".into(), size, ..Window::default() };
```

A headless run's Wayland mode (`--positioning=off`, or
`HeadlessDesktop { positioning: false }`) now reports no position for a
window, as Wayland does: `WindowRuntime::position` is `None`.
