# CSS subset

Lumen reads a narrow subset of CSS. Most properties share a name with a
markup attribute and mean the same thing; a few, like `transition` and
`display: grid`, exist only in a stylesheet. This page is the full
property list, the selector grammar, and the cascade rules.

## What is in the subset

The parts of CSS that map onto a fixed tag vocabulary and a flexbox layout
model: the full cascade order (origin, then importance, then specificity,
then source order), descendant and child combinators, structural and state
pseudo-classes, `:is()` / `:where()` / `:not()`, `!important`, custom
properties, `@media`, and `@import`.

What is left out is the surface that assumes an open-ended document:
arbitrary property names, attribute selectors, pseudo-elements, and the
at-rules that imply another runtime (`@keyframes`, `@supports`, `@layer`).
Colors are hex, lengths are pixels or percentages, and there is no
`calc()`.

Inline markup attributes beat CSS, because they sit in a higher cascade
origin; the apply pass only fills values the markup left unset.
`!important` lifts a CSS declaration above that.

A bad declaration never aborts the stylesheet. The offending
`property: value` pair is skipped and reported as a warning naming the
selector, the property, and the reason, and the rest of the sheet applies.

## Grammar

```text
stylesheet  := (at_rule | rule)*
at_rule     := "@media" query "{" rule* "}"
rule        := selector_list "{" declaration* "}"
selector_list := selector ("," selector)*
selector    := compound (combinator compound)*
combinator  := " " | ">" | "+" | "~"
compound    := tag? ("." class | "#" id | ":" pseudo)*
declaration := ident ":" value ["!important"] ";"
```

- `/* comments */` strip cleanly.
- Whitespace is liberal.
- Trailing `;` on the last declaration is optional.
- An unknown property name is dropped and reported as a `CssWarning`; the
  rest of the rule still applies.
- An unknown selector form is a parse error, and so is an at-rule other
  than `@media`.
- `!important` matches case-insensitively and tolerates whitespace after
  the `!`.

## Selectors

| Selector | Matches |
|---|---|
| `tile` | Every `<tile>` element. |
| `*` | Every element. |
| `.card` | Every element whose `class` list contains `card`. |
| `#save` | The element with `id="save"`. |
| `tile.card.primary` | A `<tile>` carrying both `card` and `primary` classes. |
| `tile:hover` | `<tile>` while hovered. Pseudo states route to dedicated `Attributes` fields (see below). |
| `button:hover:focus` | Several pseudo-classes on one compound. |
| `:root` | The root element. |

A tag must come first in a compound: `.card tile` is a descendant selector,
not a compound.

**Supported.**

- Descendant combinator (`.app .card`).
- Child combinator (`.list > .row`).
- `:is(...)`, `:where(...)`, `:not(...)`.
- Structural pseudos (`:first-child`, `:last-child`, `:only-child`,
  `:empty`, `:nth-child(an+b | odd | even)`, `:root`).
- `!important`.
- `@media`, covering `(prefers-color-scheme: dark|light|no-preference)`,
  `(prefers-reduced-motion: reduce|no-preference)`,
  `(prefers-contrast: more|less|custom|no-preference)`, and
  `(min-width|max-width|width: <px>)`. Combine features with `and`. Each
  feature needs its own parentheses, and a bare `(prefers-reduced-motion)`
  or `(prefers-contrast)` means `no-preference`.

**Not supported.**

- Adjacent / general-sibling combinators (`+`, `~`). They parse and count
  toward specificity, but they never match.
- An `:is()` / `:where()` / `:not()` argument that is more than one
  compound. `:not(.a)` works; `:not(.a .b)` parses and never matches.
- Attribute selectors (`[type="text"]`).
- Pseudo-elements (`::before`, `::placeholder`) - hard parse error.
- Media types and query lists: `@media screen`, `@media (a), (b)`,
  `not`, and `or` are all parse errors. A bare `(prefers-color-scheme)`
  with no value is one too.
- `@supports`, `@keyframes`, `@font-face`, `@layer`, `@property`,
  `@charset`. Each aborts the stylesheet with a parse error rather than
  being skipped.

### Pseudo-classes

| Pseudo | Maps to | Notes |
|---|---|---|
| `:hover` | `hover-bg`, `hover-border`, + state-routed props | `bg` feeds the `HoverTint` tween; `border: ...` swaps the border while hovered. |
| `:focus` | `focus-outline`, `focus-border`, + state-routed props | Any focus source (pointer or keyboard). `outline: <w> <color>` draws the focus ring (outside the box, CSS outline semantics); `border: ...` swaps the border while focused. |
| `:focus-visible` | keyboard-only focus ring + state-routed props | **True keyboard-only focus** (CSS `:focus-visible` heuristic): the runtime marks focus gained via Tab / Shift-Tab, roving tab arrows, or assistive tech with a `FocusVisible` marker; pointer clicks focus without it. An `outline` under `:focus-visible` paints only for keyboard focus and wins over a `:focus` outline while the marker is present. |
| `:active` | `press-bg`, + state-routed props | |
| `:disabled` | `disabled-bg`, + state-routed props | Routes `bg` to the disabled fill; the runtime `Disabled` marker also gates input on the element. Pairs with the `disabled` markup attribute. State-routed `text-color` / `opacity` / `box-shadow` under `:disabled` apply once at spawn. |
| `:checked` | `checked-bg` | Routes `bg` to a `<toggle>`'s checked-track fill. |
| `:selected` | `selected-bg` | Routes `bg` to a `<tabs>` strip button's fill while it carries the `Selected` marker (e.g. `.tab-btn:selected`). Falls back to a built-in accent when no rule matches. |
| `:drag-over` | `drag-over-bg`, + state-routed props | An in-app drag (see `drag-payload` / `drop-target` in the [tag reference](./tags.md)) is hovering this drop target with an acceptable payload (HTML5 `dragover` parity), e.g. `.lane:drag-over { bg: ...; }`. |
| `:root` | structural | Matches the root element. |
| `:first-child`, `:last-child`, `:only-child`, `:empty`, `:nth-child(N)` | structural | Per Selectors-4. |
| `:is(...)`, `:where(...)`, `:not(...)` | functional | Per Selectors-4 section 17 specificity rules. |

