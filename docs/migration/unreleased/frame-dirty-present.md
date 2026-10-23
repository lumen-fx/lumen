# `FrameDirty` has a `present` field

This affects Rust code that builds `lumen_core::render_world::FrameDirty`
with a struct literal. The resource gains `present: bool`, which asks for a
frame put up again from what was already extracted, without walking the
tree, when only something the renderer holds changed:

```rust
// before
FrameDirty { dirty: true }
// after
FrameDirty { dirty: true, present: false }
```

Code that sets `frame_dirty.dirty = true` on the resource needs no change.
