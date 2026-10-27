# `HeadlessOptions` gained the virtual desktop

This affects Rust code that builds `HeadlessOptions` for
`run_app_headless_rendered` or the Rust SDK's `App::screenshot` and
`App::run_headless_rendered`. Three fields are new, `monitors`,
`positioning` and `toplevel_drag`, which set the desktop a headless run's
windows open on and whether a dragged window follows the pointer on it, and
the struct is `Clone` but no longer `Copy`. Fill the rest from the default:

```rust
HeadlessOptions { dpr: 1.0, ticks: Some(3) }
HeadlessOptions { dpr: 1.0, ticks: Some(3), ..Default::default() }
```

`lumen_core::window_backend::HeadlessDesktop` gained `toplevel_drag` beside
`positioning` the same way; a literal gives it `toplevel_drag: false` to keep
what it did.
