# Tags and attributes

Every tag and attribute the `.lmn` markup parser accepts, what it does, and
what it defaults to. For the style surface see [CSS](css.md); for the
task-level introduction see [Writing markup](../guides/markup.md).

Markup is XML-shaped. Every tag closes (`<tile/>` or `<tile></tile>`),
attribute values are double-quoted, and a single element wraps the
document (`<root>` by convention). An unknown tag is a compile error. An
unknown attribute is dropped with a warning, so forward-compatible markup
still parses and a typo is still reported. Attributes on a custom widget tag
are exempt: the widget reads its own props out of them.

Element nesting is capped at 32 levels; a template instantiating other
templates is capped at 64 levels of nesting.

## Value forms

| Form | Accepted | Notes |
| --- | --- | --- |
| length | `auto`, `<n>`, `<n>px`, `<n>%` | A bare number is pixels. |
| number | `<n>`, `<n>px` | The `px` suffix is accepted and stripped. |
| integer | `<n>` | |
| color | `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` | A shorthand digit counts twice, so `#d8f` is `#dd88ff`. Named colours and `rgb()` are not accepted. |
| gradient | `linear-gradient(...)`, `radial-gradient(...)`, `conic-gradient(...)` | `bg` only. See [CSS values](css.md#gradients). |
| image | `url("path")`, `url('path')`, `url(path)` | `bg` only. The path resolves against the app directory. See [background images](css.md#background-images). |
| edges | 1 to 4 terms, top-right-bottom-left | `8`, `8 16`, `8 16 4`, `8 16 4 12`. Each term may be `<n>`, `<n>px`, or `<n>%`. |
| duration | `<n>ms`, `<n>s` | |
| signal | `name` or `$name` | The `$` prefix is the preferred spelling. |

### Boolean attributes

Every boolean attribute reads the same set of values:

| Value | Result |
| --- | --- |
| `true`, `yes`, `1`, or an empty value (`disabled=""`) | true |
| `false`, `no`, `0` | false |
| anything else | false, with a compiler warning naming the accepted forms |

Matching is case-sensitive, so `True` warns.

## Text content and interpolation

An element's text child becomes its text: `<label>Hello</label>` is the
same as `<label text="Hello"/>`. An explicit `text` attribute wins, and
only the first non-empty text child is read.

An element with both text and element children lays its text out first,
before the children, as a browser does:
`<button text="Save"><column width="16" height="16"/></button>` is wide
enough for the caption and the icon, and its `gap`, `padding`,
`flex-direction`, and alignment place the text like a child. The text is not
an element of its own: selectors, scripts, and the element's `children`
list do not see it. Text fields (`<input>`, `<textarea>`) are the exception;
their text always fills the field.

`{...}` placeholders are substituted in `text`, `placeholder`, `src`,
`id`, `class`, `style`, and `drag-payload`:

| Placeholder | Resolves to |
| --- | --- |
| `{$name}`, `{name}` | The global signal `name`. |
| `{row.field}` | A field of the current `<for>` record. |
| `{$index}` | The current `<for>` row index. `{idx}` is the older spelling. |

Bare `{name}` still works; the compiler emits an informational lint
suggesting the explicit form.

A placeholder is resolved when the tree it belongs to is built, against the
signals as they stand then; a `<for>` row is built per record and reads the
signals then, and again whenever its record changes. One naming a signal
nothing has set stays in the string as the braces the author wrote, so a typo
reads as a typo. A `{row.field}` the record does not carry is the exception:
it resolves to an empty string and is reported, because the record is what
says which fields there are.

A `{k}` placeholder inside a `<template>` body reads the parameter `k`, and
falls back to the global signal `k` when no use site binds it. See
[Composition](../guides/composition.md).

## Common attributes

These apply to any element unless the entry says otherwise.

### Identity

| Attribute | Value | Effect |
| --- | --- | --- |
| `id` | text | Names the element for CSS `#id`, script lookup, and `<menu>` targeting. |
| `class` | space-separated names | Names for CSS `.class` matching. |
| `style` | typography role | Sets `font-size` from a role name when `font-size` is not authored. Roles: `display-xl`, `display-lg`, `display-md`, `display-sm`, `headline-lg`, `headline-md`, `headline-sm`, `title-lg`, `title-md`, `title-sm`, `body-lg`, `body-md`, `body-sm`, `label-lg`, `label-md`, `label-sm`, `caption`, `overline`. |
| `tab-index` | integer | Keyboard focus order, and makes the element focusable by a click. `-1` removes the element from Tab order; a click still focuses it. |
| `dir` | `ltr`, `rtl`, `auto` | Writing direction; inherited by descendants. Defaults to the app locale's direction. |
| `lang` | BCP-47 tag | Language for text shaping and accessibility; inherited. |
| `translatable` | catalogue key | Resolves every string the element shows through the loaded translation catalogue: the text from the message the key names, `placeholder` from `<key>.placeholder`, and `alt` from `<key>.alt`. An attribute the markup does not write is not translated. The text falls back to the authored `text`, then to the key itself, unless the element names one of the other strings. `lumenc i18n extract` collects these keys. |
| `format` | `number`, `currency:<code>`, `date`, `time`, `datetime`, `relative` | Writes the element's text for the app's locale: a decimal number; a decimal amount in an ISO-4217 currency; a `YYYY-MM-DD` date, optionally with `THH:MM[:SS]`; or whole seconds from now, past negative. Applies to a `bind-text` value as well as an authored `text`. A spec that is not one of these, or text the spec cannot read, leaves the text alone. |

### Sizing

| Attribute | Value | Default |
| --- | --- | --- |
| `width`, `height` | length | content size |
| `min-width`, `min-height`, `max-width`, `max-height` | length | unconstrained |
| `aspect-ratio` | number | none |
| `grow` | number | `0`; `<spacer/>` defaults to `1` |
| `shrink` | number | `1` |

### Spacing

| Attribute | Value | Default |
| --- | --- | --- |
| `padding` | edges | `0` |
| `margin` | edges | `0` |
| `gap` | number | `0` |
| `inset` | edges; any side may be `auto` | `0` on `<overlay>` and `<dialog>`, otherwise `auto` |

### Layout

| Attribute | Value | Default |
| --- | --- | --- |
| `align` | `start`, `end`, `center`, `stretch` | `stretch` |
| `justify` | `start`, `end`, `center`, `between` (`space-between`), `around` (`space-around`), `evenly` (`space-evenly`) | `start` |
| `position` | `relative`, `absolute` | `relative` |
| `z-index` | integer | paint in document order |
| `overflow`, `overflow-x`, `overflow-y` | `visible`, `hidden`, `scroll` | `visible` |
| `layout-boundary` | boolean | Auto: true for scroll containers, and for elements that carry a fixed width, a fixed height and `shrink: 0` |

Flex direction comes from the tag, not an attribute: `<row>` lays out
horizontally, `<column>` vertically. Use CSS `flex-direction` to change
it, including the reverse variants.

`overflow: scroll` on any element makes it a live scroll container, the
same as `<scroll>`.

An element's own text lays out as an item of its own, sized to the words,
so `justify` places it along the main axis the same way it places a child:
`justify="center"` on a `<button>` centres its label. It applies on a
horizontal main axis only, and an authored `text-align` wins where both
are set.

### Colour and paint

| Attribute | Value | Default |
| --- | --- | --- |
| `bg` | color, gradient, or `url()` image | transparent |
| `bg-fit` | `cover`, `contain`, `fill`, `none`, `scale-down` | `cover` |
| `radius` | number | `0` |
| `border` | `<width> [solid\|none] <#color>` in any order, or `none` | none |
| `shadow` | `[inset] <x> <y> <blur> [<spread>] <#color>` | none |
| `opacity` | number, clamped to 0..1 | `1` |
| `text-color` | color | inherited, else the built-in text color |
| `hover-bg` | color | no hover swap |
| `press-bg` | color | no press swap |
| `focus-outline` | `<width> <#color>` | none |
| `disabled-opacity` | number, clamped to 0..1 | `0.5`, used when the element is disabled and neither `:disabled { bg }` nor `:disabled { opacity }` was authored |
| `knob-color` | color | built-in knob fill |
| `knob-inset` | number | `4` |
| `thumb-size` | number | `16` |
| `popup-gap` | number | `4` |

The markup `shadow` attribute takes one shadow. Stacked shadows are a CSS
feature.

### Typography

| Attribute | Value | Default | Inherited |
| --- | --- | --- | --- |
| `text` | text | none | no |
| `font-size` | number | `16` | yes |
| `font-family` | family list | platform sans-serif | yes |
| `font-weight` | `normal` (400), `bold` (700), or 1..1000 | `400` | yes |
| `line-height` | number (multiplier) or `<n>px` | runtime ratio | yes |
| `text-align` | `start`/`left`, `center`, `end`/`right` | `start` | yes |
| `wrap` | `none`/`nowrap`, `word`/`normal`, `glyph`/`char`, `ellipsis` | `none` | yes |
| `max-lines` | non-negative integer | unlimited | yes |

`wrap="ellipsis"` elides overflowing single-line text with a trailing
`...`. It is the markup spelling of CSS `text-overflow: ellipsis`.

### Text input paint

Only meaningful on `<input>` and `<textarea>`.

| Attribute | Value | Default |
| --- | --- | --- |
| `selection-color` | color | text fill at reduced alpha |
| `selection-text-color` | color | selected glyphs keep their fill |
| `caret-color` | color | the text fill |
| `caret-width` | number | `2` |
| `caret-blink` | duration | `530ms` half-cycle, shared by the whole app |
| `password-character` | one character | a bullet |

### Scrolling

`sensitivity` and `inertia` apply to scroll containers. The
`scrollbar-*` attributes style the overlay scrollbar.

| Attribute | Value | Default |
| --- | --- | --- |
| `scroll` | `x`, `y`, `both` | `y` on `<scroll>`, otherwise no scrolling |
| `sensitivity` | number | `1.0` |
| `inertia` | number | `0.4` |
| `scrollbar-thickness` | number | `8` |
| `scrollbar-thickness-thin` | number | `4` |
| `scrollbar-margin` | number | `2` |
| `scrollbar-min-thumb` | number | `24` |
| `scrollbar-track-hover` | color | no track tint |
| `scrollbar-hover-boost` | number | `1.6` |
| `scrollbar-fade-delay` | duration | `1s` |
| `scrollbar-fade-duration` | duration | `250ms` |

Setting `scroll="x"` also flips a column-defaulted container to a row so
children stack left to right.

### Interaction and drag and drop

| Attribute | Value | Effect |
| --- | --- | --- |
| `disabled` | boolean | Input routing skips the element and the default render dims it. |
| `draggable` | boolean | Pointer drags translate the element. |
| `drag-payload` | text | Makes the element an in-app drag source publishing this payload. An empty value uses the element's `id`. Interpolated per row inside `<for>`. |
| `drop`, `drop-target` | boolean | Makes the element a drop target for OS file drops and in-app drags. |
| `accept` | MIME type | Restricts what an in-app drop target accepts. Absent accepts anything. |

An element receives the pointer when it is focusable (it has a
`tab-index`, which the controls and `<a href>` carry by default), when it
scrolls, or when it paints a background. Painting is not required: a
`<button>` with no `bg` in markup, in your CSS, or in a skin still
hovers, presses, and clicks over its own rect. Where several targets
overlap, the innermost one takes the event.

### Binding

Values name a signal, with or without a leading `$`.

| Attribute | Applies to | Direction |
| --- | --- | --- |
| `bind-text` | any element with text; `<input>`, `<textarea>` | two-way for text inputs, one-way otherwise |
| `bind-checked` | `<toggle>`, `<switch>`, `<checkbox>` | two-way |
| `bind-value` | `<slider>`, `<progress>`, `<dropdown>`, `<tabs>`, `<date-picker>`, `<time-picker>` | two-way |
| `bind-disabled` | any | one-way, drives the disabled state |
| `bind-scroll` | scroll containers | two-way, vertical offset in pixels |

`bind-text`, `bind-checked`, and `bind-value` share one slot, so an
element binds at most one of them. `bind-disabled` and `bind-scroll` are
independent and may be combined with the others.

`bind-text`, `bind-checked`, and `bind-value` also accept `$self.<field>`
and `$parent.<field>` to read a per-entity property bag instead of a
global signal. `bind-disabled` and `bind-scroll` accept named signals
only.

No `bind-*` accepts `$arg.<name>`, the form that names a template
argument; it is refused with an error. An argument is substituted once,
when the instance is created, so a value that changes while the app runs
belongs in a signal the body binds to.

A widget seeds its bound signal from the authored value on first spawn if
the signal is not already set.

### Built-in sizing floors

An always-on stylesheet applies these beneath any skin and beneath your
CSS, so a bare app still has usable controls. Any rule of yours wins.

- `root`, `title-bar`: `width: 100%`; `title-bar` is 32 px tall.
- `button`: `min-height: 36`.
- `input`, `textarea`: `min-height: 24`, `min-width: 160`.
- `toggle`: `height: 36`, `min-width: 96`.
- `switch`: `height: 28`, `min-width: 52` when neither `width` nor `min-width` is set.
- `slider`: `height: 36`, `min-width: 160`.
- `checkbox`, `radio`: `min-height: 24`.
- `progress`: `width: 100%`, `height: 6`.
- `.checkbox-box`, `.radio-dot`: 18 by 18.
- `.split-pane`: `min-width: 0`, `min-height: 0`.
- `.split-gutter`: `flex-basis: 6`, neither growing nor shrinking.
- `.split-gutter-row`: `cursor: col-resize`; `.split-gutter-column`:
  `cursor: row-resize`.

`<input>` and `<textarea>` also default to `overflow: hidden` on both
axes unless you author `overflow`, `overflow-x`, or `overflow-y`.

## Containers

### `<root>`

The document element. Lays out as a column and fills the window.

| Attribute | Value | Effect |
| --- | --- | --- |
| `skin` | `default`, `macos`, `windows`, `linux`, `auto` | Loads a built-in skin beneath your CSS. `auto` picks the skin for the current OS. |
| `frameless` | boolean | Removes the OS window frame. Pair with `<title-bar>`. |

In a multi-page app, `skin` and `frameless` are read from the home page only.
Every other attribute on a page's `<root>` styles that page. See
[multi-page apps](../guides/pages.md).

### `<column>`

Stacks children vertically.

### `<row>`

Stacks children horizontally.

### `<tile>`

A plain box with no layout defaults. The usual choice for a styled
surface.

### `<div>`

Identical to `<tile>`. Both are boxes with no per-tag defaults.

### `<spacer>`

A box that defaults to `grow="1"`, so it absorbs the remaining space on
the main axis and pushes its neighbours apart.

### `<scroll>`

A scroll container. Defaults to a vertical column scroller; `scroll="x"`
switches to a horizontal row scroller and `scroll="both"` enables both
axes. Accepts `sensitivity`, `inertia`, `bind-scroll`, and the
`scrollbar-*` attributes. A scroll container is a layout boundary.

Its automatic minimum size is zero on both axes, as for
`overflow: scroll` in CSS: a `<scroll grow="1">` in a fixed-height
column stays at the height it is given and scrolls the rest instead of
growing to its content. Write `min-height` (or `min-width`) to set a
floor.

Its direct children default to `shrink: 0`, so content taller (or wider)
than the container keeps its own size and scrolls instead of being
squeezed to fit. Write `shrink: 1` on a child to opt back in.

### `<overlay>`

Floats out of normal flow: `position: absolute` with all four insets at
`0`, so it covers its parent's box. It is not reparented; it stays where
it is written, which is what a dropdown anchored to its trigger wants.
Lays out as a column. Use it for backdrops, dropdowns, and floating
panels.

An `<overlay>` under a parent that is content-sized and has no in-flow
children covers nothing, because that parent measures zero. See
[Positioning](css.md#positioning).

### `<split>`

Panes the user resizes. Each child is a pane, and a gutter goes between
every two of them; dragging a gutter with the pointer moves the boundary
between the two panes beside it. A split a script or a plugin builds, or
adds panes to, gets its gutters the same way, and loses the gutter beside a
pane it takes out.

```html
<split class="workspace">
  <column class="tools" min-width="120"/>
  <column class="canvas" grow="1"/>
  <column class="layers" min-width="160"/>
</split>
```

The panes lay out along the split's `flex-direction`: a row unless a style
sets `flex-direction: column`, which stacks them and makes the gutters
horizontal. A gutter never takes a pane below its `min-width`
(`min-height` in a column) or past its `max-width` (`max-height`). The
built-in floor makes every pane's own minimum zero, so a pane without a
`min-width` can be dragged down to nothing.

A gutter is focusable and is a `separator`, the web's window splitter.
While it is focused, ArrowLeft and ArrowRight (ArrowUp and ArrowDown in a
column) move it by 10 px, Home gives the pane before it its smallest size
and End its largest. It carries `aria-orientation` (`vertical` between
panes side by side), and `aria-valuenow`, `aria-valuemin` and
`aria-valuemax`: the share of its two panes the pane before it takes, in
percent, and the least and the most the panes' limits let it take.

A resize sets the panes' `flex-basis`, holding it over what the styles
say. The first resize holds every pane at the size it has, and later ones
move size from one neighbour to the other, so the panes' total stays the
same. The panes keep their own `grow` and `shrink`, so when the split
itself changes size they share the difference as their styles say. The
sizes are not saved or bound to a signal; a plugin reads and sets them, and
hears when the user ends a resize (see
[Composing the built-in widgets](../contributing/plugins.md#composing-the-built-in-widgets)).

The panes carry the class `split-pane` and the gutters `split-gutter`, and
every skin styles the gutters by that class, `:hover` and `:focus`
included. A gutter also carries the class of its split's axis,
`split-gutter-row` or `split-gutter-column`, which follows the split's
`flex-direction` as it lays out; the pointer over a gutter shows
`col-resize` in a row and `row-resize` in a column. The gutter's thickness
is its `flex-basis`, 6 by default. A
`<for>` or an `<if>` written as a child of the split is one pane. On the web
the split, its panes and its gutters are the same elements, and the page's
app does the dragging and the keys.

## Text and media

### `<label>`

Text. Carries no layout defaults of its own; the text attributes above
control its appearance.

### `<a>`

An anchor. Clicking it navigates the app to another page. An anchor takes
the pointer with or without a background, and takes focus when clicked,
but it is not in the Tab chain unless you give it `tab-index="0"`.

| Attribute | Value | Effect |
| --- | --- | --- |
| `href` | page path | Target page, resolved against the app's `.lmn` files at navigation time. |

See [Pages](../guides/pages.md).

### `<image>`

Loads and draws an image file.

| Attribute | Value | Effect |
| --- | --- | --- |
| `src` | path | Image file to decode: a relative path resolves against the app directory, an absolute one is used as written. Accepts `{...}` placeholders; a filled-in path resolves the same way once it holds its value, and a new value loads the new file. |
| `alt` | text | What the image shows, for a reader who is not looking at it. Write `alt=""` for an image that carries no meaning of its own, such as a divider. `translatable` on the same element resolves it through `<key>.alt`. Carried into the compiled app; the desktop accessibility tree does not read it yet. |
| `fit` | `fill`, `cover`, `contain`, `none`, `scale-down` | How the image fills its box. |

On the web an `<image>` is emitted as an `<img>` carrying the size of the
file the build copied, `decoding="async"`, and `loading="lazy"` on every
image after the first one on the page. See
[the web target](../guides/web.md#what-the-browser-does-itself).

### `<canvas>`

A surface a script or a Rust app draws on: paths, rectangles, arcs, text,
and pixel buffers. The drawing functions come from the `canvas` namespace; see
[Rhai](scripting-rhai.md), [Lua](scripting-lua.md), or
[candela](scripting-candela.md) for the catalog. A Rust SDK app draws through
the `lumenui-canvas` crate instead; see [Canvas](ffi.md#canvas).

The tag comes from the `lumen-canvas` module, so an app declares it before
it can write one:

```toml
[dependencies]
lumen-canvas = { bundled = true }
```

In a page, the same declaration brings the module's
[web half](web-addons.md), which draws the element with the browser's own
canvas.

| Attribute | Value | Effect |
| --- | --- | --- |
| `id` | text | Names the canvas. Every drawing call takes it as its first argument, so a canvas without one cannot be drawn on. |
| `width` | px | Drawing width, in canvas units. Defaults to 300. |
| `height` | px | Drawing height, in canvas units. Defaults to 150. |

`width` and `height` say what coordinates a script draws in. They are also
the element's default size, and layout is free to give it a different box:
`flex-grow`, a percentage width, or a min or max size all do. When the box
differs, the drawing is scaled onto it, so one script draws the same picture
at any size. An axis with no declaration draws in 300 units across or 150
down, which is what an HTML canvas has always defaulted to.

Setting `width` in CSS changes the drawing space, because a `width` in CSS
and a `width` in markup are the same declaration by the time the element
exists. That is a difference from the HTML canvas, where the attribute is the
drawing space and CSS scales it. The declaration is read once, when the
element is first laid out, so a later theme or media change does not resize
the drawing space under a script that is mid-drawing.

The drawing space is the element's, so a script reads it once the element is
mounted: `on_ready`, not `on_start`. Drawing from `on_start` works either
way; the calls are kept and appear when the element does.

A canvas is retained. What a script draws stays until `canvas::clear`, or
until `canvas::resize`, which empties it the same way writing `width` on an
HTML canvas does. Drawing outside the element's box is clipped.

A canvas composites like the HTML canvas: every `globalCompositeOperation`
(the Porter-Duff operators and the blend modes), `clip` and `clear_rect`, and
`get_image_data` to read what it composed back into a pixel buffer. A
composite operation works on the canvas's own pixels, never on the page
behind the element. In a web build the module's web half draws the same
calls with the browser's Canvas 2D.

Not here yet: gradients, patterns, shadows, `measure_text`, `stroke_text`,
text alignment and baselines, `ellipse` and `arc_to`, dashed strokes, drawing
an image file or another canvas directly, and a per-canvas device pixel
ratio. On the GPU renderer, `copy`, `source-in`, `source-out`,
`destination-in` and `destination-atop` under a clip also clear a band up to
16 pixels wide just outside the clip's edge.

### `<surface>`

Shows the frames another renderer draws on the GPU: a game or an engine in
another process, an engine linked into the app, a video decoder. The frames
are shared, not copied: the renderer shows each one in place, beneath the
window's other content. Input on the element goes back to the producer. See
[Showing another renderer's frames](../guides/surfaces.md) for the whole
picture, and the `surface` namespace in [candela](scripting-candela.md#surfaces),
[Rhai](scripting-rhai.md#surfaces) or [Lua](scripting-lua.md#surfaces) for
the functions.

The tag comes from the `lumen-surface` module:

```toml
[dependencies]
lumen-surface = { bundled = true }
```

| Attribute | Value | Effect |
| --- | --- | --- |
| `id` | text | Names the surface for the `surface` functions. |
| `channel` | text | The channel a producer connects on. Defaults to the `id`. |
| `pacing` | `paced` or `free` | `paced` (the default): the producer renders one frame for each frame the surface shows, while it can be seen. `free`: the producer renders at its own rate and the surface shows the newest frame. |
| `stall-timeout` | integer milliseconds | How long a frame the surface asked for may take before the surface counts as stalled. Defaults to 1000. |
| `keyboard` | `focus` or `all` | `focus` (the default): while the surface has focus, keys go to the producer except Tab and the menu bar's shortcuts. `all`: every key goes to the producer while it has focus. |

The element is a replaced element, 300 by 150 until CSS sizes it, and asks
its producer for frames at the size of its box in device pixels, so each
frame pixel lands on one screen pixel. `object-fit` places a frame whose size
differs (for a frame or two during a resize, or a producer that renders at a
fixed size); the default is `contain`. `image-rendering: pixelated` samples
the nearest frame pixel. The background shows before the first frame and in
the bars `contain` leaves; it defaults to the `--lumen-surface-background`
custom property, black. The element can take focus, and a click gives it
focus.

The element's state is its `state` attribute and a matching class, so CSS
styles each state:

| State | Class | Meaning |
| --- | --- | --- |
| `waiting` | `surface-waiting` | No producer, or none has shown a frame yet. |
| `live` | `surface-live` | Frames arrive. |
| `stalled` | `surface-stalled` | A frame was asked for and has not come within `stall-timeout`. |
| `lost` | `surface-lost` | The producer went away. The last frame stays up. |
| `unsupported` | `surface-unsupported` | The app renders without a GPU, or in a browser, and cannot show shared frames. |

Events, on the element: `surfaceconnect` (detail `engine`, `pid`),
`surfaceresize` (`width`, `height`, the size asked of the producer),
`surfacestall` (`waited_ms`), `surfaceresume`, `surfacedisconnect` (`reason`:
`exit`, `crash`, `hung`, `rejected`, `device-lost` or `closed`; `detail`),
`surfaceerror` (`reason`: `rejected`, `import`, `unsupported` or `effect`;
`detail`) and `surfacemessage` (`text`, `bytes`).

Children of a surface paint over the frame, so an app draws overlays (labels,
selection boxes, toolbars) as ordinary elements inside it. They take the
pointer first; `pointer-events: none` passes it through to the surface.

The frame is shown as it is: opaque, upright and unblended. Clipping by an
ancestor's `overflow: hidden` and content painted over it work. Under an
`opacity` below 1 (the surface's own or a fading ancestor's) or a
`mask-image`, the frame is not drawn and the element raises `surfaceerror`
with the reason `effect`.

In a web build the element is a placeholder in the `unsupported` state, and
`lumenc web` says so when an app uses it.

Surfaces work on Linux. On Windows a surface has no endpoint, so only an engine
linked into the app can feed it, and on macOS a producer in another process is
refused.

## Controls

### `<button>`

A focusable, clickable box. Focusable by default (`tab-index="0"`).

| Attribute | Value | Effect |
| --- | --- | --- |
| `default` | boolean | Marks the default button of the containing `<dialog>`: Enter anywhere in the dialog activates it, and closing through it takes the accepted path. Also adds the `default` class. |

A button holds its label itself rather than in a child element, so
`justify="center"` is what centres the text.

### `<input>`

A single-line text field. Focusable by default. Starts with an empty
buffer, so a text child is ignored; use `value` (as in HTML) or `text` for
an initial value.

| Attribute | Value | Effect |
| --- | --- | --- |
| `placeholder` | text | Shown while the field is empty. Accepts `{...}` placeholders. `translatable` on the same element resolves it through `<key>.placeholder`. |
| `multiline` | boolean | Accept newlines. Defaults to false. |
| `pattern` | text | The value is valid only if it contains this literal substring. Not a regex. Values starting with `shape:` are reserved for the built-in checks `<date-picker>` and `<time-picker>` attach. |
| `required` | boolean | The value is valid only if it is non-empty. |
| `autofocus` | boolean | Takes focus when the containing `<dialog>` opens. |
| `password-character` | one character | Masks the rendered value. |
| `value` | text | The initial text, or the initial number or color of a typed field. |
| `type` | `text`, `number`, `color` | What the field holds. Defaults to `text`. Any other value is a text field, as it is in a browser, and the parser warns. |

`min` and `max` also feed validation on `<input>` and `<textarea>`.

#### `type="number"`

A field for a number, with the attributes and behaviour of HTML's number
input.

| Attribute | Value | Effect |
| --- | --- | --- |
| `value` | number | The initial number. |
| `min`, `max` | number | The range the steps stay in, and the range a typed value must be in to be valid. |
| `step` | number, `any` | The step. Defaults to `1`. `any` steps by one and accepts any number. |

ArrowUp and ArrowDown step the focused field, so does the wheel while the
pointer is over the focused field (the page under it does not scroll then),
and so do its `-` and `+` buttons. Each step writes the new number into the
field as an edit, so `input` handlers and a bound signal see it. A step works
the way a browser's does: a value between steps moves to the next step in
that direction, a value on a step moves by one step, and the result stays
within `min` and `max`. Steps start at `min`, or at 0 without one, and an
empty field steps from 0.

The field holds the number as text. It is valid while it is empty (unless
`required`) or holds a number within `min` and `max` that is a whole number
of steps from the start, and invalid otherwise. Validity lands in the
`valid:<id>` signal. Typing is not filtered: a letter is accepted and leaves
the field invalid, where a browser refuses it.

The field carries the class `number-input`. Its step buttons sit in a
`.number-steps` box along its right end, which the field keeps clear of its
text, and are `.number-step-down` and `.number-step-up`. On the web the field
is the browser's own `<input type="number">`, with its own step control and
none of these parts.

#### `type="color"`

A color field, with the value and events of HTML's color input: it holds
one sRGB color, written as HTML writes a color well's value. Without
`alpha` that is `#rrggbb` in lowercase. With `alpha` the color has an alpha
too and is written `color(srgb r g b / a)`, each component from 0 to 1, the
`/ a` left out for an opaque color: `color(srgb 0 1 0 / 0.5)`.

| Attribute | Value | Effect |
| --- | --- | --- |
| `value` | color | The initial color. Defaults to opaque black. |
| `alpha` | boolean | The alpha can be picked too, and the value carries it. Without it every color is made opaque. |
| `bind-value` | signal | Holds the value: the field writes it on every change and follows it when a script writes it. A signal nothing wrote yet starts at the field's color. |

A `value`, a typed color and a color a script writes to the bound signal are
read as `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` or `color(srgb r g b / a)`.
Anything else is opaque black, as a browser reads a value it cannot parse.
Named colors and `rgb()` are not read.

The field is a compact face showing the color. A click, Enter or Space opens
its panel below it (above it near the bottom of the window), and a click
outside the field or Escape closes it. In the panel, the square picks
saturation (across) and brightness (down) at the current hue, the strip
below it picks the hue, with `alpha` a second strip picks the alpha from
clear (left) to opaque (right), and the text field takes a color typed as
`#rrggbb` or `rrggbb` on Enter, or `#rrggbbaa` with `alpha`. Every change
raises `input` with the new value, and closing the panel on a color other
than the one it opened with raises `change`, as a browser's picker does.

Every part is styled by class. The face is `.color-input` and its swatch
`.color-swatch`; the panel is `.color-panel`, holding `.color-area` with its
`.color-area-thumb`, `.color-hue` with its `.color-hue-thumb`, with `alpha`
the `.color-alpha` strip (its backdrop) holding the `.color-alpha-fill`
shading and the `.color-alpha-thumb`, and the `.color-hex` field. The area's
shading and the hue strip are gradients in the user-agent sheet. On the web
the field is the browser's own `<input type="color">`, which opens the
browser's or the system's picker, and none of these parts are in the page.
The page's input carries `alpha`, which a browser that does not implement it
ignores: Chrome today shows an opaque picker and reports `#rrggbb`.

### `<textarea>`

Same as `<input>` with `multiline` defaulting to true.

### `<toggle>`

A checkbox-style boolean control rendered as a track with a knob.
Focusable by default.

| Attribute | Value | Effect |
| --- | --- | --- |
| `checked` | `true`, `yes` | Initial state. |

Style the checked track with `:checked { bg: ... }` and the knob with
`knob-color` and `knob-inset`.

### `<switch>`

The same boolean control in switch presentation: a narrower pill track
with a sliding thumb. It reports itself to assistive technology as a
switch. Same attributes as `<toggle>`.

### `<slider>`

A draggable value control. Focusable by default.

| Attribute | Value | Default |
| --- | --- | --- |
| `value` | number | `min` |
| `min` | number | `0` |
| `max` | number | `1` |
| `step` | number | `(max - min) / 100` |

Arrow keys move by one step, Page Up and Page Down by ten. Style the
thumb with `knob-color` and `thumb-size`.

A click or a drag lands on the same positions the arrow keys reach: the
value is `min` plus a whole number of steps. A range that is not a whole
number of steps stops on the last step that fits, so `min="0" max="100"
step="30"` tops out at 90. The value carries no more decimal places than
`min` and `step` do, so `min="0.8" step="0.1"` writes `0.9` to a bound
signal.

### `<checkbox>`

A row containing an indicator box and a caption. Focusable by default.
The parser synthesizes the two parts, so CSS can reach them as
`.checkbox-box` and `.checkbox-label`. On the web the indicator is the
browser's own control and carries the same class, so one rule styles
both targets.

| Attribute | Value | Effect |
| --- | --- | --- |
| `label` | text | The caption. Text content works too. |
| `checked` | `true`, `yes` | Initial state. |
| `indeterminate` | boolean | Tri-state dash until the first user toggle. |

Defaults to `align="center"` and `gap="8"` when you do not set them.

### `<radio>`

One member of an exclusive group. The group's selected value lives in a
signal. Synthesized parts are `.radio-dot` and `.radio-label`; on the web
the dot is the browser's own control under the same class.

| Attribute | Value | Effect |
| --- | --- | --- |
| `group` | signal name | Required. The signal holding the group's selected value. |
| `value` | text | Required. This member's value. Unlike other tags, `value` is a string here. |
| `label` | text | The caption. |
| `checked` | `true`, `yes` | Seeds the group signal with this member's value. |

A radio group is one Tab stop, and the arrow keys move within it. On the
desktop Lumen keeps exactly one member of each group in the Tab order; on
the web the members share the browser's own `name` group, and the browser
does the same. A `tab-index` written on a `<radio>` has no effect on either
target, because the group decides where its members sit in the Tab order.
An empty `group` or `value` puts no `name` or `value` on the browser's
control, so on the web a radio with an empty `group` belongs to no group.

### `<progress>`

A non-interactive progress track. Lays out as a row and fills its
container's width. The parser synthesizes a `.progress-fill` child.

| Attribute | Value | Effect |
| --- | --- | --- |
| `value` | number | Determinate progress. Omit it for an indeterminate sweep. A bar with `bind-value` and no `value` sweeps until its signal first holds a number, then shows it. |
| `max` | number | Upper bound. Defaults to `1`. |
| `duration` | integer milliseconds | Indeterminate sweep period. Defaults to `1200`. |
| `chunk` | number 0..1 | Fraction of the track the indeterminate chunk covers. Defaults to `0.3`. |

### `<dropdown>` and `<option>`

A combobox. The parser expands it into a header button plus a floating
options panel; the panel is dismissed by clicking outside it and flips
above the trigger near the bottom of the window.

The tag stays on the box those parts sit in, so `width`, `min-width`,
`padding`, `bg` and a `dropdown` rule in a stylesheet size and paint the
control the way they do any other widget. The generated elements carry
the classes `dropdown-button` for the closed face, `dropdown-panel` for
the floating list, and `dropdown-option` for a row in it. The face fills
the box, so sizing the `<dropdown>` sizes the whole control; the face's
height and an option row's are theme metrics, so restyle those classes to
change them.

`<dropdown>` requires `bind-value`. Its children must all be `<option>`.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<dropdown>` | `bind-value` | Required. Signal holding the selected value. |
| `<dropdown>` | `placeholder` | Header text while the signal is empty. Setting it also suppresses the first-option seed, so the dropdown starts with nothing selected. |
| `<option>` | `value` | Required. Value written to the signal on click. |
| `<option>` | `label` | Display text. Defaults to `value`. |
| `<option>` | `disabled` | Unclickable, skipped by arrow navigation. |

The closed header reads the selected option's `label`. The signal holds
that option's `value`, which is what your script reads and what a click
writes, so the two differ whenever an option carries a label of its own.
A value no option declares is shown as it stands.

The first `<option>` seeds the signal, so the dropdown opens on a real
selection. Add `placeholder` to start unselected instead. A value your
script writes first always wins over the seed.

The closed dropdown handles Up and Down to step the value, Alt+Down,
Space, or Enter to open, and type-ahead.

`<option>` outside a `<dropdown>` is an error.

### `<tabs>` and `<tab>`

A tabbed container: one `<tab>` shows at a time, picked from a strip of
buttons. Every child of a `<tabs>` must be a `<tab>`.

```html
<tabs bind-value="section">
  <tab name="general" label="General">...</tab>
  <tab name="advanced" label="Advanced">...</tab>
</tabs>
```

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<tabs>` | `bind-value` | Optional. Signal holding the name of the tab that shows. |
| `<tab>` | `name` | Required. What picks the tab: the value its button writes. |
| `<tab>` | `label` | Strip button text. Defaults to `name`. |
| `<tab>` | `disabled` | Disables the tab and its button, which a click cannot pick and arrow navigation skips. A script can still show the tab. |

Which tab shows is a name. With `bind-value` the signal holds it; without,
the `<tabs>` element's own `value` attribute does, so a script reads it with
`get_attr("value")` and switches tabs with `set_attr("value", name)`. A
`<tabs>` opens on the tab the name picks, or on its first enabled tab when
the name picks none, and an empty signal is set to that tab's name.

A click on a strip button, or an arrow key, Home, End, Enter or Space on a
focused one, shows that button's tab and raises a `change` event on the
`<tabs>` whose `value` is the tab's name. Arrow keys move along the strip
and pick as they go, and stop at either end. A tab the script or a binding
shows raises no `change`.

The strip goes in front of the tabs: the `<tabs>` holds a `.tab-strip` row
of `.tab-btn` buttons, one per tab in the tabs' order, then the tabs. The
shown tab's button is `:selected`. A hidden tab keeps its content, so focus,
scroll and text survive switching. A tab's content mounts the first time the
tab shows, so an element inside a tab nobody opened is not in the document
yet. Style the container as `tabs` and each panel as `tab`.

A `<tabs>` a script builds, with `<tab>` elements carrying `name` and
`label` attributes, gets the same strip, and the strip follows the tabs:
appending, removing or moving a `<tab>` adds, removes or moves its button,
and changing a tab's `label` relabels it.

`<tab>` outside `<tabs>` is an error. On the web the tab set is the
browser's tabs pattern: a `tablist` of `tab` buttons over `tabpanel`s.

### `<date-picker>` and `<time-picker>`

A validated text field for a date or a time. Each expands to an `<input>`
with a built-in pattern and the class `date-picker` or `time-picker`. Like
any `<input>` it is in the Tab order, and the other attributes written on
the picker (`id`, `class`, `tab-index`, `required`, sizing) pass through to
the generated input.

| Attribute | Value | Effect |
| --- | --- | --- |
| `bind-value` | signal | Required. |
| `placeholder` | text | Defaults to `YYYY-MM-DD` for dates and `HH:MM` for times. |
| `width` | length | Defaults to `180px`. |

The built-in check matches the placeholder: `YYYY-MM-DD` with month 01-12
and day 01-31 for a date, `HH:MM` with hour 00-23 and minute 00-59 for a
time. Digits and separators must sit in those positions, ignoring
surrounding whitespace. It is a shape check, not a calendar check, so
`2026-02-31` passes.

Validity lands in the `valid:<id>` signal like any other validated
field, so give the picker an `id` to read the result.

## Structure

### `<for>`

Repeats its children once per item of an array signal. Lays out as a
column.

| Attribute | Value | Effect |
| --- | --- | --- |
| `each` | array signal name | Required for iteration. Without it the children spawn once, unrepeated. |
| `key` | field name | Record field used as the reconciliation key. Without it the item index is the key. A row whose key is still in the array keeps its elements wherever the key moves, and is updated in place when its record's fields change; only rows whose keys came or went are built or removed. |
| `virtualized` | `true`, `yes` | Spawn only the rows in the visible scroll window. Needs a `<scroll>` ancestor. |
| `row-height` | number | Pixel height per virtualized row. Defaults to `32`. |

Inside the body, `{row.field}` reads the current record and `{$index}`
the row index. A virtualized `<for>` is forced to full width so its rows
have a reference box.

### `<if>`

Mounts its children conditionally.

| Attribute | Value | Effect |
| --- | --- | --- |
| `signal` | signal name | The gate. Truthy means set, non-empty, and not `false` or `0`. |
| `eq` | text | Mount only when the signal equals this value, instead of testing truthiness. |
| `mode` | `render`, `hide` | `render` (default) despawns and respawns the body on each toggle. `hide` mounts once and toggles visibility, preserving focus, scroll, and per-row state. |

While the gate is closed the `<if>` itself takes no space and paints nothing,
in either mode, so styles written on it show only with its body.

### `<template>`, `<use>`, and `<slot>`

`<template name="Card">...</template>` declares a reusable subtree.
Instantiate it as `<Card k="v"/>` or `<use template="Card" k="v"/>`.
Every `{k}` placeholder in the body is a parameter, and takes the value
the use site binds to `k`. Attributes on the `<template>` tag itself
(other than `name`) are defaults for the parameters a use site leaves
unbound; a parameter with neither reads the global signal `k`.
Placeholders resolve in attribute values and text, not in tag names.

`<slot/>` inside a template body is replaced with the content of the use
site. When the use site has none, the slot falls back to its
`default="..."` attribute and then to its own inner content.

Giving the use site an `id` prefixes every `id` inside the instantiated
body with it, so multiple instances keep distinct ids. Templates may
instantiate other templates, up to 64 levels; a cycle is an error.

A declaration is visible to the whole file, wherever it sits in it, to any
file that includes that file, and app-wide in a multi-page app.

The name of a candela component instantiates the same way: `<Home name="bob"/>`
takes the `lmn!` block the function `Home` returns. Any component can be named
this way. Where the block is the function's whole body and reads only its
parameters, the build puts it in the tree outright; where the function works a
value out or picks between blocks, the build leaves a marker there and the
runtime fills it by calling the function on the first tick. Component names and
`<template>` names share one namespace, and two
declarations claiming one name are an error. See
[composition](../guides/composition.md#naming-a-component-from-markup).

### `<include>`

`<include src="parts/header.lmn"/>` splices another file's markup in
place. Paths are relative to the including file. Includes resolve before
parsing, they nest, and a cycle is an error. Included files are watched for hot reload.

### `<script>`

Collects script source. It is not a layout element and can appear
anywhere in the tree.

| Attribute | Value | Effect |
| --- | --- | --- |
| `src` | path | Script file to load, relative to the app's `src/` directory. |

Without `src`, the tag's text content is the script. Each `src` file runs
under the host its extension names (`.cdl`, `.rhai`, `.lua`); sources of one
language are concatenated into one program. A tag with a body has no extension
to read and joins the app's one external language, or candela when there is not
exactly one. `[script] engine` in `lumen.toml` overrides all of it and puts
every source on one host. See [Scripting](../guides/scripting.md).

## Shell

### `<menubar>`

Declares the application menu. At most one per document, as a direct child
of `<root>`. Its children must be `<menu label="...">`.

On macOS and Windows it is the OS's own menu bar, and it takes no place in
the layout tree. On Linux, where desktops have no menu bar for an app to
join, and on every platform when `lumen.toml` sets
[`[window] menubar = "window"`](lumen-toml.md#window), the same markup is
drawn in the window: a bar at the top of the root,
one button per menu, each opening its menu as a popup. The bar floats over
the root's top edge and the root keeps its height clear above the content,
so the root's `justify` and `align` place the content as they would without
it. Its parts carry the classes `menubar` (the bar), `menubar-menu` (one
per menu), and `menubar-button` (the menu's label), and the menus are the
`.menu-panel` popups described below. While one of its menus is open,
moving the pointer to another button opens that menu instead.

### `<menu>`, `<menuitem>`, and `<separator>`

Inside `<menubar>`, `<menu label>` is a top-level menu and its children
are `<menuitem>` and `<separator>`.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<menu>` | `label` | Required inside `<menubar>`. Menu title. |
| `<menu>` | `id` | Required outside `<menubar>`. Names the popup. |
| `<menuitem>` | `id` | Required. Identifies the item to scripts. |
| `<menuitem>` | `label` | Display text. Defaults to `id`. |
| `<menuitem>` | `accel` | Keyboard accelerator, such as `CommandOrControl+S` or `Alt+F4`. Menubar menus only. The in-window bar runs it while the window has focus. |
| `<menuitem>` | `disabled` | Unclickable, skipped by arrow navigation. Popup menus only. |

Outside a menubar, `<menu id="m">` is an in-window popup panel toggled by
the signal `__menu_open:m`. It expands to a floating panel of buttons
with the classes `menu-panel`, `menu-item`, and `menu-separator`, and it
dismisses on an outside click.

`<menuitem>` and `<separator>` outside a menu are an error.

### `<dialog>`

A modal overlay: absolutely positioned over its parent's box, centered,
with its children as content. Focus is trapped inside it while it is
open. Write it as a direct child of `<root>`, which is what makes it
cover the window; under a content-sized parent it covers nothing (see
[Positioning](css.md#positioning)).

| Attribute | Value | Effect |
| --- | --- | --- |
| `open` | signal name | Shows the dialog while the signal is truthy. The body is mounted once and hidden, so its state survives. |

Inside a dialog, `autofocus` picks the element that takes focus on open,
and `<button default="true">` is the default button.

### `<tooltip>`

Wraps exactly one trigger element and shows a popup after the pointer
dwells on it. The pointer dwells on the trigger while it is over the
trigger or anything inside it, so a button's tooltip stays up while the
pointer is over the button's icon. When tooltips are nested, only the
innermost one under the pointer shows. The wrapper collapses at parse time,
so CSS selectors apply to the trigger, not to `<tooltip>`.

| Attribute | Value | Default |
| --- | --- | --- |
| `text` | text | empty |
| `translatable` | catalogue key | none; the popup body is the key's message, falling back to `text` |
| `delay` | integer milliseconds | `500`, or the `--lumen-tooltip-delay` custom property |
| `offset` | number | `12`, or the `--lumen-tooltip-offset` custom property |

Wrapping zero or more than one element is an error.

The popup is a `tooltip` element with the class `tooltip`, placed under
the root, so a `.tooltip` rule styles it like any element: background,
text color, padding, radius, border, shadow, and font. It inherits from the
root, which puts the root's custom properties in scope, and it paints above
everything else. Its position is the runtime's: it opens beside the pointer
and moves to the pointer's other side near the window's edges, so `inset`
and `position` in a `.tooltip` rule have no effect. The pointer passes
through it to whatever is underneath, so moving onto the popup leaves the
trigger hovered and the popup open. Scripts do not see it, and it does not
count as one of the root's children for `:last-child` and the other
structural selectors.

```css
.tooltip { bg: var(--accent); text-color: #ffffff; padding: 4 8; radius: 4; }
```

In a web build the tooltip is the trigger's `title` attribute, which the
browser shows in its own look.

### `<title-bar>`

A title-bar region for frameless windows. Lays out as a row, full width,
32 px tall.

| Attribute | Value | Effect |
| --- | --- | --- |
| `drag` | boolean | Pressing and moving the bar requests a native window drag. |

Pair it with `<root frameless="true">`.

### `<dock>` and its panels

A workspace of panels the user arranges: tab stacks in resizable splits,
panels shown, hidden, collapsed, maximized, popped out into windows of their
own and docked back. A `<dock-panel>` outside any dock is a window the app
opens by showing it. The tags come from the `lumen-dock` module, so an app
declares it first:

```toml
[dependencies]
lumen-dock = { bundled = true }
```

```html
<dock id="editor" label="Editor">
  <dock-split axis="row">
    <dock-stack basis="240">
      <dock-panel id="files" title="Files">...</dock-panel>
    </dock-stack>
    <dock-stack center="true">
      <dock-panel id="code" title="main.rs" closable="false">...</dock-panel>
    </dock-stack>
  </dock-split>
  <dock-panel id="profiler" title="Profiler" place="right">...</dock-panel>
  <dock-layout name="Focus">
    <dock-stack center="true"><dock-ref panel="code"/></dock-stack>
  </dock-layout>
</dock>
```

The markup declares the panels and the default layout; the `dock` functions
change it at run time (see [candela](scripting-candela.md#docks-and-windows),
[Rhai](scripting-rhai.md#docks-and-windows) or
[Lua](scripting-lua.md#docks-and-windows)). The `<dock-split>`,
`<dock-stack>` and `<dock-layout>` elements describe the layout and leave the
tree once it is read; the dock builds its stacks and splits out of
[`<tabs>`](#tabs-and-tab) and [`<split>`](#split), so their keys, gutters and
accessibility roles are those widgets' own. The panels are kept, and move
wherever the layout puts them with everything inside them.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<dock>` | `id` | Required. Names the dock to the `dock` functions and in a saved layout. |
| | `label` | The accessible name of the workspace. |
| `<dock-split>` | `axis` | `row` (the default) or `column`. |
| | `basis` | Size in its parent split, in pixels. |
| | `grow` | Share of the split's spare room. Defaults to 1 with no `basis`, 0 with one. |
| `<dock-stack>` | `center` | Boolean. Kept when it empties, and the stack a panel with no other place goes to. |
| | `collapsed` | Boolean. Shows only its header. |
| | `tabs` | `auto` (the default), `always` or `never`. |
| | `active` | The panel showing at first; the first one otherwise. |
| | `basis`, `grow` | As on a split. |
| `<dock-panel>` | `id` | Required. The panel's name in every call and in a saved layout. |
| | `title` | Its tab text and accessible name. Defaults to the `id`. A script that changes it changes the tab and the window title with it. |
| | `icon` | An image shown in its tab before the title, as `<image src>` reads it. |
| | `closable` | Boolean, default true. False takes away its close button and Delete. |
| | `movable` | Boolean, default true. False keeps its tab from being dragged; the tab menu and the `dock` functions still move it. |
| | `hidden` | Boolean. Starts hidden, with its place remembered. |
| | `place` | Where it goes the first time it shows, when no layout places it: `center` (the default), `left`, `right`, `top`, `bottom` (a new stack along that edge of its dock), `window` (a window of its own), or `beside:<panel>` (a tab beside another panel). |
| | `destroy-on-close` | Boolean. Closing removes the element instead of hiding it. |
| | `kind`, `state` | The app's names for what the panel is, carried in a saved layout so a restore can ask for it again. |
| | `dockable` | Boolean, default true. False keeps it in a window of its own that takes no other panel. |
| | `window-kind` | `normal` (the default) or `dialog`: a dialog stays above the main window and minimizes with it. |
| | `modal` | `none` (the default), `parent` (blocks input to the main window) or `app` (blocks every other window). |
| | `window-size` | Inner size of its own window in pixels, `"640 480"`. Defaults to the size it has where it is. |
| | `resizable` | Boolean, default true, for its own window. |
| | `window-title` | Its own window's title. Defaults to `title`. |
| | `bind-window-title` | A signal its own window's title follows, ahead of `window-title`: a document window titled with the file it shows. |
| `<dock-layout>` | `name` | A named layout the app can apply. Its tree uses `<dock-ref panel>` in place of panels; a panel it does not name keeps its place. |
| `<dock-window>` | `rect` | In a `<dock-layout>`: a window of its own at `"x y width height"`, holding a stack or a split. |

A `<dock-panel>` written straight under `<dock>`, outside any stack, waits
hidden until shown. Panels are hidden, not unmounted, so a panel's content
keeps its state while it is out of view. A `<dock>` inside another dock's
panel is not supported; `lumenc check` refuses it.

Events, each a [custom event](scripting-candela.md#element-events) with
these fields in its detail:

| Event | Raised on | When | Detail |
| --- | --- | --- | --- |
| `panelshow`, `panelhide` | panel | It enters or leaves view. | `reason`: `user`, `api` or `layout` |
| `panelactivate` | panel | It becomes its stack's showing tab. | `reason` |
| `panelfocus` | panel | Keyboard focus enters it. | |
| `panelclose` | panel | Before the user closes it. Cancelable: `prevent_default` keeps it open. | `reason` |
| `panelmove` | panel | It moved between a dock and a window, or into or out of view. | `from`, `to` (`dock`, `window`, `hidden`), `reason` |
| `windowfocus`, `windowblur` | panel | Its window gains or loses focus. Does not bubble. | |
| `layoutchange` | dock | The arrangement changed. | `reason` |
| `layouterror` | dock | A saved layout was not used. | `reason` (`parse`, `format`, `version`, `invalid`), `detail` |
| `panelrestore` | dock | A layout names a panel the app has not created. | `panel`, `kind`, `state` |

A right click on a tab, or the Menu key or Shift+F10 on a focused tab, opens
the tab menu: Move to new window, Move group to new window (when the stack
has other panels), Maximize or Restore layout, Dock back (in a window of its
own), and Close (when the panel is closable). Its items are `<menu>` items
with the ids `lumen-dock-detach`, `lumen-dock-detach-group`,
`lumen-dock-maximize`, `lumen-dock-restore`, `lumen-dock-attach` and
`lumen-dock-close-panel`, so a script's `on_menu` hears them too. A completed
move is announced to screen readers: "Inspector moved to a new window",
"Inspector docked back", "Inspector moved beside Scene". The menu labels and
the announcements are the module's messages (`lumen-dock-move-to-window`,
`lumen-dock-move-group-to-window`, `lumen-dock-maximize`, `lumen-dock-restore`,
`lumen-dock-dock-back`, `lumen-dock-close`, `lumen-dock-moved-to-window`,
`lumen-dock-docked-back`, `lumen-dock-moved-beside`, `lumen-dock-moved`),
which an app rewords or translates by writing the same ids in its own
catalogue.

Dragging a tab moves its panel: between two tabs, onto the middle of a
stack, beside a stack on one of its edges, or along an edge of the whole
dock; dragging a strip away from its tabs moves the whole stack. Past the
window's edge the tab tears off into a window of its own, which follows the
pointer where the platform lets it (not on Wayland compositors without the
toplevel-drag protocol, where the window opens on release instead); a window
of one panel dragged by its tab docks into another window's dock. Escape
cancels a drag, and a screen reader hears `lumen-dock-drag-cancelled`.

Signals: `dock.active` holds the id of the panel holding focus,
`dock.layout` the named layout in use, and `dock.shown.<panel>` whether a
panel is in view.

Classes for styling: `dock` on the dock, `dock-split` with `dock-split-row`
or `dock-split-column`, `dock-stack` with `center`, `collapsed`, `single`
(one panel), `no-tabs` and `maximized`, `dock-tab` on a tab (with `active`
on the shown one) with `dock-tab-icon`, `dock-tab-label` and
`dock-tab-close` on its icon, title and close button, `dock-panel` on every panel, `dock-window` with `dock-window-normal` or
`dock-window-dialog` on the root of a window of panels, and while a tab
drags: `dock-dragging` on the dock, `dock-drag-ghost` on the ghost following
the pointer, and `dock-drop-preview` with one of `dock-drop-tab`,
`dock-drop-center`, `dock-drop-left`, `dock-drop-right`, `dock-drop-top`,
`dock-drop-bottom` or `dock-drop-root-<side>` on the region a drop fills. The default look reads
`--lumen-dock-*` tokens with the skin's own behind them:

| Token | Styles |
| --- | --- |
| `--lumen-dock-stack-bg`, `--lumen-dock-border` | A stack's background and its border, the strip's bottom edge too. |
| `--lumen-dock-strip-bg`, `--lumen-dock-strip-padding`, `--lumen-dock-tab-gap` | The tab strip. |
| `--lumen-dock-tab-height`, `--lumen-dock-tab-padding-start`, `--lumen-dock-tab-padding`, `--lumen-dock-tab-radius`, `--lumen-dock-tab-font-size` | A tab's size and shape; the padding before its title and after its close button. |
| `--lumen-dock-tab-bg`, `--lumen-dock-tab-hover-bg`, `--lumen-dock-tab-text` | A tab at rest and under the pointer. |
| `--lumen-dock-tab-active-bg`, `--lumen-dock-tab-active-text`, `--lumen-dock-accent` | The shown tab and the line along its top edge. |
| `--lumen-dock-icon-size` | A tab's icon. |
| `--lumen-dock-close-bg`, `--lumen-dock-close-hover-bg` | A tab's close button. |
| `--lumen-dock-window-bg` | The root of a window of panels. |
| `--lumen-dock-ghost-bg`, `--lumen-dock-ghost-radius` | The ghost a dragged tab shows. |
| `--lumen-dock-drop-bg`, `--lumen-dock-drop-border`, `--lumen-dock-drop-radius` | The region a drop fills, and the marker between two tabs. |
| `--lumen-dock-tearoff-distance` | How far past the window's edge a dragged tab goes before it tears off. Default 24. |

In a page the dock shows its default layout, with tabs that switch; the
functions that rearrange it leave it as it is. See
[Docks and windows](../guides/docks.md).

## Custom tags

A Rust plugin can register additional tags, which then parse like
built-in ones. An element written with a `#[derive(Widget)]` tag becomes that
widget, its props read from the element's attributes. See
[Authoring plugins](../contributing/plugins.md#custom-tags).
