# `<split>` is a built-in tag

This affects apps and runtime modules that use a tag named `split` of their
own: a custom widget registered as `split`, or a module that lists `split`
under `tags`. `<split>` is now the built-in container of resizable panes,
so a module declaring it is refused when it loads, as it is for every other
built-in tag, and markup that writes `<split>` gets the built-in panes and
gutters.

Rename your tag:

```html
<!-- before -->
<split>...</split>
<!-- after -->
<my-split>...</my-split>
```

Or drop it and use the built-in one, which puts a draggable gutter between
its children; see `<split>` in the tags reference.
