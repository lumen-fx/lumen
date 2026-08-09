# Markup tag reference

Lumen's authoring surface is a subset of XML-shaped markup with a fixed
tag vocabulary. Unknown tags fail-fast at parse time with the byte
offset. The live list is the `KNOWN_TAGS` array in
[`lumenc/src/parser_html.rs`](https://github.com/lumen-fx/lumen/blob/main/lumenc/src/parser_html.rs);
this page tracks every entry.

This page groups tags by family. For attribute semantics that work on
*every* tag (`id`, `class`, `width`, `padding`, `bg`, etc.) see the
[CSS subset](./css.md) - the same names work as inline markup attrs.

## Common attribute families

All visible tags accept the layout + visual attributes. Repeated here
only when a tag interprets them in a non-default way.

| Family | Attributes |
|---|---|
| **Layout box** | `width`, `height`, `min-width`, `min-height`, `max-width`, `max-height`, `aspect-ratio`, `padding`, `margin`, `gap`, `grow`, `align`, `justify`, `position`, `inset`, `overflow`, `overflow-x`, `overflow-y` |
| **Visuals** | `bg`, `radius`, `shadow`, `opacity` |
| **Text** | `text`, `text-color`, `font-size`, `text-align`, `wrap`, `max-lines`, `style` (named typography role) |
| **Interaction** | `hover-bg`, `press-bg`, `focus-outline`, `tab-index`, `draggable`, `drop-target` (alias `drop`), `drag-payload`, `accept` |
| **Bindings** | `bind-text`, `bind-checked`, `bind-value`, `bind-scroll`, `bind-disabled` |
| **Reactivity** | `each`, `key`, `signal`, `mode`, `eq` (per-tag - see `<for>` / `<if>`) |
| **i18n** | `dir` (`ltr` \| `rtl` \| `auto`, inherits down the tree) and `lang` (a BCP-47 tag, e.g. `en-US`) - drive text shaping, the CSS logical properties (`padding-inline-start` and friends, see the [CSS subset](./css.md#layout)), and assistive tech. |

`drop-target="true"` (or the bare `drop-target` / `drop="true"` shorthand)
marks an in-app drop zone; `accept="<mime>"` filters what it accepts
(absent = accept anything). `drag-payload="..."` makes an element a drag
source - an empty value derives the payload from the element's `id`. The
`on_drop(target_id, payload)` script hook fires when a drag releases over
a drop target; see [Scripting](./scripting.md).

Value grammar (lengths, edges, colors) lives in
[`lumenc/src/values.rs`](https://github.com/lumen-fx/lumen/blob/main/lumenc/src/values.rs).
A condensed reminder:

- Length: bare number is px (`24`), `Npx` (`24px`), `N%` (`50%`).
- Edges (padding / margin / inset): 1, 2, or 4 numbers (CSS shorthand).
- Color: `#rrggbb` or `#rrggbbaa`.
- `bg`: a color OR `linear-gradient(...)` / `radial-gradient(...)` / `conic-gradient(...)`.

---

## `<root>`

The single document root. Every `.lmn` file has exactly one. Defaults
to a vertical-flex layout filling the window.

**Synopsis.** Single root of the layout tree; `skin` opts into the
embedded user-agent stylesheet, `frameless` removes window decorations.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `skin` | identifier | e.g. `skin="default"` - opt into the embedded user-agent stylesheet. Equivalent to `[skin] name` in `lumen.toml`. |
| `frameless` | bool | `frameless="true"` removes the title bar / window chrome. Pair with `<title-bar drag="true">` to keep window dragging. |
| `class` | identifier list | Same `class` semantics as elsewhere; CSS rules can target the root. |

```xml
<root skin="default" class="app theme-dark">
  <column padding="24">
    <label text="Hello" />
  </column>
</root>
```

**Related.** `<title-bar>`.

---

## `<column>` / `<row>`

Flex containers. `<column>` stacks children top-to-bottom; `<row>`
stacks left-to-right. Defaults: `align="stretch"`, `justify="start"`.

**Attributes.** Layout family. `gap` controls spacing between
children. `grow` is set on *children* to claim leftover axis.

```xml
<column padding="24" gap="12" align="center">
  <label text="Heading" />
  <row gap="8" justify="end">
    <button text="Cancel" />
    <button text="OK" />
  </row>
</column>
```

**Related.** `<spacer>`, `<tile>`.

---

## `<spacer>`

Empty flex grow box. Defaults to `grow="1"` so it eats remaining axis.
Use to push siblings to opposite ends of a row / column.

```xml
<row>
  <label text="Logo" />
  <spacer />
  <button text="Sign in" />
</row>
```

---

## `<tile>`

Generic styled box leaf. Carries no built-in behaviour; use as a paint
target for backgrounds, gradients, shadows.

```xml
<tile width="120" height="80" bg="#7aa2f7" radius="10"
      shadow="0 4 14 #00000077" />
```

**Related.** `<button>` (focusable tile with click dispatch).

---

## `<scroll>`

Clipped scrolling container. Auto-applies `layout-boundary` (taffy
sub-tree isolation) so its inner content can be relayouted without
invalidating the rest of the tree.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `scroll` | `y` \| `x` \| `both` | Default `y`. `x` flips the inner flex to `row` automatically. |
| `sensitivity` | number | Wheel-delta multiplier. `1.0` = default; `0.5` halves the scroll speed. |
| `inertia` | number | 0..1 friction factor. `0` = no inertia; higher = longer glide. |
| `layout-boundary` | bool | `true` by default for `<scroll>`. |
| `bind-scroll` | signal name | Two-way f32 binding (logical px) to the vertical offset. |

```xml
<scroll height="320" sensitivity="0.6" inertia="0.4">
  <for each="todos" key="id" gap="6">
    <row class="list-row">
      <label width="32" text="{row.idx}" />
      <label grow="1" text="{row.label}" />
    </row>
  </for>
</scroll>
```

A focused `<scroll>` region (or the nearest scrollable ancestor of the
focused element) also responds to the keyboard: arrow keys scroll by a
line, `PageUp` / `PageDown` by a page, and `Home` / `End` jump to the
ends. Horizontal arrows are ignored on a vertical-only scroller, and a
focused text input keeps its own arrow handling instead of scrolling.

`bind-scroll="signal"` makes the offset reactive without any per-frame
script hook: writing the signal (f32, logical pixels) scrolls the
container on that tick - out-of-range values clamp to the content
extent, exactly like user scrolling - and cancels any in-flight fling.
The other direction is throttled: user scrolling writes the offset back
into the signal once, when the scroll *settles* (offset stopped moving
and the fling slept), never on every frame.

```xml
<scroll height="320" bind-scroll="feed_pos"> ... </scroll>
<button id="to-top" text="Back to top" />
```

```candela
fn on_start() { lumen::on("click", "to-top", "scroll_top"); }
fn scroll_top(id) { lumen::signal_set_float("feed_pos", 0.0); }
```

**Related.** `<for>`.

---

## `<overlay>`

Floats out of normal flow. Defaults: `position="absolute"`,
`inset="0 0 0 0"`, so it covers its nearest positioned ancestor
(typically the root). Use for modal backdrops, dropdowns, tooltips -
anything that should paint above its siblings.

```xml
<root>
  <column> ... main content ... </column>
  <overlay class="dim">
    <tile width="200" height="200" bg="#ffffffcc" />
  </overlay>
</root>
```

**Related.** `<dialog>` (modal sugar over `<overlay>`).

---

## `<label>`

Text leaf. Renders `text` through cosmic-text; respects `font-size`,
`text-color`, `wrap`, `max-lines`, `text-align`.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `text` | string | Body text. Can also be the element's text content (`<label>Hi</label>`). |
| `font-size` | number | px. Default 16. |
| `text-color` | color | Default inherits from CSS / root. |
| `wrap` | `none` \| `word` \| `glyph` | Default `none` (no wrap). |
| `max-lines` | int >= 0 | Truncate to N lines with an ellipsis. |
| `text-align` | `start` \| `center` \| `end` | aliases: `left` / `right`. |
| `style` | typography role | Named type scale (e.g. `title-lg`, `body-md`, `caption`) that sets a default `font-size`. See [CSS subset](./css.md#text). |
| `bind-text` | signal name | Replace text content with the named signal's value. |

```xml
<label class="lead" text="Welcome." wrap="word" max-lines="3" />
<label bind-text="status" />
```

> **Sizing.** `<label>` carries no built-in width or height floor - the
> text shaper measures the real glyph extent of `text` (at the
> resolved `font-size` / `wrap` / `max-lines`) and that becomes the
> element's intrinsic size, the same way a browser sizes an unstyled
> `<span>`. A bare `<label text="Hi" />` is visible without explicit
> sizing because of that measurement, not a hardcoded default. Explicit
> `width` / `height` / `min-width` still override it.

---

## `<input>` / `<textarea>`

Editable text field. `<input>` is single-line; `<textarea>` is
multiline (Enter inserts `\n`, Shift+Enter commits).

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `placeholder` | string | Shown when content is empty. |
| `bind-text` | signal name | Two-way: typing writes back to the signal. |
| `multiline` | bool | Explicit override. `<textarea>` defaults to true; `<input>` defaults to false. |
| `disabled` | bool | Marks the field disabled - the runtime `Disabled` marker gates input, and CSS `:disabled` routes its fill. |
| `required` | bool | Validation flag - empty content fails. |
| `pattern` | string | Validation - content must contain this **literal substring** (not a regex; full regex not yet wired). |
| `min` / `max` | number | Numeric range when content parses as a number. |

```xml
<input width="280" placeholder="Type something..." bind-text="who" />
<textarea width="320" height="80"
          placeholder="Multi-line - Shift+Enter to commit"
          bind-text="note" />

<!-- Validation. valid:<id> signal mirrors the result. -->
<input id="email" required="true" pattern="@" bind-text="email" />
<label bind-text="valid:email" />
```

> **Pattern is literal-substring, not regex.** It is a contains-check.
> A full regex backend is not yet wired; until then `pattern="@"`
> matches any string containing the character.

**Keyboard.** Text fields support word-wise editing: `Ctrl`/`Cmd`+Left/Right
jumps by word, `Ctrl`/`Cmd`+`Backspace`/`Delete` deletes the previous /
next word, and `Ctrl`/`Cmd`+A selects all. Plain Left/Right, `Home`, and `End`
move the caret; holding `Shift` extends the selection. On `<textarea>`
(multiline), Up/Down move the caret one visual line (soft-wrap aware) while
keeping a sticky target column, and `Home`/`End` jump to the visual line
start/end (not the `\n`-delimited logical line); `Ctrl`/`Cmd`+`Home`/`End`
jump to the document start/end. A bare `Enter` inserts a newline and
`Shift`+`Enter` commits; on single-line `<input>`, `Enter` commits.

**Pointer + IME.** A click (or drag, double-click word, triple-click line)
hit-tests against the real shaped glyphs, so the caret lands on the correct
character even in proportional fonts. When an OS input method is active, the
candidate window docks under the caret (not the whole field).

**Related.** `<date-picker>`, `<time-picker>` (validated `<input>` collapses).

---

## `<image>`

Raster or SVG image. The asset is loaded once and cached
(`MemoryBudget.images_mb`).

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `src` | path | Relative paths resolve against the app dir, then each `[asset_roots]` path. |
| `fit` | `fill` \| `cover` \| `contain` \| `none` \| `scale-down` | Default `none`. |
| `opacity` | 0..1 | Wraps the draw in `push_layer(alpha)` when < 1. |

```xml
<image src="icons/sun.png" width="48" height="48" fit="contain" />
```

> **SVG limitations.** The SVG walker handles linear + radial
> gradients and clip-paths. Pattern / mask / nested-image / text nodes
> currently warn and skip.

---

## `<div>`

Generic block leaf. Renders as a tile-with-no-default-decoration.
Mostly useful when you want a class-styled box without `<tile>`'s
default sizing.

```xml
<div class="card-body">
  <label text="content" />
</div>
```

---

## `<a href="...">`

Real anchor. A click navigates the app's file-based pages: every
`.lmn` file in the app directory is a page keyed by its filename stem,
and `href` is a page path resolved against that set - not a URL
scheme. On the future web-transpile target it maps 1:1 onto a DOM `<a
href>`.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `href` | page path | Required to navigate. Resolved by longest existing `.lmn` prefix, so `href="user/42"` hits `user.lmn` with `/42` left over for the page to read. |
| `text` | string | Same as `<label>` - can also be the element's text content. |

```xml
<a href="settings" text="Settings" bg="#2d6cdf" text-color="#ffffff"
   padding="6 12 6 12" radius="6" />
```

`<a>` carries no built-in visuals or padding - style it like a `<tile>`
or `<button>`. It is not in the default Tab order (no implicit
`tab-index`); add `tab-index="0"` if it should be keyboard-reachable.
Multi-page apps also have a `page(path)` scripting command that reaches
the same navigation resolver as a click on `<a href>`.

**Related.** `<button>` for an action that isn't a page link.

---

## `<button>` / `<toggle>` / `<switch>` / `<slider>`

Stateful controls. Each is focusable (`tab-index=0` by default) and
hit-tested for pointer interaction.

### `<button>`

A focusable tile with click dispatch.

```xml
<button id="bump" text="Click me" />
<button id="save" class="btn-primary">Save changes</button>
```

Inner text content becomes the button label. A bare `<button>` gets a
36 px minimum height from the built-in stylesheet; width comes from
the measured label text plus padding, so it stays legible without
explicit sizing.

### `<toggle>`

Boolean two-state control, rendered as a slab-shaped checkbox / switch
hybrid.

```xml
<toggle id="dark" bind-checked="dark" />
<toggle checked="true" />        <!-- initial state -->
```

`bind-checked="signal"` is two-way: the user click writes back via the
`BindChecked` reverse-listener. A `<toggle>` also accepts `disabled` to
gate interaction, and CSS `:checked` / `:disabled` route its fill.

### `<switch>`

Boolean two-state control on the same machinery as `<toggle>` -
`checked`, `bind-checked`, `disabled` all work the same way - rendered
as a pill-shaped track with a sliding thumb (iOS / Material switch
proportions) instead of `<toggle>`'s slab.

```xml
<switch id="wifi" bind-checked="wifi_on" />
```

Use `<switch>` and `<toggle>` for the same kind of boolean setting;
pick whichever visual matches the platform convention you're
targeting. All four built-in skins style `<switch>` with the same
track/accent colors as their `<toggle>`, just in pill geometry.

### `<slider>`

0..1 (or `min`..`max`) drag value.

```xml
<slider id="volume" min="0" max="100" value="42"
        bind-value="volume" width="240" />
```

`bind-value="signal"` is two-way. The dragged value is clamped to the
[`min`, `max`] range (defaults: 0 and 1). A focused slider is also
keyboard-driven: Left/Right (and Up/Down) nudge by one step, `PageUp` / `PageDown`
by ten steps, and `Home` / `End` jump to `min` / `max`. The mouse wheel
over a hovered slider nudges by one step per notch (and never scrolls an
ancestor `<scroll>` container), and `Escape` mid-drag cancels the drag,
restoring the pre-drag value. `step="..."` sets the keyboard / wheel
increment; absent, it defaults to `(max - min) / 100`.

---

## `<checkbox>` / `<radio>` / `<progress>`

W5 form controls. All three desugar in the parser to real element
subtrees, so every visual (indicator size, colors, radii, sweep timing)
is CSS-reachable through the skins or app CSS.

### `<checkbox label="...">`

Box + label boolean control on the same machinery as `<toggle>`:
`checked`, `bind-checked` (two-way), fires `on_toggle(id, checked)`.
Clicking anywhere on the row - box, label, or gap - toggles, as does
`Space` while focused. Desugar:

```
checkbox (row, Toggleable)
|- .checkbox-box   - indicator tile; check / dash mark rendered as text
`- .checkbox-label - the caption
```

```xml
<checkbox id="opt" label="Enable telemetry" bind-checked="telemetry" />
<checkbox label="Partially synced" indeterminate="true" />
<checkbox label="Locked" disabled="true" />
```

`indeterminate="true"` is the web/Qt tri-state: the box renders a dash
(over the checked fill) regardless of `checked` until the first user
toggle clears it. Script `bind-checked` writes do not clear it. Style
via `.checkbox-box { ... }`, `checkbox:checked { bg: ... }` (the box fill
when on), `checkbox:focus { outline: ... }`.

### `<radio group="..." value="..." label="...">`

Name-grouped exclusive choice (Qt auto-exclusive radio group / ARIA
radiogroup). All `<radio>` elements sharing a `group` string form one
group; the group's selected value lives in the PropertyStore signal of
that name - read it from scripts like any signal, drive it to select
programmatically.

```xml
<radio group="ship" value="air"  label="Air freight" checked="true" />
<radio group="ship" value="sea"  label="Sea freight" />
<radio group="ship" value="rail" label="Rail" disabled="true" />
```

- Exactly one selected per group: `checked="true"` seeds the signal;
  with none checked the first enabled member is auto-selected.
- Click (row / dot / label) and `Space` select.
- `Left`/`Right`/`Up`/`Down` move selection to the previous / next enabled member,
  wrapping at the ends and skipping disabled members; selection follows
  focus (Qt/GTK).
- Roving tabindex: only the selected member sits in the Tab chain, so
  `Tab` enters and leaves the whole group as one stop.

Style via `.radio-dot { ... }`, `radio:selected { bg: ... }` (dot fill),
`radio:focus { outline: ... }`.

### `<progress value max>`

Non-interactive progress bar (never focusable, consumes no input).

```xml
<progress value="30" max="100" width="240" />   <!-- determinate 30% -->
<progress bind-value="pct" max="1" />            <!-- signal-driven -->
<progress />                                     <!-- indeterminate sweep -->
```

With `value` / `bind-value`, the `.progress-fill` child's width tracks
`value / max`. Without either, the bar is indeterminate: a 30 %-wide
chunk bounces across the track; the sweep period comes from
`duration="ms"` -> CSS `progress-duration` -> the
`--lumen-progress-period` token (skins default 1200 ms). A later
`bind-value` write flips an indeterminate bar to determinate. Track
styling on `progress { ... }`, fill on `.progress-fill { ... }`. Hidden
bars (closed tab, hidden dialog) stop animating and requesting frames.

---

## `<for each="..." key="...">`

Reactive iteration. Spawns one copy of its inline children per item in
the named `ArraySignal`. Reference an item's fields inside the body with
the `{row.field}` form - `{row.label}`, `{row.status}`, and so on - and
each row resolves it against its own item at reconcile time.

> **Use `{row.field}`, not bare `{field}`.** A bare `{field}` inside a
> `<for>` body resolves against global signals first, not the row item,
> so it is ambiguous and the compiler warns on it. Always qualify row
> fields with `row.`; use `{$name}` when you deliberately want a global
> signal from inside the loop.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `each` | signal name | Must reference an array signal. |
| `key` | field name | Stable identity for reconciliation. The reconciler diffs by `key`. |
| `virtualized` | bool | Recycle row entities for long lists (see the note below). |
| `row-height` | number | Required when `virtualized="true"`. |

```xml
<for each="todos" key="id" gap="6">
  <row class="list-row" align="center" gap="10">
    <label width="32" text="{row.idx}" />
    <label grow="1" text="{row.label}" />
    <label class="pill" text="{row.status}" />
  </row>
</for>
```

The rows come from an array signal. The Rhai and Lua hosts write one through
`signal_array(name)`, and an embedder writes one over the C ABI:

```rhai
let todos = signal_array("todos");
todos.set([
    #{ id: "1", idx: "1", label: "Layout - taffy", status: "done" },
    #{ id: "2", idx: "2", label: "Reactive signals", status: "done" },
]);
todos.push(#{ id: "3", idx: "3", label: "New row", status: "todo" });
```

A candela script builds lists a different way: ship an empty container in the
markup and spawn the rows into it with the DOM API, which also covers reorder
and per-row updates that `<for>` rebuilds wholesale. See
[Scripting](./scripting.md#the-dynamic-dom-api).

> **Reconciler.** Append-only + tail-trim fast paths preserve focus,
> scroll position, and per-row signals. Mid-stream insert / reorder
> still falls back to a full rebuild.

> **Virtualized lists.** `virtualized="true"` has the row-pool shape but
> hard-codes the scroll-window math; a general windowing solution is not
> yet wired.

---

## `<if signal="..." mode="render|hide">`

Conditional subtree. Mounts its inline children only when the named
signal is truthy (non-empty AND not literal `"false"` / `"0"`).

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `signal` | signal name | Required. The signal to watch. |
| `mode` | `render` \| `hide` | `render` (default): spawn / despawn on flip. `hide`: spawn once + flip `Visible`. |
| `eq` | string | Optional. When set, the truthy check becomes `signal == eq`. Powers the `<tabs>` collapse. |

```xml
<!-- Spawn only when loaded -->
<if signal="loaded">
  <column> ...body... </column>
</if>

<!-- Keep body's state across show/hide flips -->
<if signal="dialog-open" mode="hide">
  <column> ...form fields...</column>
</if>
```

`mode="hide"` is the right default for any subtree whose descendants
hold non-trivial state (input cursors, scroll positions). `mode="render"`
saves the despawn / respawn ECS cycles for branches that don't.

**Related.** `<dialog>` (sugar over `<if mode="hide">`).

---

## `<dialog open="...">`

Modal overlay. Sugar for an absolute-positioned full-viewport container
whose visibility is bound to a signal. Implements the Qt `QDialog`
contract (W5):

- **Focus trap** - Tab / Shift-Tab cycle only within the open dialog
  (`FocusBoundary`).
- **Initial focus** - on open, focus moves to the first
  `autofocus="true"` descendant; else the first focusable descendant in
  markup order; else the dialog panel itself.
- **Focus restore** - on close, focus returns to whatever held it
  before the dialog opened.
- **Default button** - `<button default="true">` (gains the `default`
  class for skin styling). `Enter` anywhere in the dialog activates it,
  except when focus sits on another button or a multiline textarea;
  single-line inputs fire the default alongside their own commit
  (Qt/web line-edit behaviour). With no explicit default, the first
  enabled button acts as the Enter target (Qt autoDefault).
- **Exactly-once accepted / rejected** - every open->close cycle fires
  exactly one of `on_dialog_accepted(id)` / `on_dialog_rejected(id)`
  (per-id routers `on("dialog_accepted", id, fn)` /
  `on("dialog_rejected", id, fn)`). Accepted = the close went through
  the default button (click or Enter-anywhere); everything else -
  Escape, cancel buttons, script signal writes - is rejected. `id` is
  the dialog's `id="..."`, falling back to its open-signal name.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `open` | signal name | When the signal is truthy, the dialog mounts. |
| `id` | string | Handed to the accepted / rejected script hooks. |

```xml
<dialog id="confirm-dialog" open="dialog_open">
  <column class="card" width="420" gap="12">
    <label class="h2" text="Confirm" />
    <input placeholder="Type to confirm" bind-text="confirm_text" autofocus="true" />
    <row gap="10">
      <button id="dialog-close" text="Cancel" />
      <button id="dialog-confirm" text="Confirm" default="true" />
    </row>
  </column>
</dialog>
```

```candela
fn on_start() {
    lumen::signal_set("dialog_open", "");        // initial: closed
    lumen::on("click", "dialog-close", "close");
    lumen::on("click", "dialog-confirm", "close");
    lumen::on("dialog_accepted", "confirm-dialog", "accepted");
    lumen::on("dialog_rejected", "confirm-dialog", "rejected");
}

fn open(id) { lumen::signal_set("dialog_open", "1"); }
fn close(id) { lumen::signal_set("dialog_open", ""); }
fn accepted(id) { /* commit */ }
fn rejected(id) { /* discard */ }
```

The dialog descendants' state (input text, toggles) survives show/hide
because the collapse uses `<if mode="hide">`.

---

## `<title-bar drag="true">`

Frameless-window draggable region. Defaults: 32 px tall,
full-width row at the top. Adding `drag="true"` makes pressing and
moving the bar request a native `winit::Window::drag_window()`.

```xml
<root frameless="true">
  <title-bar drag="true">
    <label text="My app" />
    <spacer />
    <button id="close" text="x" />
  </title-bar>
  <column> ...body... </column>
</root>
```

---

## `<tooltip text="..." delay="...">`

Hover-delay popup. Wraps **exactly one** trigger child; the parser
collapses the `<tooltip>` wrapper and attaches a `TooltipSource` to
the inner element. At runtime `lumen-primitives::TooltipPlugin`
watches `Hovered`, stamps a per-entity `HoverStartedAt`, and spawns an
absolute-positioned popup once dwell exceeds `delay_ms`.

Placement is **cursor-relative** (Qt `QToolTip`): below-right of the
pointer hotspot by `offset` px, flipping above / left of the cursor
near the viewport's bottom / right edges so the popup never leaves the
window.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `text` | string | Tooltip body. |
| `delay` | int ms | Dwell time before show. Unset => the `--lumen-tooltip-delay` skin token (default 500; macOS skin 1000, Windows 400). |
| `offset` | px | Cursor-to-popup gap. Unset => `--lumen-tooltip-offset` (default 12). |

```xml
<tooltip text="Save changes (Ctrl+S)" delay="300">
  <button id="save" text="Save" />
</tooltip>
```

Multi-child tooltips are a parse error so authors get clear feedback
instead of silent first-child pickup.

---

## `<tabs bind-value="...">` + `<tab name label>`

Tabbed container. Children must be `<tab name="..." label="...">...</tab>`.
The parser flattens to a column with a button strip on top and per-tab
`<if mode="hide" eq="...">` bodies, so switching tabs preserves
descendant state.

**`<tabs>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `bind-value` | signal name | Required. Signal stores the active tab `name`. First tab seeds as default. |

**`<tab>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `name` | string | Required. Used as the equality value for the bound signal. |
| `label` | string | Optional. Visible button text; falls back to `name`. |

```xml
<tabs bind-value="active_tab">
  <tab name="primitives" label="Primitives">
    <row gap="12"> ...content... </row>
  </tab>
  <tab name="controls" label="Controls">
    <column> ...content... </column>
  </tab>
</tabs>
```

`<tab>` outside `<tabs>` is a parse error.

---

## `<dropdown bind-value="...">` + `<option value label>`

Select widget. The parser collapses to a header button + an
absolute-positioned options panel toggled via `__dropdown_open:<signal>`.
Clicking an option writes its value to the bound signal and closes the
panel. The panel also dismisses itself on `Escape` or on a press
outside its bounds.

**`<dropdown>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `bind-value` | signal name | Required. Current selection. |
| `placeholder` | string | Header text before any selection. |

**`<option>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `value` | string | Required. Written to the bound signal on click. |
| `label` | string | Optional. Visible text; falls back to `value`. |

```xml
<dropdown bind-value="weight" placeholder="Select weight...">
  <option value="light"  label="Light" />
  <option value="medium" label="Medium" />
  <option value="heavy"  label="Heavy" />
</dropdown>
```

`<option>` outside `<dropdown>` is a parse error.

---

## `<menu id="...">` + `<menuitem id label accel>` + `<separator/>`

Popup / context menu. `<menu>` collapses to an absolute-positioned
panel toggled via `__menu_open:<id>`. Call `open_menu(id)` /
`close_menu(id)` from a script to flip the panel; item clicks fire
`on_menu(id)` and close the menu. Like the dropdown, the panel also
dismisses on `Escape` or an outside press.

**`<menu>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `id` | string | Required. The menu's id stem; the open-signal becomes `__menu_open:<id>`. |

**`<menuitem>` attributes.**

| Attr | Type | Notes |
|---|---|---|
| `id` | string | Required. Passed to `on_menu(id)` / a per-id handler. |
| `label` | string | Optional. Visible text; falls back to `id`. |
| `accel` | string | Optional (menu-bar only - see below). Accelerator hint. |

```xml
<menu id="actions">
  <menuitem id="rename"    label="Rename" />
  <menuitem id="duplicate" label="Duplicate" />
  <separator />
  <menuitem id="delete"    label="Delete" />
</menu>
```

```candela
fn on_start() {
    lumen::on("click", "open-actions", "open");
    lumen::on("menu", "delete", "do_delete");
}

fn open(id) { lumen::open_menu("actions"); }
fn do_delete(id) { /* ... */ }
```

`<menuitem>` or `<separator>` outside `<menu>` or `<menubar>` is a
parse error.

---

## `<menubar>` + `<menu label>`

Native OS menu bar (macOS / Windows). `<menubar>` lives directly
inside `<root>`; it's extracted from the layout tree at parse time and
attached to the OS window via `muda`. Linux builds skip menu-bar
attach (libxdo dep is optional).

Inside `<menubar>`, `<menu label="...">` blocks contain `<menuitem>` /
`<separator>` exactly like popup menus, but their `id` lands on the
native menu chain. Click dispatch fires `on_menu(id)`.

```xml
<root>
  <menubar>
    <menu label="File">
      <menuitem id="new"  label="New"     accel="Cmd+N" />
      <menuitem id="open" label="Open..."   accel="Cmd+O" />
      <separator />
      <menuitem id="quit" label="Quit"    accel="Cmd+Q" />
    </menu>
    <menu label="Edit">
      <menuitem id="undo" label="Undo"    accel="Cmd+Z" />
    </menu>
  </menubar>
  <column> ...body... </column>
</root>
```

Duplicate `<menubar>` blocks under one `<root>` are a parse error.

---

## `<date-picker bind-value="...">` / `<time-picker bind-value="...">`

Validated text inputs. Today they collapse to an `<input>` with a
built-in `pattern` substring matcher and an `id` so the `valid:<id>`
signal mirrors the result. A full grid picker is not yet wired.

**Attributes.**

| Attr | Type | Notes |
|---|---|---|
| `bind-value` | signal name | Required. |
| `id` | string | Optional. Used as the validation signal stem. |
| `placeholder` | string | Default: `YYYY-MM-DD` (date) or `HH:MM` (time). |

```xml
<date-picker bind-value="meet_date" />
<time-picker bind-value="meet_time" />
```

Pattern matchers (literal-substring): date requires `-`, time
requires `:`. The placeholder hints at the full shape.

---

## `<template name="..." {defaults}>`

Reusable markup body with `<slot/>`, id auto-namespacing, and default
attribute values. Detailed in [Templates + slots](./templates.md).

```xml
<template name="card" variant="primary">
  <column class="card card-{variant}">
    <slot />
  </column>
</template>

<card> ...content... </card>
<card variant="danger"> ...content... </card>
```

---

## `<script>` / `<script src="..." />`

Attaches the app's script. Not a layout node.

```xml
<script src="main.cdl" />
```

Or inline (avoid XML-illegal characters):

```xml
<script>
  import "lumen.cdl";
  fn on_start() { lumen::signal_set("ready", "yes"); }
  fn main() {}
</script>
```

Multiple `<script>` blocks concatenate in source order. The file extension
picks nothing on its own: select the host with `[script] engine` in
`lumen.toml`. See [Scripting](./scripting.md).

---

## `<include src="...lmn" />`

Splits markup across files. At parse time the referenced `.lmn` file is
loaded and its **top-level elements splice in place** of the `<include>`
tag - as if you had pasted the file's contents there. Not a layout node;
nothing survives in the tree but the included elements.

```xml
<root>
  <column>
    <include src="parts/header.lmn" />   <!-- header.lmn's elements land here -->
    <label text="body" />
  </column>
</root>
```

Paths resolve **relative to the file that contains the `<include>`**, so
a nested include inside `parts/header.lmn` resolves against `parts/`.

**Templates register globally.** A `<template name="...">` defined in an
included file is usable from *any* file - the parser collects every
template across the whole include graph before expanding use-sites, so
inclusion order doesn't matter:

```xml
<!-- lib.lmn -->
<template name="card"><tile class="card"><slot/></tile></template>

<!-- main.lmn -->
<root>
  <include src="lib.lmn" />
  <card>hi</card>          <!-- resolves to lib.lmn's template -->
</root>
```

Rules and limits:

- **Nested includes** are followed recursively.
- **Cycles are rejected** with an error naming the full chain
  (`include cycle detected: main.lmn -> a.lmn -> b.lmn -> a.lmn`).
- A **missing file** is an error carrying the include-site position
  (`include "parts/x.lmn" not found (from main.lmn:3:5): ...`).
- Editing any included file **hot-reloads** the app, exactly like editing
  `main.lmn`.
- Both `<include src="..."/>` (self-closing) and
  `<include src="..."></include>` (any body ignored) are accepted.

Includes are resolved by the runtime and `lumenc check`. Tooling that
parses raw strings without a file loader (e.g. the LSP when a document
isn't backed by a real path) drops unresolved `<include>` tags rather
than erroring, so single-file editing never breaks.

---

## Default sizes / behaviours

Without explicit `width` / `height`, an element with no measurable
text content and no default from the table below collapses to 0 px -
that's plain taffy behavior for an empty box. A built-in stylesheet
(loaded beneath any skin and beneath your own CSS) gives the stock
controls a tap-sized floor so a bare control is visible without
sizing:

| Tag | Default `height` | Default `min-width` |
|---|---|---|
| `<root>` | 100% | - |
| `<title-bar>` | 32 px | - |
| `<button>` | min-height 36 px | - (width from measured label text) |
| `<input>` / `<textarea>` | min-height 24 px | 160 px |
| `<toggle>` | 36 px | 96 px |
| `<switch>` | 28 px | 52 px (only applies when `width` is also unset) |
| `<slider>` | 36 px | 160 px |
| `<checkbox>` / `<radio>` | min-height 24 px | - (indicator + label supply width) |
| `<progress>` | 6 px | 100% width |
| `<spacer>` | - | - (carries `grow=1`) |

`<label>`, `<tile>`, `<div>`, and `<a>` carry no built-in floor at all;
`<label>` sizes from its measured text (see the note under `<label>`
above), and the others size from explicit attributes, children, or a
`bg` / `border` you set - an empty `<tile>` with no sizing is 0x0 and
invisible, same as an empty `<div>` in a browser with no CSS.

Override any of this with explicit attributes or your own CSS rule of
equal or higher precedence; inline attributes always win over both the
built-in stylesheet and your own CSS.

## Reserved attribute names by tag

| Tag | Attribute | Meaning |
|---|---|---|
| `<if>` | `signal`, `mode`, `eq` | Conditional subtree. Ignored on other tags. |
| `<dialog>` | `open` | Sugar for `signal=<v> mode=hide`. |
| `<tabs>` | `bind-value` | Required. |
| `<dropdown>` | `bind-value`, `placeholder` | `placeholder` only here. |
| `<menu>` | `id` | Required for popup menus. |
| `<menubar>` | - | Only valid directly inside `<root>`. |
| `<title-bar>` | `drag` | Frameless-window drag region. |
| `<root>` | `skin`, `frameless` | Window-level metadata. |
| `<image>` | `src`, `fit` | Asset path + sizing. |
| `<a>` | `href` | Page path for navigation. |
| `<date-picker>` / `<time-picker>` | `bind-value` | Required. |

Unknown attributes are silently ignored today (forward-compat). A strict
mode that surfaces them as parse errors is on the roadmap.
