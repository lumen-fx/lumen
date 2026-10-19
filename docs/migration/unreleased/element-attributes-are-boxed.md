# `Element.attrs` is a `Box<Attributes>`

This affects Rust code that builds a `lumen_ir::layout_ir::Element` with a
struct literal or assigns its `attrs` field, typically a compiler plugin that
adds elements in `transform_ir`. The attribute bag is now boxed, so an
element is a few pointers wide and a deep tree no longer runs a debug build
out of stack.

```rust
// before
let el = Element {
    tag: "label".to_string(),
    attrs: Attributes { text: Some("Hi".into()), ..Attributes::default() },
    ..Element::default()
};
// after
let mut el = Element { tag: "label".to_string(), ..Element::default() };
el.attrs.text = Some("Hi".into());
```

`attrs: Box::new(Attributes { ... })` also works. Code that reads or writes
fields through `el.attrs.text` is unaffected, as is a call that passes
`&el.attrs` to a function taking `&Attributes`. A trait conversion such as
`Style::from(&el.attrs)` needs the bag itself: write `Style::from(&*el.attrs)`.

The artifact encoding is unchanged.
