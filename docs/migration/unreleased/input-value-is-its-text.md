# `<input value>` is the text a field starts with

This affects `<input>` elements written with a `value` attribute. `value` used
to be read as a number: a field written `value="hello"` failed to parse, and
one written `value="5"` started empty on the desktop while a browser showed
`5`. It is now the text the field starts with, as in HTML, on every target.

```html
<!-- before -->
<input text="hello" />
<!-- after: either works -->
<input value="hello" />
<input text="hello" />
```

A field that wrote both keeps the one written last.
