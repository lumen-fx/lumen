# Input messages carry a `time_stamp`

This affects Rust code that builds `PointerData`, `MouseWheel`, `KeyPressed` or
`KeyReleased` with a struct literal, typically a test that injects input. Each
now has a `time_stamp: Instant` field, the moment the event happened, so an
app can measure how long an input took to reach the screen (see
`FrameTiming`). `PointerData::mouse` stamps the current instant; a literal
names one.

```rust
// before
keys.write(KeyPressed { key, modifiers, repeat: false });
// after
keys.write(KeyPressed { key, modifiers, repeat: false, time_stamp: Instant::now() });
```

Code that only reads these messages is unaffected.
