# `opacity` fades an element and its children as one picture

An element with `opacity` below 1 and children now composites the whole
subtree first and fades the result once, as CSS does. Before, every descendant
faded on its own, so a parent's background showed through its opaque
children. A faded card with opaque content now looks the way it does in a
browser: the content hides the card under it. Nested opacities still compound.
No markup or CSS changes are needed; check any screenshot baseline that
captured a faded element with children.

This affects Rust plugins that paint native leaves too. `NativeExtract::place`
no longer takes the entity's `Opacity`; it reads opacity from the world itself
and `NativePlacement::opacity` is the alpha your leaf's own paints carry. A
faded ancestor with children is a layer the renderer composites over your
pixels, so it is no longer part of that figure, and `NativePaintCtx::opacity`
is `1.0`.

Drop the `Option<&Opacity>` from the query that feeds `place` and the argument
from the call. Before and after:

```rust
let placed = place.place(e, transform, opacity)?;
let placed = place.place(e, transform)?;
```
