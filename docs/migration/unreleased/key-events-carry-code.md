# Key and pointer messages carry the physical key and the motion

This affects Rust code that builds `KeyPressed`, `KeyReleased` or
`FocusedKey` from `lumen_core::input`, or a `PointerData`, with a struct
literal: a test that simulates a key, or a backend that translates its own
input.

Each key message gains `code: KeyCode`, the physical key in the DOM `code`
naming (`KeyW`, `ArrowLeft`, `Numpad5`). Give it the key that was pressed,
or let `Key::us_code` derive the one a US layout puts the logical key on:

```rust
// before
KeyPressed { key, modifiers, repeat: false, time_stamp }
// after
KeyPressed { code: key.us_code(), key, modifiers, repeat: false, time_stamp }
```

`PointerData` gains `movement: Vec2`, the pointer's motion since its previous
move in logical pixels. `PointerData::default()` fills it with zero, so a
literal that ends in `..Default::default()` needs no change; one that names
every field adds `movement: Vec2::ZERO`.

Scripts see the same values as `event.code()` and `event.movement_x()` /
`event.movement_y()`; nothing in a script has to change.
