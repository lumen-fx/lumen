# `HeadlessOptions` gained the virtual desktop

This affects Rust code that builds `HeadlessOptions` for
`run_app_headless_rendered` or the Rust SDK's `App::screenshot` and
`App::run_headless_rendered`. Two fields are new, `monitors` and
`positioning`, which set the desktop a headless run's windows open on, and the
struct is `Clone` but no longer `Copy`. Fill the rest from the default:

```rust
HeadlessOptions { dpr: 1.0, ticks: Some(3) }
HeadlessOptions { dpr: 1.0, ticks: Some(3), ..Default::default() }
```
