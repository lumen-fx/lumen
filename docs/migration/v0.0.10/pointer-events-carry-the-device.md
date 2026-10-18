# Pointer events carry the device

Pointer events now report the pen and touch readings the web `PointerEvent`
has, and a node can capture a pointer. Three existing behaviours change with
it.

A `pointermove` reports `button` as `-1`, as on the web; it used to report `0`.
A handler that tells a drag from a hover by `button() == 0` reads `buttons()`
instead, which holds the mask of buttons down.

```rust
// before
if e.button() == 0 { drag(e); }
// after
if e.buttons() & 1 != 0 { drag(e); }
```

The back and forward mouse buttons report `3` and `4`; they used to report `0`,
the primary button's code.

Rust code that writes `PointerMoved`, `PointerPressed` or `PointerReleased`
itself (a window backend, a test) fills in the new `pointer` field with the
device behind the event. `PointerData::mouse(buttons)` is the mouse, and
`Default::default()` the mouse with no button held. Code that reads presses to
drive a widget checks `is_primary()` rather than the button alone, so a second
finger does not press it.

```rust
// before
PointerPressed { position, button, local: None }
// after
PointerPressed { position, button, local: None, pointer: PointerData::mouse(1) }
```

The C ABI is 0.15: `LumenEvent` gains the pointer fields at its end and the
`lumen_node_*_pointer_capture` calls are new. The existing fields keep their
offsets, so a callback built against 0.14 reads them unchanged.
