# An input's placeholder draws dimmed

This affects desktop apps with an `<input placeholder>` or a
`<textarea placeholder>`.

The hint an empty field shows used to draw in the field's own text color,
so it read as a typed value. It is now dimmed, as in a browser: the built-in
skins color it with the new `--lumen-placeholder` token, which follows
`--lumen-text-muted`, and a skinless app draws it `#a9a9a9`.

To restyle it, write a `::placeholder` rule, which takes `color`,
`opacity`, `font-size`, `font-weight` and `font-family`:

```css
input::placeholder { color: #8a8f98; }
```

To draw every hint in the text color again, point the token at that color.
With a built-in skin that is its text token:

```css
:root { --lumen-placeholder: var(--lumen-text); }
```

`::placeholder` was a parse error before, so no existing stylesheet
changes meaning.
