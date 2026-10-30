# `CloseRequest` carries a `reason`

This affects Rust code that builds a `lumen_core::input::CloseRequest`, such
as a system that vetoes a close. The message gained a `reason` field of type
`CloseReason`: `Platform` for the window's close button, `Shutdown` for a
signal or the end of a bounded headless run, `WindowClose` for the app's own
`window.close()`, and `AppQuit` for `app.quit()`.

A veto carries the reason of the request it answers:

```rust
msgs.write(CloseRequest { vetoed: true });
msgs.write(CloseRequest { reason: request.reason, vetoed: true });
```

Code that only reads `CloseRequest` keeps working. A close hook now also runs
for the app's own `window.close()` and `app.quit()`, headless runs included.
