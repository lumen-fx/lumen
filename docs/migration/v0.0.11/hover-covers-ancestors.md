# `:hover` holds for every ancestor of the element under the pointer

An element now counts as hovered while the pointer is over it or over any of
its descendants, as CSS `:hover` does in a browser. Before, only the innermost
element under the pointer was hovered, so a button lost its `:hover` style and
its `<tooltip>` as soon as the pointer crossed onto the icon or label inside
it. A parent's `:hover` rule now shows while one of its children is hovered,
which changes how an app looks if it styles a container with `:hover` and
relied on the style dropping over the container's children. Scope such a rule
to the element you mean, for example by moving it onto the child.

With tooltips nested inside each other, only the innermost hovered one shows.

Scripts see `pointerenter` and `pointerleave` the web's way: an element gets
`pointerenter` when the pointer reaches it or one of its descendants, and
`pointerleave` when the pointer is over neither. Moving from a button onto its
icon fires `pointerenter` on the icon and nothing on the button.

What a click, a press, a wheel, `pointerdown` and the other pointer events
aim at does not change: the innermost element under the pointer, like
`event.target`. `document.hovered()` still returns that element.

Rust code that reads the `Hovered` marker keeps its meaning: it sits on the
one hit target only. To ask whether an element is hovered in the `:hover`
sense, read the new `HoverWithin` marker, which sits on the hit target and
every ancestor of it.

```rust
// before: true only when the pointer is over the card itself
fn card_hovered(cards: Query<Entity, (With<Card>, With<Hovered>)>) {}
// after: true while the pointer is anywhere over the card
fn card_hovered(cards: Query<Entity, (With<Card>, With<HoverWithin>)>) {}
```
