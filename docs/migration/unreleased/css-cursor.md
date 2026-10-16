# The mouse cursor comes from CSS `cursor`

The pointer now shows the CSS `cursor` of the element under it, as in a
browser. Before, Lumen picked one of five cursors itself: the I-beam over text
fields, a pointing hand over anything clickable (a button, a toggle, a tab, an
element with a hover style or a `tab-index`), and grab hands over scrollbar and
slider thumbs.

Text fields keep the I-beam and links show the pointing hand, from the
user-agent stylesheet. Everything else shows the platform arrow, the way native
buttons and web buttons do, so buttons and other clickable elements stop
showing the hand. To keep it, say so in your stylesheet:

```css
button, toggle, switch, checkbox, radio, .clickable { cursor: pointer; }
```

Scrollbar and slider thumbs still show `grab` and `grabbing`.

For Rust code: `lumen_core::input::CursorShape` is gone. `CursorRequest` holds
a `cursor` (a `Cursor`: the `url()` images to try and a `CursorKeyword`) and the
decoded `image`, if any, instead of a shape. A window backend of your own shows
`image` when it is set and the keyword otherwise; `CursorKeyword::None` hides
the pointer.