Any pseudo-class outside this set (attribute pseudos, pseudo-elements,
...) is a parse error so typos don't silently no-op.

**State-routed properties.** Under a state pseudo-class, these properties
swap with the state and restore when it ends:

| Property | Routed under |
|---|---|
| `bg` | every state pseudo. Takes a plain color here; a gradient is rejected. |
| `text-color` | every state pseudo, e.g. Windows' pressed-text drop to secondary. |
| `opacity` | every state pseudo, e.g. adwaita's 50%-opacity disabled controls. |
| `shadow` / `box-shadow` | every state pseudo. Full stack replacement, e.g. the WinUI TextBox accent focus underline (`input:focus { box-shadow: inset 0 -2 0 var(--lumen-accent); }`). |
| `border` | `:hover`, `:focus`, `:focus-visible` only. |
| `outline`, `outline-offset` | `:focus`, `:focus-visible` only. `outline` is not a property outside those two. |

Every other property inside a state rule is consumed and dropped, with no
warning and no effect: geometry is not state-routable. That covers
`knob-inset`, `thumb-size`, `popup-gap`, `progress-chunk`,
`disabled-opacity`, `caret-width`, `caret-blink`, `password-character`,
`line-height`, and every `scrollbar-*` property. Write them as plain
declarations; a `:hover { caret-width: 3 }` or
`.scroll:hover { scrollbar-track-hover: #333 }` parses and does nothing.
It also covers `border` and `outline` under a state they do not route
under, so `:active { outline: 2 #fff }` is silently dropped.

#### Disabled dimming

Two different slots both affect a disabled element's opacity, and they
are easy to confuse:

- `:disabled { opacity: <n> }` is the state-routed override - an
  explicit dimming amount for one element.
- `disabled-opacity: <n>` is a plain, always-applicable declaration -
  the amount used when the element is disabled and the author set
  *neither* `disabled-bg` nor the `:disabled { opacity }` override. It
  replaces what used to be a fixed 50% runtime fallback.

If both are absent, a disabled element with no other override dims to
50% by default.

### Cascade

Lumen follows the W3C Cascade-5 cascade order: origin -> importance ->
specificity -> source order, with **later rules winning** at equal
weight (CSS Cascade-5 section 6.4.4). HTML inline attributes (`<tile width="50px"/>`)
beat CSS by origin precedence, and `!important` lifts a CSS declaration
above its origin's normal block.

```css
.btn { bg: #444; }
.btn { bg: #7aa2f7; }            /* wins (later, equal specificity) */
.btn.primary { bg: #f7768e; }    /* wins for `.btn.primary` (higher specificity) */
.btn { bg: #333 !important; }    /* wins everywhere (importance flip) */
```

CSS custom property declarations follow the same cascade order, then
the resolved values feed `var(--name)` calls in the actual declarations.

The lumenc compiler ships a static lint mode that flags rules whose
resolved value changes between the old first-wins ordering and the
new last-wins ordering - run `lumenc lint --css-cascade <dir>` to
audit a stylesheet before upgrading.

## Property surface

The table below is the live set in `apply_declaration` /
`apply_decl_for_pseudo`. The "Markup attr" column shows the inline
attribute that does the same thing on a tag; CSS-only properties say
*CSS-only*.

**Standard-CSS aliases.** For the common cases Lumen accepts the
standard CSS spelling and maps it to its own property name, so muscle
memory works:

| Standard CSS | Lumen property |
|---|---|
| `color` | `text-color` |
| `background`, `background-color` | `bg` |
| `border-radius` | `radius` |
| `flex-grow` | `grow` |

**Units.** Numeric properties accept a bare number or an explicit `px`
suffix interchangeably: `radius: 8` and `radius: 8px` are the same, as are
`font-size: 16` and `font-size: 16px`. `em`, `rem`, `vw`, `vh`, `ch`, and
`pt` are not units Lumen understands. `%` works only where a row below
says so (the sizing properties, `padding` / `margin` / `inset`, and the
gaps); `fr` only inside a grid track list; `deg` only inside a gradient;
`ms` and `s` only in the properties that name a duration.

**Colors are hex.** `#rrggbb` and `#rrggbbaa`, nothing else.
`rgb()`, `rgba()`, `hsl()`, `transparent`, and the named colors all fail
to parse. Write a fully transparent fill as `#00000000`.

**There is no `calc()`**, and no `min()`, `max()`, `clamp()`, `env()`,
`attr()`, or `url()`. The functions that do exist are `var()`, the three
gradients, `minmax()` in a grid track list, and `cubic-bezier()` in a
transition.

A value that fails to parse surfaces as a `CssWarning` naming the selector,
the property, and the reason; the declaration is skipped and the rest of
the stylesheet still applies.

### Layout

