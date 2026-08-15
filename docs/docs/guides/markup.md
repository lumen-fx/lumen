# Writing markup

A Lumen app describes its interface in a `.lmn` file: an XML-shaped tree of
tags with attributes. `main.lmn` is the entry point, and everything the window
shows starts there.

## Anatomy of a file

```html
<root>
  <column gap="12" padding="24">
    <label class="title" text="Hello" />
    <row gap="8">
      <button id="ok" text="OK" />
      <button id="cancel" class="ghost" text="Cancel" />
    </row>
  </column>
  <script src="main.cdl" />
</root>
```

The rules are strict and small:

- One root element per file, normally `<root>`.
- Every tag closes, either with a matching end tag or as `<tag />`.
- Attribute values are quoted.
- `<!-- comments -->` work anywhere between tags.
- Text between an open and close tag becomes the element's text:
  `<label>Hello</label>` and `<label text="Hello" />` are the same thing.

An unknown tag is an error, so a typo fails at `lumenc check` instead of
silently rendering nothing. An unknown attribute is dropped with a warning
naming the tag and the attribute, so forward-compatible markup still parses
and a typo is still reported.

Nesting is capped at 32 levels. A tree that deep is almost always an accident,
and the cap turns it into an error message rather than a crash.

## Containers and controls

Tags fall into two groups.

**Containers** arrange their children. The direction lives in the tag name
rather than an attribute, so you pick `<column>` or `<row>` instead of setting
a flex direction. `<scroll>` is a container that scrolls its overflow,
`<tile>` is a plain box, `<spacer>` eats leftover space along the parent's
axis, and `<overlay>` and `<dialog>` float above their siblings instead of
taking part in the surrounding layout.

**Controls** carry state and respond to input. `<button>`, `<input>`,
`<textarea>`, `<toggle>`, `<switch>`, `<slider>`, `<checkbox>`, `<radio>`, and
`<progress>` are the primitives; `<dropdown>`, `<tabs>`, `<menu>`, and the date
and time pickers are built from them. Controls are focusable by default, so
Tab reaches them without you writing a tab order.

`<label>` and `<image>` are leaves that paint text and pictures.

The complete list, with every attribute each tag accepts, is in
[Tags and attributes](../reference/tags.md).

## Attributes

Attributes come in a few families, and most of them apply to any tag:

- **Sizing**: `width`, `height`, `min-width`, `max-width`, `min-height`,
  `max-height`, `aspect-ratio`. Lengths are pixels (`120px`), percentages
  (`50%`), `auto`, or a bare number read as pixels.
- **Spacing**: `padding`, `margin`, `gap`. `padding` and `margin` take one to
  four terms in the CSS top-right-bottom-left order.
- **Layout**: `grow`, `shrink`, `align`, `justify`, `position`, `inset`,
  `z-index`, `overflow`.
- **Paint**: `bg`, `text-color`, `hover-bg`, `press-bg`, `radius`, `border`,
  `shadow`, `opacity`. Colours are `#rrggbb` or `#rrggbbaa` literals; `bg` also
  takes a gradient.
- **Text**: `font-size`, `font-weight`, `font-family`, `line-height`,
  `text-align`, `wrap`, `max-lines`.
- **Behaviour**: `id`, `class`, `tab-index`, `disabled`, `draggable`,
  `translatable`, `lang`, `dir`.

Some attributes only mean something on one tag: `each` on `<for>`, `href` on
`<a>`, `group` on `<radio>`, `open` on `<dialog>`.

```html
<tile width="120px" height="80px" radius="10" bg="#7aa2f7"
      shadow="0 4 14 #00000077" />
```

An attribute written on the element always beats a stylesheet rule for the same
property. Use attributes for the handful of values that belong to that one
element, and CSS for anything you want to reuse; see
[Styling](styling.md).

## Identity: id and class

`id` names one element. Scripts look elements up by it, styles select it with
`#name`, and hot reload uses it to carry a control's state across an edit. Give
every element a script touches, or whose typed text and scroll position should
survive a reload, an `id`.

`class` takes a space-separated list and is the normal hook for styling:

```html
<button id="save" class="btn primary" text="Save" />
```

## Text and interpolation

Text can come from the tag body, the `text` attribute, or a signal. Braces
interpolate a signal into text and into string attribute values:

```html
<label text="Signed in as {$user}" />
```

Binding an element's whole text to a signal is a separate, cheaper form:

```html
<label bind-text="$status" />
```

Both are covered in [Reactivity](reactivity.md).

## Splitting a file up

`<include src="parts/header.lmn" />` splices another file's elements in place
of the tag. Paths resolve against the directory of the file doing the
including, includes may nest, and a cycle is reported with the whole chain.
Included files are watched, so editing a fragment reloads the running app.

```html
<root>
  <include src="parts/toolbar.lmn" />
  <column class="body">
    <include src="parts/list.lmn" />
  </column>
</root>
```

Includes are resolved before parsing, which means a `<template>` declared in
an included file is usable from every file in the app.
For reusable parameterised chunks, see [Composition](composition.md).

## Attaching a script

```html
<script src="main.cdl" />
```

The `src` form points at a script file next to the markup. A `<script>` tag
with a body works too, and is convenient for a few lines. An app can carry more
than one `<script>` tag, and each file's extension picks the host that runs it.
Files of one language are concatenated in document order and share a set of
functions; different languages run side by side and share only signals. See
[Scripting](scripting.md).

## Where to look things up

- Every tag and attribute, with accepted values:
  [Tags and attributes](../reference/tags.md)
- Selectors and properties: [CSS](../reference/css.md)
- Signals, lists, and conditionals: [Reactivity](reactivity.md)
- Multi-page apps and `<a href>`: [Pages](pages.md)
- Templates and slots: [Composition](composition.md)
