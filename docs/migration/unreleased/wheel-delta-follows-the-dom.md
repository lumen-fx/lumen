# Wheel deltas on the desktop follow the DOM sign

A desktop `wheel` event's `delta_y` used to be negative when the wheel turned
toward the user, the opposite of the web target. It now follows the DOM on
every target: positive `delta_y` scrolls down and positive `delta_x` scrolls
right. A handler that zooms or steps through items by the wheel's sign goes the
other way on the desktop until you flip its comparison.

candela, before and after:

```rust
fn on_wheel(ev: int) { if event(ev).delta_y() < 0.0 { next_page(); } }
fn on_wheel(ev: int) { if event(ev).delta_y() > 0.0 { next_page(); } }
```

`lumenc scroll` and the `lumen.simulate` scroll kind take the same sign, so a
test that scrolled down with `lumenc scroll 100 100 0 -50` now writes
`lumenc scroll 100 100 0 50`. A Rust plugin that writes or reads
`lumen_core::input::MouseWheel` uses the same convention.