| CSS property | Markup attr | Value |
|---|---|---|
| `width`, `height` | `width`, `height` | `auto`, a length (`24`, `24px`), or `50%`. |
| `min-width`, `min-height`, `max-width`, `max-height` | same | Same as `width`. |
| `aspect-ratio` | `aspect-ratio` | Number (w/h ratio). |
| `padding`, `margin` | same | Edges (1, 2, 3, or 4 terms); each term is px **or** `%` (percent resolves against the containing block's width, per CSS). The 3-term form is `top`, `left-right`, `bottom`. |
| `gap` | `gap` | 1 or 2 terms (`<row+col>` / `<row> <col>`); px or `%`. The markup attribute takes a single px number only. |
| `row-gap`, `column-gap` | *CSS-only* | Number (px) or `%`. |
| `grow` | `grow` | Number (flex grow factor). |
| `flex-shrink` | `shrink` | Number (flex shrink factor; default `1`). |
| `flex-basis` | *CSS-only* | Length (`auto`, px, `%`). |
| `flex` | *CSS-only* | `<grow> [<shrink>] [<basis>]`, plus `none` / `auto` / `initial` - exact CSS shorthand semantics (`flex: 1` = `1 1 0%`). |
| `flex-wrap` | *CSS-only* | `nowrap` \| `wrap` \| `wrap-reverse`. |
| `align-content` | *CSS-only* | `start` \| `end` \| `center` \| `stretch` \| `normal` \| `space-between` \| `space-around` \| `space-evenly`. Distribution of wrapped flex lines. |
| `flex-direction` | *CSS-only* | `row` \| `column` \| `row-reverse` \| `column-reverse`. In markup, direction comes from the tag: pick `<row>` or `<column>`. |
| `box-sizing` | *CSS-only* | `border-box` (Lumen UA default) \| `content-box`. |
| `z-index` | `z-index` | Integer, or `auto` (same as `0`) - sibling paint-order override (higher paints on top; equal keeps document order). |
| `align` (alias `align-items`) | `align` | `start` \| `end` \| `center` \| `stretch` \| `baseline`, plus `flex-start` / `flex-end`. The markup attribute takes only the first four. |
| `align-self` | *CSS-only* | Same values as `align`, one child overriding its parent's `align`. |
| `justify` | `justify` | `start` \| `end` \| `center` \| `between` \| `around` \| `evenly`, plus the `space-` spellings. |
| `justify-items`, `justify-self` | *CSS-only* | Same values as `align` - grid-item placement within its cell. |
| `display` | *CSS-only* | `flex` (default) \| `grid` \| `none`. `none` removes the element and its subtree from layout entirely - space-releasing, unlike `opacity: 0`. |
| `position` | `position` | `relative` \| `absolute`. There is no `static`, `fixed`, or `sticky`. |
| `inset` | `inset` | Edges. Only meaningful with `position: absolute`. |
| `overflow`, `overflow-x`, `overflow-y` | same | `visible` \| `hidden` \| `scroll`. There is no `auto` and no `clip`. |
| `scroll` | `scroll` | `y` \| `x` \| `both`. |
| `sensitivity`, `inertia` | same | Scroll-tuning numbers. |
| `tab-index` | `tab-index` | Integer. |
| `draggable` | `draggable` | `true` \| `false`. |
| `layout-boundary` | `layout-boundary` | bool - taffy subtree isolation. |
| `padding-inline-start`, `padding-inline-end`, `padding-block-start`, `padding-block-end` | *CSS-only* | Number (px) - CSS Logical Properties longhands for `padding`, resolved against the element's `dir`. |
| `margin-inline-start`, `margin-inline-end`, `margin-block-start`, `margin-block-end` | *CSS-only* | Number (px) - logical longhands for `margin`. |
| `inset-inline-start`, `inset-inline-end`, `inset-block-start`, `inset-block-end` | *CSS-only* | Number (px) - logical longhands for `inset`. |

### CSS Grid

`display: grid` on a container activates `grid-template-columns` /
`grid-template-rows`; children place with `grid-row` / `grid-column`.
`gap` / `row-gap` / `column-gap` work the same as in flex.

| CSS property | Markup attr | Value |
|---|---|---|
| `grid-template-columns`, `grid-template-rows` | *CSS-only* | Whitespace-separated track list: each track is a px length, an `fr` unit, `auto`, `min-content`, `max-content`, or `minmax(<min>, <max>)`. A `%` track is a parse error. |
| `grid-row`, `grid-column` | *CSS-only* | `<start>` or `<start>/<end>` - 1-based line numbers. A single number auto-places the end line. There is no `span N` and no `auto`. |

```css
.grid {
  display: grid;
  grid-template-columns: 150px 1fr 1fr 1fr 1fr 1fr 1fr 1fr;
  row-gap: 10;
  column-gap: 10;
}
.cell { justify-self: center; align-self: center; }
```

Named grid lines, `grid-template-areas`, `repeat()`, and `fit-content()`
are not supported. Write out each track explicitly.

### Visuals

| CSS property | Markup attr | Value |
|---|---|---|
| `bg` | `bg` | Color **OR** gradient (see below). A state rule (`:hover { bg: ... }`) takes a color only. |
| `radius` | `radius` | 1-4 numbers (px); no percentages. Multi-value follows the CSS `border-radius` shorthand rotation `[top-left, top-right, bottom-right, bottom-left]` - `radius: 4 4 0 0` rounds only the top corners. The markup attribute takes a single number. |
| `border-top-left-radius`, `border-top-right-radius`, `border-bottom-right-radius`, `border-bottom-left-radius` | *CSS-only* | Number (px) - per-corner longhands. |
| `text-color` | `text-color` | Color. |
| `selection-color` | `selection-color` | Color. Text-selection highlight on `input` / `textarea` (skin default: `var(--lumen-selection)`). |
| `caret-color` | `caret-color` | Color. Text-input caret tint on `input` / `textarea`; unset falls back to the text fill. |
| `selection-text-color` | `selection-text-color` | Color. Foreground of selected glyphs on `input` / `textarea` (Qt `HighlightedText` / Slint `selection-foreground-color`); unset keeps the normal text fill. |
| `opacity` | `opacity` | 0..1. |
| `disabled-opacity` | `disabled-opacity` | 0..1 (clamped). The dimming amount applied to a disabled element when the author set neither `disabled-bg` nor an explicit `:disabled { opacity: ... }`. This is a different slot from `:disabled { opacity: ... }` itself - see [Disabled dimming](#disabled-dimming). Not state-routable: writing it inside `:disabled { }` has no effect, use it as a plain declaration. |
| `shadow`, `box-shadow` | `shadow` | Comma-separated shadow stack; each entry `[inset] <ox> <oy> <blur> [<spread>] <color>`. The two spellings are the same property in CSS; only the markup `shadow=` attribute is limited to one entry. Spread inflates (or deflates, when negative) the shadow rect before blurring, so `box-shadow: 0 0 0 2 #fff` draws a hard 2px ring, the standard double-focus-ring idiom. |

### Borders

Real CSS borders: they consume layout space per the box model (with the
`border-box` default, an authored `width` already includes them) and
paint between the background and the content, inside the border box.
Style is `none | solid`; widths and colors are per-side (per-side
colors paint with mitred corner splits, exactly like browsers).

| CSS property | Markup attr | Value |
|---|---|---|
| `border` | `border` | `<width> [solid \| none] <#color>`, any order - `1px solid #444`. `border: none` clears. If the style keyword is omitted (`1px #444`) Lumen normalises to `solid`. |
| `border-width` | *CSS-only* | 1-4 terms, px or `thin`/`medium`/`thick` (1/3/5px). CSS TRBL rotation. |
| `border-color` | *CSS-only* | Color (the uniform base for all four sides). |
| `border-top-color`, `border-right-color`, `border-bottom-color`, `border-left-color` | *CSS-only* | Color - per-side override of the base. E.g. the Windows elevation edge: `border: 1px solid #0000000f; border-bottom-color: #00000029;`. |
| `border-top`, `border-right`, `border-bottom`, `border-left` | *CSS-only* | Per-side shorthand `<width> [solid \| none] <#color>` - sets that side's width + color (and the shared solid style). |
| `border-style` | *CSS-only* | `none` \| `hidden` (a synonym for `none`) \| `solid`. Per CSS, without a style the computed width is `0`, so width and color alone paint nothing. `dashed`, `dotted`, and the rest are parse errors. |
| `border-top-width`, `border-right-width`, `border-bottom-width`, `border-left-width` | *CSS-only* | Number. Useful for separators (`border-bottom-width: 1`). |
| `border-inline-start-width`, `border-inline-end-width`, `border-block-start-width`, `border-block-end-width` | *CSS-only* | Number - CSS Logical Properties, resolved against the element's writing direction. |

### Text

| CSS property | Markup attr | Value |
|---|---|---|
| `font-size` | `font-size` | Number (px). |
| `font-family` | `font-family` | CSS fallback chain, e.g. `"Segoe UI Variable Text", "Segoe UI", sans-serif`. The shaper resolves the first family present in the system font database (case-insensitive; quotes optional); the generic keywords `sans-serif`, `serif`, `monospace`, `cursive`, `fantasy` (plus the platform aliases `system-ui`, `ui-sans-serif`, `ui-serif`, `ui-monospace`, `-apple-system`) map to the platform families. An unresolvable chain falls back to sans-serif. Inherited. |
| `font-weight` | `font-weight` | `normal` (400), `bold` (700), or a number `1..=1000` (CSS Fonts 4). Variable and multi-weight families select the nearest face; the relative keywords `lighter` / `bolder` are rejected. Inherited. |
| `wrap` (alias `white-space`) | `wrap` | `none` (= `nowrap`) \| `word` (= `normal`) \| `glyph` (= `char`). Markup additionally accepts `wrap="ellipsis"` as shorthand for `text-overflow: ellipsis`. |
| `max-lines` | `max-lines` | Integer >= 0. Overflowing text past the cap is elided with `...`. |
| `text-overflow` | `wrap="ellipsis"` | `clip` \| `ellipsis`. `ellipsis` elides overflowing single-line text with a trailing `...` (Qt elide contract; the ellipsis is trimmed to fit inside the box). Combine with an explicit `wrap` + `max-lines` for a multi-line clamp. Not inherited. The built-in skins set it on `.tab-btn`, `.dropdown-button`, and `.dropdown-option` - the surfaces Qt elides. |
| `text-align` | `text-align` | `start` \| `center` \| `end`, plus the aliases `left` / `right`. |
| `line-height` | `line-height` | Unitless multiplier (`1.2`) or a `px` length (`19px`) - the two mean different things, see below. Inherited. |

**Named typography roles are a markup attribute, not a CSS property.**
`style="headline-lg"` on an element names a predefined type scale that
resolves to a `font-size`, so you can pick a role instead of a pixel size:

```xml
<label style="headline-lg" text="Settings" />
```

The roles are, largest to smallest: `display-xl`, `display-lg`,
`display-md`, `display-sm`, `headline-lg`, `headline-md`, `headline-sm`,
`title-lg`, `title-md`, `title-sm`, `body-lg`, `body-md`, `body-sm`,
`label-lg`, `label-md`, `label-sm`, `caption`, `overline`. An explicit
`font-size` still overrides the role's size.

There is no matching CSS property: `style: headline-lg` in a stylesheet is
an unknown property and warns. The role does not inherit either, so set it
on the element that carries the text. And `style=` in Lumen markup never
means a CSS declaration block - `style="color: red"` is a role name that
does not resolve, not an inline style. Inline style properties go in their
own attributes (`text-color="#f00"`).

**`line-height` has two forms that mean different things.** A bare
number (`line-height: 1.2`) is a unitless multiplier of the element's
own font size - it tracks `font-size` automatically, so it keeps
working if you (or the OS text-scale setting) change the font size
later. A `px` value (`line-height: 19px`) is a fixed line box height
that does *not* track `font-size` - use it when you need an exact,
unchanging line grid regardless of what font size ends up applied.
Pick the form that matches what you want; the two are not
interchangeable, and Lumen does not convert between them for you.

### Text input

Properties specific to `input` / `textarea` caret and masking. All
three are Lumen-native (no standard CSS equivalent) and not
state-routable.

| CSS property | Markup attr | Value |
|---|---|---|
| `caret-width` | `caret-width` | Number (px). Stroke width of the caret. Absent = the runtime default. |
| `caret-blink` | `caret-blink` | Duration (`Nms` / `Ns`). Full on/off blink period of the caret. Absent = the runtime default. |
| `password-character` | `password-character` | A single character. The glyph substituted for every character of a masked (password-style) text input. Absent = the runtime default mask glyph. |

### Interaction

| CSS property | Markup attr | Value |
|---|---|---|
| `hover-bg` | `hover-bg` | Color. Equivalent to `:hover { bg: ... }`. |
| `press-bg` | `press-bg` | Color. Equivalent to `:active { bg: ... }`. |
| `focus-outline` | `focus-outline` | `<width-px> <#color>`. Draws outside the box while focused (CSS `outline` semantics - never affects layout). Same as `:focus { outline: ... }`; use `:focus-visible { outline: ... }` for a keyboard-only ring. |
| `outline-offset` | *CSS-only* | Number (px) - gap between the border box edge and the focus ring's inner edge. Valid at rest or inside `:focus` / `:focus-visible`. |
| `knob-color` | `knob-color` | Color - fill of a `<toggle>`'s knob / `<slider>`'s thumb child (Lumen-native analog property; the child is not selector-reachable). The skins seed it; absent = the runtime fallback. |
| `knob-inset` | `knob-inset` | Number (px). Gap between a `<toggle>` / `<switch>` knob's edge and its track's edge. Lumen-native; absent = the runtime default. |
| `thumb-size` | `thumb-size` | Number (px). Diameter of a `<slider>`'s thumb. Lumen-native; absent = the runtime default. |
| `popup-gap` | `popup-gap` | Number (px). Offset between a `<dropdown>` / `<menu>` trigger and its floating panel. Lumen-native; absent = the runtime default. |
| `hover-border` | *CSS-only* | Border shorthand. Equivalent to `:hover { border: ... }`. |
| `focus-border` | *CSS-only* | Border shorthand. Equivalent to `:focus { border: ... }`; wins over `hover-border` when both states are active. |

### Progress

Applies to `<progress>`. Both properties are Lumen-native (no standard
CSS equivalent) and not state-routable.

| CSS property | Markup attr | Value |
|---|---|---|
| `progress-duration` | `duration` | Bare integer milliseconds, > 0. Sweep period of an indeterminate `<progress>` bar. Unlike the other durations on this page it takes no unit suffix: `progress-duration: 1200ms` is a parse error, `progress-duration: 1200` is right. |
| `progress-chunk` | `chunk` | 0..1 fraction, rejected (not clamped) outside that range. Fraction of the track width covered by the moving chunk of an indeterminate sweep. |

### Image

| CSS property | Markup attr | Value |
|---|---|---|
| `fit` | `fit` | `fill` \| `cover` \| `contain` \| `none` \| `scale-down`. |

### Animation

| CSS property | Markup attr | Value |
|---|---|---|
| `transition` | *none - CSS-only* | `<property> <duration> [<easing>]`, comma-separated. |
| `transition-property` | *none - CSS-only* | Comma list of animatable property names, or `none`. |
| `transition-duration` | *none - CSS-only* | Comma list of `Nms` / `Ns`, cycled over the property list. |
| `transition-timing-function` | *none - CSS-only* | Comma list of easing keywords / `cubic-bezier(...)`, cycled. |

> **Animatable set.** `opacity`, `background-color` (aliases
> `background`, `bg`), `color` (alias `text-color`), `border-color` -
> geometry-free visual properties only. **Layout properties (`width`,
> `height`, padding, margins, ...) are deliberately not transitionable**:
> they would re-run layout every frame; such entries parse and are
> dropped with a warning. See
> [Animations + transitions](./animations.md).

### Scrollbars

| CSS property | Markup attr | Value |
|---|---|---|
| `scrollbar-color` | *none - CSS-only* | `auto` \| `<thumb-color> [<track-color>]` (CSS Scrollbars Styling L1). |
| `scrollbar-width` | *none - CSS-only* | `auto` \| `thin` \| `none`. |
| `scrollbar-thickness` | `scrollbar-thickness` | Number (px). Overlay bar width at `scrollbar-width: auto`. Lumen-native; absent = the runtime default. |
| `scrollbar-thickness-thin` | `scrollbar-thickness-thin` | Number (px). Overlay bar width at `scrollbar-width: thin`. Lumen-native; absent = the runtime default. |
| `scrollbar-margin` | `scrollbar-margin` | Number (px). Gap between the bar and the container's content edge. Lumen-native; absent = the runtime default. |
| `scrollbar-min-thumb` | `scrollbar-min-thumb` | Number (px). Minimum thumb length, so a very long scrollable area still gets a grabbable thumb. Lumen-native; absent = the runtime default. |
| `scrollbar-track-hover` | `scrollbar-track-hover` | Color. Track fill shown while the pointer hovers the scrollbar. Lumen-native; not equivalent to a `:hover { }` rule - see the note below. |
| `scrollbar-hover-boost` | `scrollbar-hover-boost` | Number. Brightness multiplier applied to the thumb fill while hovered, paired with `scrollbar-track-hover`. Lumen-native; absent = the runtime default. |
| `scrollbar-fade-delay` | `scrollbar-fade-delay` | Duration (`Nms` / `Ns`). Idle time before an overlay bar starts fading out. Lumen-native; absent = the runtime default (~1 s). |
| `scrollbar-fade-duration` | `scrollbar-fade-duration` | Duration (`Nms` / `Ns`). Length of the fade-out animation itself. Lumen-native; absent = the runtime default. |

The four geometry properties (`scrollbar-thickness`,
`scrollbar-thickness-thin`, `scrollbar-margin`, `scrollbar-min-thumb`)
apply when the container spawns and are not re-read on a later re-cascade,
so a theme flip or a class change does not resize an existing scrollbar.
`scrollbar-track-hover` and `scrollbar-hover-boost` do re-apply.

Applies to `<scroll>` containers. Overlay bars appear as-needed (only
when content overflows), fade out after an idle delay (`scrollbar-fade-delay`,
default ~1 s), and reappear on scroll or hover. One `scrollbar-color`
value tints the thumb (the translucent track then shows on hover only);
a second value paints the track whenever the bar is visible.
`scrollbar-width: none` hides the bars while the container keeps
scrolling. The default skin sets the thumb via the
`--lumen-scrollbar-thumb` token.

**`scrollbar-track-hover` and `scrollbar-hover-boost` are not
`:hover`-routed**, despite the name. Declare them as plain properties on
the `scroll` rule itself (`scroll { scrollbar-track-hover: #333; }`) -
the "while hovering the bar" behavior is already what the property
means. Putting either one inside an actual `:hover { }` block parses
but has no effect, the same as any other non-state-routed property.

### Custom properties (CSS variables)

```css
:root {
  --bg: #0d0d14;
  --fg: #e6e6e6;
}

root  { bg: var(--bg); text-color: var(--fg); }
label { text-color: var(--fg, #fff); }   /* fallback */
```

Declare a custom property in any rule, not just `:root`. It cascades like
any other declaration, honours `!important`, and inherits to descendants,
so `.theme-light { --bg: #f6f6f9; }` retints that subtree.
`var(--name, fallback)` resolves against the element's effective var
scope: inherited from ancestors, then overridden by every matching rule's
`--custom` declarations in source order. Nested `var()` calls resolve
recursively; a cycle is an error.

Two places read a narrower set. Markup attribute substitution
(`bg="var(--color-bg)"`, below) and the window's GPU clear color both look
only at declarations under a bare `:root` selector. A `:root.dark { }`
block, a `:root { }` inside `@media`, and a `root { }` tag rule are all
invisible to those two, though they work normally everywhere a stylesheet
declaration reaches.

Inline-attribute substitution: markup `bg="var(--color-bg)"` resolves
against the same merged set a stylesheet declaration does - the
built-in Palette theme, the always-on UA baseline, the active skin (if
any), and your own `main.css`, each able to override an earlier layer's
value for the same name. An unresolved call with no fallback degrades
the same way an unresolved `var()` in a stylesheet declaration does: it
is dropped rather than aborting the load, so a typo in a token name
loses that one attribute instead of the whole app. One gap: only the
skin set via `lumen.toml [skin] name` is visible here, because inline
substitution runs before markup is parsed - an explicit `<root
skin="...">` *in the same file*, which normally overrides
`lumen.toml`, is not yet known at that point, so a token that only that
skin defines will not resolve in an inline attribute (it still resolves
everywhere a stylesheet declaration can reach it).

## Gradients

`bg:` accepts three gradient forms in addition to plain colors.

### Linear

`linear-gradient([<deg>], <stop>, <stop>, ...)`. Angle defaults to
`180deg` (top to bottom) and must be written in degrees; the CSS keyword
directions (`to right`, `to bottom`) are not supported. Stops are a color
plus an optional `<offset%>`.

```css
.banner {
  bg: linear-gradient(90deg, #bb9af7, #f7768e);
}

.spectrum {
  bg: linear-gradient(0deg,
       #f7768e   0%,
       #e0af68  25%,
       #9ece6a  50%,
       #7aa2f7  75%,
       #bb9af7 100%);
}
```

### Radial

`radial-gradient(<stop>, <stop>, ...)`. The center is fixed at
50%/50% and the radius at 1.0 (covering the box). `circle at <pos>`
and `ellipse` shapes land in a follow-up.

```css
.spot {
  bg: radial-gradient(#fff, #7aa2f7 70%, #0d0d14);
}
```

### Conic

`conic-gradient([from <deg>], <stop>, <stop>, ...)`. Optional `from
<deg>` sets the start angle; default 0.

```css
.dial {
  bg: conic-gradient(from 0deg, #7aa2f7, #bb9af7, #f7768e, #7aa2f7);
}
```

> **Gradients do not work in a state rule.** `bg` under `:hover`,
> `:active`, `:checked`, and the rest takes a plain color; a gradient
> there is a parse error. To swap a gradient on hover, flip a class from
> script and put the second gradient on that class.

## Shadows

### Markup `shadow=` - one entry

`[inset] <offset-x> <offset-y> <blur> [<spread>] <#color>`. The markup
attribute accepts exactly one shadow spec.

```xml
<tile shadow="0 4 14 #00000077" />
<tile shadow="inset 0 2 6 #00000055" />
```

### CSS `box-shadow:` / `shadow:` - stacked

Comma-separated; the two spellings are the same property. A leading
`inset` keyword on an entry flips it from a drop shadow to an inner one.

```css
.card {
  box-shadow:
    0 1 2 #00000022,
    0 6 24 #00000055,
    inset 0 1 0 #ffffff22;
}
```

## Transitions

CSS shorthand:

```css
.card {
  opacity: 0.5;
  transition: opacity 200ms ease-out;
}

.card.visible {
  opacity: 1.0;
}
```

Class / theme flips that change a transitioned property tween the value
over the declared duration instead of snapping. State pseudo-classes
route through the same declarations: a `transition: bg 130ms ease` on a
`button` drives its `:hover` / `:active` color blend with that duration
and curve.

**Shorthand grammar.**

- Comma-separated entries.
- Each entry: `<property> <duration> [<easing>]`.
- `<duration>`: `Nms` or `Ns` (seconds -> ms).
- `<easing>`: `linear` | `ease` | `ease-in` | `ease-out` | `ease-in-out` | `cubic-bezier(p1x, p1y, p2x, p2y)`. Default `ease-out`.

**Retargeting.** A transition re-triggered mid-flight restarts from the
current interpolated value (never the old endpoint), and equal-value
writes are no-ops - standard CSS Transitions semantics.

**Entering elements.** A mounted / shown element (dialog opened, dropdown
panel revealed, `<if>` body spawned) that declares `transition: opacity`
starts fully transparent and fades to its computed opacity - the
`@starting-style` analogue. An element's `opacity` multiplies into its
whole subtree, so fading a dialog root fades its content too. Removal
transitions (fade-out before despawn) are **not** supported - CSS
cannot express them without JS either; hide/close is instant.

Deep-dive: [Animations + transitions](./animations.md).

## Theming pattern

A common pattern: declare tokens on `:root`, switch a theme class to
flip an alternate scope.

```css
:root {
  --bg: #0d0d14;
  --fg: #e6e6e6;
  --accent: #7aa2f7;
}

.theme-light {
  --bg: #f6f6f9;
  --fg: #0d0d14;
  --accent: #5274d7;
}

root   { bg: var(--bg); text-color: var(--fg); }
button { bg: var(--accent); hover-bg: var(--accent); }
```

Then from a script:

```candela
lumen::set_root_class("app theme-light");
```

The runtime detects `Changed<LumenClasses>` on the root and re-applies
CSS, so the entire token scope flips in a single tick.

### Following the OS theme with `@media`

`@media (prefers-color-scheme: dark|light)` is resolved at runtime
against the live OS theme, and the re-resolver re-runs when the OS
theme changes - no restart. The same pass handles
`@media (prefers-reduced-motion)`, `@media (prefers-contrast)`, and
`@media (min-width | max-width | width: <px>)`.

```css
:root { --bg: #f6f6f9; --fg: #0d0d14; }

@media (prefers-color-scheme: dark) {
  :root { --bg: #0d0d14; --fg: #e6e6e6; }
}

root { bg: var(--bg); text-color: var(--fg); }
```

You can also flip a theme explicitly from script with
`lumen::set_root_class("app theme-light")`; the two approaches compose
(a `@media` block sets defaults, a manual class overrides).

## Text-property inheritance

These properties inherit down the tree the way CSS text properties do:
`text-color`, `font-size`, `font-family`, `font-weight`, `text-align`,
`wrap`, `max-lines`, `line-height`, `selection-color`,
`selection-text-color`, and `caret-color`. Setting `text-color` on `root`
(or any container) cascades to descendant `<label>` / `<input>` text
unless a child overrides it.

Nothing else inherits. That includes `bg`, `padding`, `radius`, and the
named typography `style` role, which applies only to the element that
carries it. Custom properties are the other inheriting case; see
[Custom properties](#custom-properties-css-variables).

## Skins

Lumen ships four embedded skins, all opt-in (no skin = the blank
framework):

| Name | Look |
|---|---|
| `default` | The neutral dark-first skin. |
| `macos` | macOS 14/15-era Aqua: 20px bezel buttons with **no hover state**, pill switch, accent menus, soft accent focus halo, SF font stack at 13px. |
| `windows` | Windows 11 / WinUI 3 (Fluent 2): 4px radii, elevation bottom edge, accent primary buttons, TextBox focus underline, keyboard-only double focus ring, Segoe UI Variable at 14px. |
| `linux` | libadwaita-leaning neutral: flat fg-alpha fills, bold suggested-action accent, 46x26 pill switch, 12px popovers, 50 %-opacity disabled, Adwaita Sans / Inter at 15px. |

`<switch>` is styled in all four skins, each mirroring its own
`<toggle>` treatment (same track/accent fill, per-skin pill geometry).

These four are **independent, mutually exclusive stylesheets, not a
base skin plus per-OS overlays**: selecting `linux` does not layer its
rules on top of `default`. Each skin file declares its own complete
`:root` token block and its own widget rules from scratch. A rule that
reads a token missing from the *active* skin's own `:root` loses that
property silently - like any other malformed declaration in a
stylesheet, an unresolved `var()` there is reported as a `CssWarning`
rather than a hard error, so copying a snippet that assumes another
skin's token vocabulary is a real trap with no visible error to catch
it.

Select one via markup or `lumen.toml` (markup wins when both are set):

```html
<root skin="auto">
```

```toml
[skin]
name = "auto"   # or "default" / "macos" / "windows" / "linux"
```

`auto` resolves once at startup from the running OS (`macos` /
`windows` / anything else -> `linux`). Forcing a concrete name works on
any OS - that's the cross-platform preview path (run the Windows skin
on a Mac to check a design).

Each per-OS skin is **light-first with a full dark override** via
`@media (prefers-color-scheme: dark)`, re-resolved live on OS theme
change. All skins load at the **user-agent origin**, below your
`main.css` (the author origin). Because origin is the first term of the
cascade, **any** author rule beats **any** skin rule for a normal
declaration, regardless of specificity. So an author `.editor { bg:
... }` overrides a skin `textarea:hover { bg: ... }` even though the
skin selector is more specific; you never need a higher-specificity or
`!important` hack to retint a skinned widget. (`!important` still lifts a
declaration above its origin's normal block per the cascade.)

**There is a layer below the skin, and it is not itself a skin.** A
built-in `ua.css` stylesheet applies to every app unconditionally -
skinned, differently-skinned, or with no `skin=` attribute at all - and
it loads first within the user-agent origin, below whichever named
skin (if any) is active, which in turn loads below your own `main.css`.
It carries baseline widget sizing: root and title-bar sizing, `button`
min-height, `input` / `textarea` min-height and min-width, `toggle` /
`switch` / `slider` height and min-width, `checkbox` / `radio`
min-height, `progress` width and height, and the internal
`.checkbox-box` / `.radio-dot` / `.progress-fill` floors. So a bare
`<button>` with no skin opted in and no app CSS still gets a sane
minimum size - "no skin" means no color/shape opinion, not no sizing at
all. Because `ua.css` sits first in the user-agent origin, any app or
skin rule of equal specificity overrides it exactly like overriding a
skin rule - no `!important` needed. A few sizing defaults still come
from Rust instead of `ua.css` because they are conditional on whether
another property was already authored, something a static stylesheet
cannot express, so do not be surprised if diffing `ua.css` against
observed behavior does not explain everything you see.

**There is a layer below even that one, and it is not a skin either.**
A built-in Palette theme (modeled on libadwaita's named-color table)
seeds a light/dark set of custom properties - `--accent-color`,
`--window-bg-color`, `--card-bg-color`, and the rest of the Adwaita
role names, hyphenated - below `ua.css`, below any skin, below your own
CSS. It is a separate vocabulary from the `--lumen-*` tokens below, not
an alias for them, and no shipped skin or `ua.css` rule reads it today,
so opting into a skin looks and behaves exactly as it always has. It
exists so `var(--accent-color)` and friends resolve to *something*
sensible in your own CSS even in a bare, skinless app; redeclare any
name in your own `:root` to override it, same as any other custom
property.

**`button.primary` convention class.** Like `.dialog-surface` and
`.card`, the class `primary` on a `<button>` is a convention the skins
style as the platform's emphasized action - macOS default-button accent
fill, Windows accent button, adwaita suggested-action. Use at most one
per view:

```html
<button class="primary" text="Save" />
```

## Default skin token variables

Opting into an embedded skin (`<root skin="...">` or `[skin] name`
in `lumen.toml`) brings a `:root` block of `--lumen-*` design tokens
that every built-in widget rule reads. Redeclaring any of them in your
own `:root` retints the whole skin in one place, because the resolver
merges the skin and app custom properties into a single cascade on
every reapply:

```css
:root {
  --lumen-surface: #1b2a4a;   /* button / input / dropdown header fill */
  --lumen-accent:  #7aa2f7;   /* focus rings, checked track, selected tab */
  --lumen-text:    #e6e6e6;
}
```

The full token set (`--lumen-surface*`, `--lumen-track*`,
`--lumen-panel*`, `--lumen-border*`, `--lumen-text*`, `--lumen-accent`,
`--lumen-disabled-bg`, ...) lives at the top of each shipped skin in
`lumen/runtime/src/skins/{default,linux,macos,windows}.css`. The per-OS
skins add accent-state and focus tokens on top of the default set:
`--lumen-accent-hover`, `--lumen-accent-active`, `--lumen-on-accent`,
`--lumen-border-strong`, `--lumen-focus-ring`,
`--lumen-focus-ring-width`, `--lumen-input-bg`, `--lumen-window-bg`,
`--lumen-knob`, and (Windows) the elevation-edge pair.

`--lumen-window-bg` does double duty: the per-OS skins also use it to
paint the window's GPU clear color - what you see for an instant while
the window is created, and behind anything your own tree doesn't cover
- so setting it retints both the root element and that clear color
together. Redeclare it in your own `:root` (skinned or not) to change
just the clear color; with no skin and no override it falls back to a
built-in constant.

Every skin also seeds one token per widget-geometry / caret / scrollbar
property from [Property surface](#property-surface) above:
`--lumen-knob-inset`, `--lumen-thumb-size`, `--lumen-popup-gap`,
`--lumen-progress-chunk`, `--lumen-disabled-opacity`,
`--lumen-caret-width`, `--lumen-caret-blink`,
`--lumen-line-height`, and the eight
`--lumen-scrollbar-*` tokens (`thickness`, `thickness-thin`, `margin`,
`min-thumb`, `track-hover`, `hover-boost`, `fade-delay`,
`fade-duration`). Redeclaring one of these in your own `:root` retints
that value the same way as any other token above; see the property's
own row for its accepted value shape.

## Splitting CSS across files (`@import`)

Break a stylesheet into pieces with `@import`:

```css
@import "tokens.css";
@import "widgets.css";

/* main.css's own rules follow */
.hero { bg: var(--brand); }
```

The imported sheets are spliced **ahead of** the importing file's own
rules, so at **equal specificity the importing file wins** the cascade
(later source order beats earlier). Put shared tokens and base rules in
imports; keep the overrides in the file doing the importing.

Rules and limits:

- **Top of file only.** `@import` must appear before any style rule.
  Leading `/* comments */` and whitespace are allowed; an `@import` after
  a rule is an error. (This is stricter than the CSS spec, which also
  permits `@charset` / `@layer` first - Lumen relaxes the spec by
  requiring top-of-file and nothing else.)
- **Order = cascade.** Imports splice in the order written; imported-first
  means the importing file's rules resolve last and win ties.
- **Nested imports** are followed recursively; paths resolve relative to
  the file that wrote the `@import`.
- **Cycles are rejected** with an error naming the chain
  (`@import cycle detected: main.css -> a.css -> b.css -> a.css`).
- A **missing file** is an error naming the importing file.
- Editing any imported file **hot-reloads** the app.
- Only the `@import "path.css";` string form is supported - no
  `url(...)`, media-query, or `@import layer(...)` variants.

Import resolution happens in the load path, not in the CSS parser itself,
so the parser stays a pure string-to-stylesheet function.

## Common pitfalls

- **Inline beats CSS.** A markup `bg="#fff"` is the runtime value;
  CSS `bg:` on the same element only applies if the markup didn't set
  one, unless the CSS declaration is marked `!important`. Switch the
  inline attr to a class to give CSS control.
- **Colors are hex only.** `rgb(0 0 0 / 50%)`, `hsl(...)`, and `red` do
  not parse. So `transparent` is `#00000000`.
- **There is no `calc()`, `em`, or `rem`.** Numbers are px, or `%` where
  a property says so.
- **`overflow: auto` is not a value.** Use `scroll`.
- **Layout properties don't transition.** `transition: width 200ms`
  parses but is dropped with a warning; only `opacity`,
  `background-color`, `color`, and `border-color` animate.
- **A state rule swallows most properties.** Anything inside
  `:hover { }` that is not `bg`, `text-color`, `opacity`, a shadow, or a
  border/outline under the states that route them is dropped with no
  warning at all.
- **`box-shadow: 0 0 0 transparent` is a no-op.** The fill optimization
  drops fully-transparent draws. Use `radius` + `bg` instead.
- **Adjacent / general-sibling combinators (`+`, `~`) parse but don't
  match.** Descendant (` `) and child (`>`) combinators do work.
- **An unsupported at-rule fails the whole stylesheet.** `@keyframes`,
  `@font-face`, `@supports`, and `@layer` are parse errors, not skips.
