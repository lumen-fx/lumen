# Mouse wheel messages carry their delta mode

This affects Rust code that writes a `lumen_core::input::MouseWheel` message
with a struct literal, such as a test that scrolls a headless app. The message
gains `delta_mode: WheelDeltaMode`, the unit the device reported the delta in
before it was converted to pixels. `delta` stays in pixels.

In a literal, give it `delta_mode: WheelDeltaMode::Pixel`:

```rust
use lumen_core::input::{MouseWheel, WheelDeltaMode};

app.world.write_message(MouseWheel {
    delta: glam::Vec2::new(0.0, 40.0),
    delta_mode: WheelDeltaMode::Pixel,
    position,
    local: None,
    time_stamp: lumen_core::time::Instant::now(),
});
```
