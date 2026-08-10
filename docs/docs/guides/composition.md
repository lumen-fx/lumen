# Composition

Two ways to avoid repeating markup: templates, for a subtree you use many times
with different values, and includes, for splitting one long file into several.

## Templates

Define a template once, then instantiate it wherever you need it:

```html
<root>
  <template name="day">
    <column class="day" id="day-{idx}" width="120px">
      <label class="day-name" id="day-{idx}-name" text="{name}"/>
      <image class="day-icon" src="icons/{icon}.png" width="56px" height="56px"/>
    </column>
  </template>

  <row gap="12">
    <day idx="0" name="Mon" icon="sun"/>
    <day idx="1" name="Tue" icon="cloud"/>
    <day idx="2" name="Wed" icon="rain"/>
  </row>
</root>
```

Each `{name}` marker in the body is replaced by the matching attribute from the
use site. Markers work anywhere in the body, including inside `id`, `class`, and
`src` values, which is how the example above gives each instance a distinct id
and image.

The `<template>` block itself never renders. It is stripped from the tree, so
where you put it in the file does not matter.

### Two ways to instantiate

`<day idx="0"/>` and `<use template="day" idx="0"/>` do the same thing. The
short form reads better for a small component; the `<use>` form is clearer when
the template name would collide with something else, and it is the form to
prefer for a shared page layout.

Do not name a template after a built-in tag. A template named `button` takes
over every `<button>` in the app.

### Defaults

Attributes on the `<template>` tag, other than `name`, fill in markers the use
site leaves out:

```html
<template name="chip" icon="dot" tone="neutral">
  <row class="chip chip-{tone}">
    <image src="icons/{icon}.png"/>
    <label text="{label}"/>
  </row>
</template>

<chip label="Ready"/>
<chip label="Failed" tone="danger" icon="warning"/>
```

Supply every marker either at the use site or as a default. A marker with no
value is left in the markup as written, and shows up later as a parse error or
as literal text.

### Slots

A `<slot/>` in the template body is where the use site's own content lands.
This is what makes a shared frame possible:

```html
<template name="card">
  <column class="card" gap="8">
    <label id="title" class="card-title" text="{title}"/>
    <slot/>
  </column>
</template>

<card title="Recent">
  <label text="Nothing yet."/>
  <button text="Refresh"/>
</card>
```

When the use site is self-closing, or has no children, the slot falls back to
its own content:

```html
<slot><label text="Empty"/></slot>
```

Slots are unnamed. If a template body has more than one `<slot/>`, every one of
them receives the same content.

### Ids inside a template

Give the use site an `id` and every id written in the template body is prefixed
with it:

```html
<card id="recent" title="Recent">
  <label text="Nothing yet."/>
</card>
```

The card's title label, written as `id="title"` in the template body, is
addressable as `recent:title` in that instance, and prefixes stack through
nested instances. Content you pass into a slot keeps the ids you gave it.
Without an `id` on the use site, the ids in the body stay exactly as the
template wrote them, which is what you want for a template used only once.

Reach for this whenever a template appears more than once and its contents need
to be reachable from a script or a CSS id selector; otherwise the instances all
answer to the same id.

### Templates using templates

A template body can instantiate other templates. Expansion repeats until nothing
is left to expand, so order of definition does not matter. A template that
instantiates itself, directly or through a chain, fails the build rather than
looping.

### Where templates are visible

A template is visible to the whole file it is defined in, and to any file that
includes it.

In a [multi-page app](pages.md), templates are visible app-wide: a
`<template>` in any `.lmn` file in the app directory can be used from any page.
Put a shared frame in `layout.lmn`, which contributes its templates but is not
itself a page:

```html
<!-- layout.lmn -->
<root>
  <template name="layout">
    <column padding="20" gap="16" width="100%" height="100%">
      <row gap="12">
        <a href="index" text="Home"/>
        <a href="settings" text="Settings"/>
      </row>
      <column gap="8">
        <slot/>
      </column>
    </column>
  </template>
</root>
```

Every page then wraps its content in it:

```html
<!-- settings.lmn -->
<root>
  <use template="layout">
    <label text="Settings" font-size="24"/>
  </use>
</root>
```

## Includes

An include splices another file's markup into this one at that exact spot:

```html
<root>
  <include src="parts/toolbar.lmn"/>
  <column grow="1">
    <include src="parts/editor.lmn"/>
  </column>
</root>
```

The included file holds bare markup, not a document; it does not need its own
`<root>`. Paths are relative to the file doing the including, includes may
nest, and a cycle fails the build with the whole chain named.

Includes are resolved before templates are expanded, so a `<template>` defined
in an included file is usable in the file that included it. This is the way to
keep a component library in its own file:

```html
<include src="components.lmn"/>
<card title="Recent"><label text="Nothing yet."/></card>
```

`lumenc run` watches included files, so editing one reloads the app.

## Which to reach for

- The same subtree appearing several times, with different text or images: a
  template.
- Content that needs a frame around it: a template with a `<slot/>`.
- One long page you want to read in pieces, each appearing once: includes.
- A stylesheet growing too large: `@import` in `main.css`. See
  [styling](styling.md).
