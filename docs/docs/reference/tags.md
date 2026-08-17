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
| color | `#rrggbb`, `#rrggbbaa` | Named colours and `rgb()` are not accepted. |
| gradient | `linear-gradient(...)`, `radial-gradient(...)`, `conic-gradient(...)` | `bg` only. See [CSS values](css.md#gradients). |
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

`{...}` placeholders are substituted in `text`, `placeholder`, `src`,
`id`, `class`, `style`, and `drag-payload`:

| Placeholder | Resolves to |
| --- | --- |
| `{$name}`, `{name}` | The global signal `name`. |
| `{row.field}` | A field of the current `<for>` record. |
| `{$index}` | The current `<for>` row index. `{idx}` is the older spelling. |

Bare `{name}` still works; the compiler emits an informational lint
suggesting the explicit form. Placeholders that match nothing resolve to
an empty string.

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
| `tab-index` | integer | Keyboard focus order. `-1` removes the element from Tab order. |
| `dir` | `ltr`, `rtl`, `auto` | Writing direction; inherited by descendants. |
| `lang` | BCP-47 tag | Language for shaping, accessibility, and formatters; inherited. |
| `translatable` | catalogue key | Resolves the element's text through the loaded translation catalogue. `lumenc i18n extract` collects these keys. |

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
| `inset` | edges | `0` on `<overlay>` and `<dialog>`, otherwise auto |

### Layout

| Attribute | Value | Default |
| --- | --- | --- |
| `align` | `start`, `end`, `center`, `stretch` | `stretch` |
| `justify` | `start`, `end`, `center`, `between` (`space-between`), `around` (`space-around`), `evenly` (`space-evenly`) | `start` |
| `position` | `relative`, `absolute` | `relative` |
| `z-index` | integer | paint in document order |
| `overflow`, `overflow-x`, `overflow-y` | `visible`, `hidden`, `scroll` | `visible` |
| `layout-boundary` | boolean | Auto: true for scroll containers and for elements with both a fixed width and a fixed height |

Flex direction comes from the tag, not an attribute: `<row>` lays out
horizontally, `<column>` vertically. Use CSS `flex-direction` to change
it, including the reverse variants.

`overflow: scroll` on any element makes it a live scroll container, the
same as `<scroll>`.

### Colour and paint

| Attribute | Value | Default |
| --- | --- | --- |
| `bg` | color or gradient | transparent |
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

`<input>` and `<textarea>` also default to `overflow: hidden` on both
axes unless you author `overflow`, `overflow-x`, or `overflow-y`.

## Containers

### `<root>`

The document element. Lays out as a column and fills the window.

| Attribute | Value | Effect |
| --- | --- | --- |
| `skin` | `default`, `macos`, `windows`, `linux`, `auto` | Loads a built-in skin beneath your CSS. `auto` picks the skin for the current OS. |
| `frameless` | boolean | Removes the OS window frame. Pair with `<title-bar>`. |

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

### `<overlay>`

Floats out of normal flow: `position: absolute` with all four insets at
`0`, so it covers its nearest positioned ancestor. Lays out as a column.
Use it for backdrops, dropdowns, and floating panels.

## Text and media

### `<label>`

Text. Carries no layout defaults of its own; the text attributes above
control its appearance.

### `<a>`

An anchor. Clicking it navigates the app to another page.

| Attribute | Value | Effect |
| --- | --- | --- |
| `href` | page path | Target page, resolved against the app's `.lmn` files at navigation time. |

See [Pages](../guides/pages.md).

### `<image>`

Loads and draws an image file.

| Attribute | Value | Effect |
| --- | --- | --- |
| `src` | path | Image file to decode, relative to the app directory. Accepts `{...}` placeholders. |
| `alt` | text | What the image shows, for a reader who is not looking at it. Write `alt=""` for an image that carries no meaning of its own, such as a divider. Carried into the compiled app; the desktop accessibility tree does not read it yet. |
| `fit` | `fill`, `cover`, `contain`, `none`, `scale-down` | How the image fills its box. |

## Controls

### `<button>`

A focusable, clickable box. Focusable by default (`tab-index="0"`).

| Attribute | Value | Effect |
| --- | --- | --- |
| `default` | boolean | Marks the default button of the containing `<dialog>`: Enter anywhere in the dialog activates it, and closing through it takes the accepted path. Also adds the `default` class. |

### `<input>`

A single-line text field. Focusable by default. Starts with an empty
buffer, so a text child is ignored; use `text` for an initial value.

| Attribute | Value | Effect |
| --- | --- | --- |
| `placeholder` | text | Shown while the field is empty. Accepts `{...}` placeholders. |
| `multiline` | boolean | Accept newlines. Defaults to false. |
| `pattern` | text | The value is valid only if it contains this literal substring. Not a regex. Values starting with `shape:` are reserved for the built-in checks `<date-picker>` and `<time-picker>` attach. |
| `required` | boolean | The value is valid only if it is non-empty. |
| `autofocus` | boolean | Takes focus when the containing `<dialog>` opens. |
| `password-character` | one character | Masks the rendered value. |

`min` and `max` also feed validation on `<input>` and `<textarea>`.

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

### `<checkbox>`

A row containing an indicator box and a caption. Focusable by default.
The parser synthesizes the two parts, so CSS can reach them as
`.checkbox-box` and `.checkbox-label`.

| Attribute | Value | Effect |
| --- | --- | --- |
| `label` | text | The caption. Text content works too. |
| `checked` | `true`, `yes` | Initial state. |
| `indeterminate` | boolean | Tri-state dash until the first user toggle. |

Defaults to `align="center"` and `gap="8"` when you do not set them.

### `<radio>`

One member of an exclusive group. The group's selected value lives in a
signal. Synthesized parts are `.radio-dot` and `.radio-label`.

| Attribute | Value | Effect |
| --- | --- | --- |
| `group` | signal name | Required. The signal holding the group's selected value. |
| `value` | text | Required. This member's value. Unlike other tags, `value` is a string here. |
| `label` | text | The caption. |
| `checked` | `true`, `yes` | Seeds the group signal with this member's value. |

Radios start at `tab-index="-1"`; exactly one member of each group is
promoted at runtime, so Tab enters and leaves the group as a unit and
arrow keys move within it.

### `<progress>`

A non-interactive progress track. Lays out as a row and fills its
container's width. The parser synthesizes a `.progress-fill` child.

| Attribute | Value | Effect |
| --- | --- | --- |
| `value` | number | Determinate progress. Omit it (and any `bind-value`) for an indeterminate sweep. |
| `max` | number | Upper bound. Defaults to `1`. |
| `duration` | integer milliseconds | Indeterminate sweep period. Defaults to `1200`. |
| `chunk` | number 0..1 | Fraction of the track the indeterminate chunk covers. Defaults to `0.3`. |

### `<dropdown>` and `<option>`

A combobox. The parser expands it into a header button plus a floating
options panel; the panel is dismissed by clicking outside it and flips
above the trigger near the bottom of the window.

`<dropdown>` requires `bind-value`. Its children must all be `<option>`.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<dropdown>` | `bind-value` | Required. Signal holding the selected value. |
| `<dropdown>` | `placeholder` | Header text while the signal is empty. Setting it also suppresses the first-option seed, so the dropdown starts with nothing selected. |
| `<option>` | `value` | Required. Value written to the signal on click. |
| `<option>` | `label` | Display text. Defaults to `value`. |
| `<option>` | `disabled` | Unclickable, skipped by arrow navigation. |

The first `<option>` seeds the signal, so the dropdown opens on a real
selection. Add `placeholder` to start unselected instead. A value your
script writes first always wins over the seed.

The closed dropdown handles Up and Down to step the value, Alt+Down,
Space, or Enter to open, and type-ahead.

`<option>` outside a `<dropdown>` is an error.

### `<tabs>` and `<tab>`

A tabbed container. The parser expands it into a button strip plus one
body per tab; bodies are mounted once and shown or hidden, so focus and
scroll state survive switching.

`<tabs>` requires `bind-value`. Its children must all be `<tab>`.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<tabs>` | `bind-value` | Required. Signal holding the active tab name. |
| `<tab>` | `name` | Required. The value written when the tab is picked. |
| `<tab>` | `label` | Strip button text. Defaults to `name`. |
| `<tab>` | `disabled` | The strip button is unclickable and skipped by arrow navigation; the body still mounts if a script activates the tab. |

The first `<tab>` seeds the signal, so it is active at startup. The
generated elements carry the classes `tabs`, `tab-strip`, and `tab-btn`.

`<tab>` outside `<tabs>` is an error.

### `<date-picker>` and `<time-picker>`

A validated text field for a date or a time. Each expands to an `<input>`
with a built-in pattern and the class `date-picker` or `time-picker`.

| Attribute | Value | Effect |
| --- | --- | --- |
| `bind-value` | signal | Required. |
| `placeholder` | text | Defaults to `YYYY-MM-DD` for dates and `HH:MM` for times. |
| `id` | text | Passed through to the generated input. |

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
| `key` | field name | Record field used as the reconciliation key. Without it the item index is the key. |
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
| `src` | path | Script file to load, relative to the app directory. |

Without `src`, the tag's text content is the script. Each `src` file runs
under the host its extension names (`.cdl`, `.rhai`, `.lua`); sources of one
language are concatenated into one program. A tag with a body has no extension
to read and joins the app's one external language, or candela when there is not
exactly one. `[script] engine` in `lumen.toml` overrides all of it and puts
every source on one host. See [Scripting](../guides/scripting.md).

## Shell

### `<menubar>`

Declares the OS-native application menu. At most one per document, as a
direct child of `<root>`. It is removed from the layout tree. Its
children must be `<menu label="...">`.

### `<menu>`, `<menuitem>`, and `<separator>`

Inside `<menubar>`, `<menu label>` is a top-level menu and its children
are `<menuitem>` and `<separator>`.

| Tag | Attribute | Effect |
| --- | --- | --- |
| `<menu>` | `label` | Required inside `<menubar>`. Menu title. |
| `<menu>` | `id` | Required outside `<menubar>`. Names the popup. |
| `<menuitem>` | `id` | Required. Identifies the item to scripts. |
| `<menuitem>` | `label` | Display text. Defaults to `id`. |
| `<menuitem>` | `accel` | Keyboard accelerator. Menubar menus only. |
| `<menuitem>` | `disabled` | Unclickable, skipped by arrow navigation. Popup menus only. |

Outside a menubar, `<menu id="m">` is an in-window popup panel toggled by
the signal `__menu_open:m`. It expands to a floating panel of buttons
with the classes `menu-panel`, `menu-item`, and `menu-separator`, and it
dismisses on an outside click.

`<menuitem>` and `<separator>` outside a menu are an error.

### `<dialog>`

A modal overlay: absolutely positioned over the viewport, centered, with
its children as content. Focus is trapped inside it while it is open.

| Attribute | Value | Effect |
| --- | --- | --- |
| `open` | signal name | Shows the dialog while the signal is truthy. The body is mounted once and hidden, so its state survives. |

Inside a dialog, `autofocus` picks the element that takes focus on open,
and `<button default="true">` is the default button.

### `<tooltip>`

Wraps exactly one trigger element and shows a popup after the pointer
dwells on it. The wrapper collapses at parse time, so CSS selectors apply
to the trigger, not to `<tooltip>`.

| Attribute | Value | Default |
| --- | --- | --- |
| `text` | text | empty |
| `delay` | integer milliseconds | `500`, or the `--lumen-tooltip-delay` custom property |
| `offset` | number | `12`, or the `--lumen-tooltip-offset` custom property |

Wrapping zero or more than one element is an error.

### `<title-bar>`

A title-bar region for frameless windows. Lays out as a row, full width,
32 px tall.

| Attribute | Value | Effect |
| --- | --- | --- |
| `drag` | boolean | Pressing and moving the bar requests a native window drag. |

Pair it with `<root frameless="true">`.

## Custom tags

A Rust plugin can register additional tags, which then parse like
built-in ones. See [Authoring plugins](../contributing/plugins.md).
