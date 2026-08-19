# Composition

Three ways to avoid repeating markup: templates, for a subtree you use many
times with different values; components, for markup a script decides on, usable
from a script or straight from the tree; and includes, for splitting one long
file into several.

## Templates

A `<template>` declares a named subtree with parameters. Declare it once, then
instantiate it wherever you need it:

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

Each `{name}` marker in the body takes the value the use site binds to that
name. Markers work in any attribute value and in text, which is how the example
above gives each instance its own id and image. A marker written where a tag
name goes is not markup, and fails the build.

A marker is substituted once, when the instance is created, and the value stays
put after that. For text that changes while the app runs, bind inside the
template body (`bind-text="$status"`, see
[reactivity](reactivity.md)) rather than passing the value through a marker; a
`bind-*` attribute cannot read a marker, and the `$arg.<name>` form is refused.

The `<template>` block itself never renders. It is stripped from the tree, so
where you put it in the file does not matter, and a use site may come before
the declaration.

A template and an `lmn!` block in a script are the same thing: both declare a
fragment, and both instantiate the same way. See
[components](#components) for the script side.

### Two ways to instantiate

`<day idx="0"/>` and `<use template="day" idx="0"/>` do the same thing. The
short form reads better for a small component; the `<use>` form is clearer when
the template name would collide with something else, and it is the form to
prefer for a shared page layout.

Do not name a template after a built-in tag. A template named `button` takes
over every `<button>` in the app.

### Parameters and defaults

Every marker the body reads is a parameter. Attributes on the `<template>` tag,
other than `name`, give a parameter its default, which fills in wherever the use
site leaves that name unbound:

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

Bind every marker either at the use site or as a default. A marker with neither
stays in the markup as written, where it reads as a
[global signal](reactivity.md) like any other `{marker}` in the tree, and shows
up as literal text when no signal by that name is set.

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

When the use site is self-closing, or has no content, the slot falls back to
its own:

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

A template body can instantiate other templates, up to 64 levels deep. A
template that instantiates itself, directly or through a chain, fails the build
with the chain named.

### Where templates are visible

A template is visible to the whole file it is declared in, and to any file that
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

Two files declaring the same template name with different bodies fails the
build: the set is app-wide, so either answer would change what half the use
sites render.

## Components

A component is a candela function that returns markup. Write the markup in an
`lmn!` block and the logic around it in candela:

```rust
import "lumen.cdl";

fn Home(name) {
    return lmn!(<label class="home" text="home for $name"/>);
}

fn App() {
    return lmn!(
        <column id="app">
            <Home name="bob"/>
        </column>
    );
}

fn on_ready() {
    lumen::mount(App());
}

fn main() {}
```

`lmn!` is a markup block, not candela: tags, attributes, `$name`
interpolation, and elements naming another component. Everything else in the
function is ordinary candela, so a component decides what to render with `if`
and loops and then hands back one piece of markup.

A block is the same entity a `<template>` is. It compiles to a fragment when
the app is built, and the call instantiates that fragment by key, so a shipped
app carries the compiled markup and parses nothing while it runs. `lumenc
check` reads every block, and a malformed one fails the check with the file and
line it was written on.

A call returns a node handle, valid for the tick it was minted in. Attach it
with `lumen::mount(handle)` to put it at the app root, or with any of the
tree-mutating builtins in the
[candela reference](../reference/scripting-candela.md) to put it somewhere
else.

Write components in a `.cdl` file. An inline `<script>` block is read as XML
like the rest of the markup, so a block written in one has to sit inside a
`<![CDATA[ ... ]]>` section for its tags to survive.

### Arguments

`$name` reads the candela value of that name where the block was written, and
substitutes it once, when the instance is built. A value that changes while the
app runs belongs in a `bind-*` attribute inside the block, exactly as in a
template body:

```rust
fn Counter(label) {
    return lmn!(<label text="$label" bind-text="count"/>);
}
```

`{name}` keeps its markup meaning inside a block: it is a
[signal reference](reactivity.md), resolved from the global scope every time
that signal changes. Write `$name` for something the surrounding candela knows
and `{name}` for something the app's signals hold.

Write `$$` for a literal `$`.

### Components inside components

An element whose tag starts with a capital letter names the candela function of
that name. Props map to that function's parameters by name, in any order; a
parameter no prop names is passed the empty string. A prop naming a parameter
the function does not declare fails the compile, naming the component.

```rust
fn Row(title, tone) {
    return lmn!(<row class="row row-$tone"><label text="$title"/></row>);
}

fn List() {
    return lmn!(
        <column>
            <Row title="First" tone="warm"/>
            <Row tone="cool" title="Second"/>
        </column>
    );
}
```

A prop is text. `$name` in a prop value reads the candela value of that name
where the element was written and renders it into the string, so `Row` above
receives `"First"` and `"warm"`. A component that wants a number parses it.

A component element means the same thing in a block as in a `.lmn` file: the
build resolves it against the component it names, rather than the block
expanding to a call. `List` above builds `Row` twice at compile time and
carries the result.

### Naming a component from markup

A `.lmn` file writes a component as a tag, the way it writes a template:

```html
<root>
  <column id="app">
    <Home name="bob"/>
  </column>
  <script src="main.cdl"/>
</root>
```

Props there are markup attribute values, so `name="bob"` reaches the block's
`$name` as text. Any component can be named this way; the section below is what
that costs.

Component names and `<template name="...">` names share one namespace, and it
is app-wide. Two declarations claiming one name fail the build with both sites
named; rename either. A component that reaches itself, directly or through
another, fails the build with the chain named.

### When the subtree appears

Nothing parses markup while an app runs, so the tree a use site stands for is
built before the app starts. What that takes depends on the component:

- **The build can stand in for the call.** `Home` above returns its block and
  nothing else, and every value in the block came from a parameter. The build
  puts the block at the use site with the arguments already substituted, which
  is exactly what calling `Home` would have produced. The subtree is on screen
  in the first frame, and the function is never called.
- **The function has to run.** It works a value out, or picks between blocks:

  ```rust
  fn Greet(who: string) {
      let loud = who + "!";
      return lmn!(<label text="hello $loud"/>);
  }

  fn Pick(on: string) {
      if on == "yes" { return lmn!(<label text="on"/>); }
      return lmn!(<label text="off"/>);
  }
  ```

  Every block either one may return is still compiled into the app. What
  stands at the use site is a marker; the runtime calls the function with the
  props as arguments and puts the node it returns in the marker's place.
  `Pick` picks its arm there, from blocks that are already built.

Both are on screen in the first frame. The fill runs on the first tick, before
the tree is drawn, so a component that has to run costs the time that run takes
rather than a frame the reader sees empty. A component doing real work at that
moment does delay that first frame; move it behind a signal if it is slow.

A function the loaded program does not declare is reported once, naming the
component, rather than leaving an empty element behind.

### Annotate the parameters of a component that runs

Note the `: string` on `Greet` and `Pick`. A component that has to run is
called by name, and both the build and a shipped app call it through compiled
bytecode: candela makes a function callable by name only where it says what its
arguments are. A component written with a bare parameter compiles and ships,
and the call to it finds nothing, which leaves an empty element where its body
belongs.

Props arrive as text, so `string` fits every one of them; `any` works too.

A component the build stands in for is never called, so its parameters need no
annotation. Annotating them all is the simpler rule, and it is what keeps a
component usable after an edit turns it into one that has to run.

`lumenc web` names the ones that would come out empty.

### A component on the web

Both kinds are in the document. A component's shape is markup, not app state,
so `lumenc web` resolves it while it builds the site: where the build cannot
stand in for the call it runs the function itself and writes the body it
returns into the HTML. A reader with no scripting and a crawler get the whole
tree, in every `render` and `prerender` combination. The browser adopts those
elements the way it adopts the rest of the page rather than building them
again.

The one thing that does not reach the document is a component written inside a
`<for>`: what it renders depends on the row it is rendered for, so the browser
fills it per row. The build says which components those are.

### What a block may not do

- **One root element.** A block returns one node, so a body with no root or
  several fails the build.
- **No markup children on a component element.** `<Row title="x"/>` is a call;
  `<Row><label/></Row>` is refused. Pass what the component renders as a prop,
  or give the component its own `<slot/>` and instantiate it as a template.
- **`lmn-` is reserved.** Names starting with `lmn-` belong to what a block
  generates.

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

Includes are resolved before anything else, so a `<template>` declared in an
included file is usable in the file that included it. This is the way to
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
- Markup a script decides on and places: a component.
- A subtree whose logic lives in candela but whose place in the page is fixed:
  a component, named as a tag from the `.lmn` file.
- One long page you want to read in pieces, each appearing once: includes.
- A stylesheet growing too large: `@import` in `main.css`. See
  [styling](styling.md).
