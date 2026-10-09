# Unset `inset` sides are `auto`, not `0`

This affects apps that set `position: absolute` without naming every side of
`inset`. An unset side used to be `0`, so an absolutely positioned element with
no inset stretched over its parent's padding box, and one pinned by only some
sides was held at `0` on the others. Unset sides are now `auto`, as the CSS
reference documents and as browsers do: the element keeps its content size
and sits against the sides you named.

To keep the stretch, name the sides:

```css
.backdrop { position: absolute; }
.backdrop { position: absolute; inset: 0; }
```

`<overlay>` and `<dialog>` still default to `inset: 0`. `inset` also accepts
`auto` per side now, so `inset: 4 8 auto auto` pins an element to its parent's
top-right corner at its natural size.
