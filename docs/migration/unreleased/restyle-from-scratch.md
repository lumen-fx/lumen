# A restyle resolves an element from scratch, and tag defaults are rules

This affects apps that change classes, ids or inline styles from a script, apps
that create elements from a script, and Rust code that drives an element's
style components.

A restyle (a class added or removed, an id changed, a `set_style`, a theme
flip, a breakpoint crossing) used to copy over only the properties the new
resolution set, so a property that only a removed class had set stayed on the
element. It now resolves the element from scratch, as the first cascade did: a
property no rule, markup attribute or inline style sets any more goes back to
its initial value. Removing `.selected` from a row takes its `justify` and its
background with it.

What a tag is by default (a `<column>` stacking downwards, an `<overlay>`
covering its parent, a `<spacer>` taking the space left, a `<scroll>`
scrolling vertically) now comes from the built-in user-agent sheet instead of
from values the parser wrote into the element. An element a script creates
takes those defaults too: `spawn("column")` used to lay its children out in a
row, and now stacks them. A stylesheet rule for one of these tags applies from
the first frame on, where it used to take effect only once the app had
restyled. A `scroll="x"` attribute now lays its element's children out in a
row whatever the stylesheet says about `flex-direction`, like any other
attribute.

The metrics of the parts `<tabs>`, `<menu>`, `<date-picker>` and
`<time-picker>` expand into (a tab button's height and padding, a menu row's
height, a picker's width) moved into the same sheet. A skin's or an app's rule
for `.tab-btn`, `.menu-item`, `.date-picker` and the rest outranks them, as it
already did once the app had run its first frame.

## Rust

A value a system writes into `Style`, `Visuals`, `TextStyle` or `Opacity`
behind the cascade's back is lost at the next restyle. Hold it in
`lumen_core::style_overrides::StyleOverrides` instead; see "Holding a style
value" in the plugin guide.

The snapshot components the built-in widgets kept are gone, because the layer
replaces them: `HoverBaseColor`, `PressBaseColor`, `BaseBorder`,
`StateStyleBase`, `CheckboxBaseFill` and `RadioBaseFill`, the `default_top`,
`default_bottom` and `positioned` fields of `PopupPanel`, and the
`saved_display` field of `IfMarker`. Read what an element shows under a
widget's layer with `StyleOverrides::below`.

In `lumen_ir::css`, `reapply_single`, `reapply_single_with_media` and
`reapply_with_ancestors` are replaced by `RestyleWalk` for walking a tree,
`resolve_element` for one element, and `reapply_detached` for an element not
yet in the tree. `AncestorInfo` carries an element's own declarations,
sibling list and content.

Before:

```rust
use lumen_ir::css::{AncestorInfo, SiblingPosition, reapply_with_ancestors};

let mut el = Element { tag: "tile".into(), ..Default::default() };
el.attrs.classes = vec!["card".into()];
reapply_with_ancestors(&mut el, &sheet, &media, &ancestors, SiblingPosition::default())?;
let attrs = el.attrs;
```

After:

```rust
use lumen_ir::css::{AncestorInfo, resolve_element};

let node = AncestorInfo::new("tile", vec!["card".into()], None)
    .with_declarations(markup_and_inline_declarations);
let attrs = resolve_element(&node, &sheet, &media, &ancestors);
```
