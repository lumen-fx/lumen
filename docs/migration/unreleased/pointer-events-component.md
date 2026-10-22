# `PointerTransparent` is `PointerEvents::None`

This affects Rust code that inserted `lumen_core::input::PointerTransparent`
to let the pointer pass through an element. The marker is replaced by the
`PointerEvents` component, which is what CSS `pointer-events` sets:

```rust
// before
commands.entity(e).insert(PointerTransparent);
// after
commands.entity(e).insert(PointerEvents::None);
```

The value now inherits the way CSS `pointer-events` does: a descendant
carrying `PointerEvents::Auto` takes the pointer again, where the old marker
covered its whole subtree with no way back. A stylesheet rule reaches the
same component, so `.badge { pointer-events: none; }` needs no Rust at all.
