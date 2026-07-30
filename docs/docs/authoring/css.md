# CSS subset

Lumen parses a deliberately narrow CSS subset that maps 1:1 to the
markup attribute surface. The grammar and the applicable property list
live in
[`lumenc/src/parser_css.rs`](https://github.com/lumen-ui/lumen/blob/main/lumenc/src/parser_css.rs).
Most properties have the same name as their markup attribute; a few
are CSS-only (notably `transition:` and `box-shadow:` with comma lists).

## Why a subset?

Lumen supports the parts of CSS that map cleanly onto its markup and
layout model: full cascade order (origin -> importance -> specificity ->
source order), descendant / child combinators, structural and state
pseudo-classes, `!important`, and a working `@media` layer. What it
leaves out is the surface that doesn't fit a fixed tag vocabulary -
arbitrary property names, attribute selectors, pseudo-elements, and the
at-rules that imply an animation or capability runtime (`@keyframes`,
`@layer`, `@supports`). The one stylesheet-composition at-rule Lumen
*does* support is `@import` - see [Splitting CSS across files](#splitting-css-across-files-import).

Each supported property either maps 1:1 to a markup attribute or is a
small CSS-only extension (comma-separated `box-shadow`, `transition`).
Inline markup attributes always win over CSS by origin precedence - the
apply pass only fills values the HTML parser left unset - and
`!important` can lift a CSS declaration above that.

The narrow property set also buys a fast invalidation set: every rule's
class names go into a [`Stylesheet::class_invalidation_set`], and any
class flip not in that set is a no-op (Blink's `RuleFeatureSet` pattern
at small scale).

A malformed declaration never aborts the sheet. The offending
`property: value` pair is skipped and reported as a `CssWarning`
(selector + property + reason); the rest of the stylesheet still
applies.

## Grammar

```text
stylesheet  := rule*
rule        := selector "{" declaration* "}"
selector    := ":root" | tag_part? ("." class_part)* (":" pseudo)?
declaration := ident ":" value ";"
```

- `/* comments */` strip cleanly.
- Whitespace is liberal.
- Trailing `;` on the last declaration is optional.
- An unknown property name is silently dropped (matches CSS).
- An unknown selector form is a parse error.

## Selectors

| Selector | Matches |
|---|---|
| `tile` | Every `<tile>` element. |
| `.card` | Every element whose `class` list contains `card`. |
| `tile.card.primary` | A `<tile>` carrying both `card` and `primary` classes. |
| `tile:hover` | `<tile>` while hovered. Pseudo states route to dedicated `Attributes` fields (see below). |
| `:root` | Sugar for `root`. Conventionally used for `:root { --token: value; }`. |

**Supported.**

- Descendant combinator (`.app .card`).
- Child combinator (`.list > .row`).
- `:is(...)`, `:where(...)`, `:not(...)`.
- Structural pseudos (`:first-child`, `:last-child`, `:only-child`,
  `:empty`, `:nth-child(an+b)`, `:root`).
- `!important`.
- `@media (prefers-color-scheme: dark|light|no-preference)`,
  `@media (prefers-reduced-motion)`, `@media (prefers-contrast)`,
  `@media (min-width|max-width|width: <px>)`.

**Not supported.**

- Adjacent / general-sibling combinators (`+`, `~`) - parse but do
  not match at this layer (W4.x follow-up to thread the sibling
  cursor through `apply_to_element`).
- Attribute selectors (`[type="text"]`).
- Pseudo-elements (`::before`, `::placeholder`) - hard parse error.
- `@supports`, `@keyframes`, `@import`, `@layer`.

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
| `:root` | structural | Matches the root element. |
| `:first-child`, `:last-child`, `:only-child`, `:empty`, `:nth-child(N)` | structural | Per Selectors-4. |
| `:is(...)`, `:where(...)`, `:not(...)` | functional | Per Selectors-4 section 17 specificity rules. |

Any pseudo-class outside this set (attribute pseudos, pseudo-elements,
...) is a parse error so typos don't silently no-op.

**State-routed properties.** Under `:hover` / `:focus` /
`:focus-visible` / `:active` / `:disabled`, these properties swap with
the state and restore when it ends:

* `bg` - the fill (tweened for hover/press; see the pseudo table),
* `border` (hover/focus) and `outline` + `outline-offset` (focus,
  focus-visible),
* `text-color` - e.g. Windows' pressed-text drop to secondary,
* `opacity` - e.g. adwaita's documented 50 %-opacity disabled controls,
* `box-shadow` - full stack replacement; e.g. the WinUI TextBox 2px
  accent focus underline (`input:focus { box-shadow: inset 0 -2 0
  var(--lumen-accent); }`).

Other properties inside a state rule are consumed without effect (no
warning) - geometry is not state-routable.

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
suffix interchangeably - `radius: 8` and `radius: 8px` are the same,
as are `font-size: 16` and `font-size: 16px`. Values that don't parse
as a number surface as a `CssWarning` rather than silently applying.

### Layout

| CSS property | Markup attr | Value |
|---|---|---|
| `width`, `height` | `width`, `height` | Length (`24`, `50%`). |
| `min-width`, `min-height`, `max-width`, `max-height` | same | Length. |
| `aspect-ratio` | `aspect-ratio` | Number (w/h ratio). |
| `padding`, `margin` | same | Edges (1, 2, 3, or 4 terms); each term is px **or** `%` (percent resolves against the containing block's width, per CSS). |
| `gap` | `gap` | 1 or 2 terms (`<row+col>` / `<row> <col>`); px or `%`. |
| `row-gap`, `column-gap` | *CSS-only* | Number (px) or `%`. |
| `grow` | `grow` | Number (flex grow factor). |
| `flex-shrink` | `shrink` | Number (flex shrink factor; default `1`). |
| `flex-basis` | *CSS-only* | Length (`auto`, px, `%`). |
| `flex` | *CSS-only* | `<grow> [<shrink>] [<basis>]`, plus `none` / `auto` / `initial` - exact CSS shorthand semantics (`flex: 1` = `1 1 0%`). |
| `flex-wrap` | *CSS-only* | `nowrap` \| `wrap` \| `wrap-reverse`. |
| `align-content` | *CSS-only* | `start` \| `end` \| `center` \| `stretch` \| `space-between` \| `space-around` \| `space-evenly`. Distribution of wrapped flex lines. |
| `flex-direction` | `flex` | `row` \| `column` \| `row-reverse` \| `column-reverse`. |
| `box-sizing` | *CSS-only* | `border-box` (Lumen UA default) \| `content-box`. |
| `z-index` | `z-index` | Integer - sibling paint-order override (higher paints on top; equal keeps document order). |
| `align` | `align` | `start` \| `end` \| `center` \| `stretch`. |
| `justify` | `justify` | `start` \| `end` \| `center` \| `between` \| `around` \| `evenly`. |
| `position` | `position` | `relative` \| `absolute`. |
| `inset` | `inset` | Edges. Only meaningful with `position: absolute`. |
| `overflow`, `overflow-x`, `overflow-y` | same | `visible` \| `hidden` \| `scroll`. |
| `scroll` | `scroll` | `y` \| `x` \| `both`. |
| `sensitivity`, `inertia` | same | Scroll-tuning numbers. |
| `tab-index` | `tab-index` | Integer. |
| `draggable` | `draggable` | `true` \| `false`. |
| `layout-boundary` | `layout-boundary` | bool - taffy subtree isolation. |

### Visuals

| CSS property | Markup attr | Value |
|---|---|---|
| `bg` | `bg` | Color **OR** gradient (see below). |
| `radius` | `radius` | 1-4 numbers (px). Multi-value follows the CSS `border-radius` shorthand rotation `[top-left, top-right, bottom-right, bottom-left]` - `radius: 4 4 0 0` rounds only the top corners. |
| `border-top-left-radius`, `border-top-right-radius`, `border-bottom-right-radius`, `border-bottom-left-radius` | *CSS-only* | Number (px) - per-corner longhands. |
| `text-color` | `text-color` | Color. |
| `selection-color` | `selection-color` | Color. Text-selection highlight on `input` / `textarea` (skin default: `var(--lumen-selection)`). |
| `opacity` | `opacity` | 0..1. |
| `shadow` | `shadow` | Single shadow: `[inset] <ox> <oy> <blur> [<spread>] <color>`. |
| `box-shadow` | *none - CSS-only* | Comma-separated shadow stack; each entry `[inset] <ox> <oy> <blur> [<spread>] <color>`. Spread inflates (or deflates, when negative) the shadow rect before blurring - `box-shadow: 0 0 0 2 #fff` draws a hard 2px ring, the standard double-focus-ring idiom. |

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
| `border-style` | *CSS-only* | `none` \| `solid`. Per CSS, without a style the computed width is `0` - width/color alone paint nothing. |
| `border-top-width`, `border-right-width`, `border-bottom-width`, `border-left-width` | *CSS-only* | Number. Useful for separators (`border-bottom-width: 1`). |
| `border-inline-start-width`, `border-inline-end-width`, `border-block-start-width`, `border-block-end-width` | *CSS-only* | Number - CSS Logical Properties, resolved against the element's writing direction. |

### Text

| CSS property | Markup attr | Value |
|---|---|---|
| `font-size` | `font-size` | Number (px). |
| `font-family` | `font-family` | CSS fallback chain, e.g. `"Segoe UI Variable Text", "Segoe UI", sans-serif`. The shaper resolves the first family present in the system font database (case-insensitive; quotes optional); the generic keywords `sans-serif`, `serif`, `monospace`, `cursive`, `fantasy` (plus the platform aliases `system-ui`, `ui-sans-serif`, `ui-serif`, `ui-monospace`, `-apple-system`) map to the platform families. An unresolvable chain falls back to sans-serif. Inherited. |
| `font-weight` | `font-weight` | `normal` (400), `bold` (700), or a number `1..=1000` (CSS Fonts 4). Variable and multi-weight families select the nearest face; the relative keywords `lighter` / `bolder` are rejected. Inherited. |
| `wrap` (alias `white-space`) | `wrap` | `none` (= `nowrap`) \| `word` (= `normal`) \| `glyph`. Markup additionally accepts `wrap="ellipsis"` as shorthand for `text-overflow: ellipsis`. |
| `max-lines` | `max-lines` | Integer >= 0. Overflowing text past the cap is elided with `...`. |
| `text-overflow` | `wrap="ellipsis"` | `clip` \| `ellipsis`. `ellipsis` elides overflowing single-line text with a trailing `...` (Qt elide contract; the ellipsis is trimmed to fit inside the box). Combine with an explicit `wrap` + `max-lines` for a multi-line clamp. Not inherited. The built-in skins set it on `.tab-btn`, `.dropdown-button`, and `.dropdown-option` - the surfaces Qt elides. |
| `text-align` | `text-align` | `start` \| `center` \| `end`. |
| `style` | `style` | Named typography role - see below. |

**Named typography roles (`style`).** A `style` value names a
predefined type scale that resolves to a `font-size`, so you can write
`style="headline-lg"` instead of picking pixel sizes by hand. An
explicit `font-size` (inline or CSS) still overrides the role's size.
The roles are, largest to smallest: `display-xl`, `display-lg`,
`display-md`, `display-sm`, `headline-lg`, `headline-md`,
`headline-sm`, `title-lg`, `title-md`, `title-sm`, `body-lg`,
`body-md`, `body-sm`, `label-lg`, `label-md`, `label-sm`, `caption`,
`overline`.

### Interaction

| CSS property | Markup attr | Value |
|---|---|---|
| `hover-bg` | `hover-bg` | Color. Equivalent to `:hover { bg: ... }`. |
| `press-bg` | `press-bg` | Color. Equivalent to `:active { bg: ... }`. |
| `focus-outline` | `focus-outline` | `<width-px> <#color>`. Draws outside the box while focused (CSS `outline` semantics - never affects layout). Same as `:focus { outline: ... }`; use `:focus-visible { outline: ... }` for a keyboard-only ring. |
| `outline-offset` | *CSS-only* | Number (px) - gap between the border box edge and the focus ring's inner edge. Valid at rest or inside `:focus` / `:focus-visible`. |
| `knob-color` | `knob-color` | Color - fill of a `<toggle>`'s knob / `<slider>`'s thumb child (Lumen-native analog property; the child is not selector-reachable). The skins seed it; absent = the runtime fallback. |
| `hover-border` | *CSS-only* | Border shorthand. Equivalent to `:hover { border: ... }`. |
| `focus-border` | *CSS-only* | Border shorthand. Equivalent to `:focus { border: ... }`; wins over `hover-border` when both states are active. |

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

> **Animatable set (v1):** `opacity`, `background-color` (aliases
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

Applies to `<scroll>` containers. Overlay bars appear as-needed (only
when content overflows), fade out after ~1 s of inactivity, and reappear
on scroll or hover. One `scrollbar-color` value tints the thumb (the
translucent track then shows on hover only); a second value paints the
track whenever the bar is visible. `scrollbar-width: none` hides the
bars while the container keeps scrolling. The default skin sets the
thumb via the `--lumen-scrollbar-thumb` token.

### Custom properties (CSS variables)

```css
:root {
  --bg: #0d0d14;
  --fg: #e6e6e6;
}

root  { bg: var(--bg); text-color: var(--fg); }
label { text-color: var(--fg, #fff); }   /* fallback */
```

`var(--name, fallback)` resolves against the element's effective var
scope: inherited from ancestors, then overridden by every matching
rule's `--custom` declarations in source order.

Inline-attribute substitution: markup `bg="var(--color-bg)"` resolves
through the same lookup (T2d).

## Gradients

`bg:` accepts three gradient forms in addition to plain colors.

### Linear

`linear-gradient([<deg>], <stop>, <stop>, ...)`. Angle defaults to
`180deg` (top to bottom). Stops are color + optional `<offset%>`.

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

`radial-gradient(<stop>, <stop>, ...)`. v1 keeps the center fixed at
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

> **HoverTint over gradients.** The hover-tint tween only animates solid
> fills. Gradient backgrounds skip the tween path - a gradient snaps to
> its hover counterpart instead of blending.

## Shadows

### Markup `shadow=` - single

`[inset] <offset-x> <offset-y> <blur> <#color>`. Markup form accepts
exactly one shadow spec.

```xml
<tile shadow="0 4 14 #00000077" />
<tile shadow="inset 0 2 6 #00000055" />
```

### CSS `box-shadow:` - stacked

Comma-separated. Leading `inset` keyword on each entry flips drop ->
inner.

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
- `<easing>`: `linear` | `ease` | `ease-in` | `ease-out` | `ease-in-out` | `cubic-bezier(p1x, p1y, p2x, p2y)`. Default `ease-out` (close enough to CSS `ease` for v1).

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

Then from Rhai:

```rhai
set_root_class("app theme-light");
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
`set_root_class("app theme-light")`; the two approaches compose (a
`@media` block sets defaults, a manual class overrides).

## Text-property inheritance

Text properties (`text-color`, `font-size`, `font-family`,
`font-weight`, `text-align`, `wrap`, `max-lines`, and the named `style`
role) inherit down the tree the way CSS text properties do. Setting `text-color` on `root` (or any
container) cascades to descendant `<label>` / `<input>` text unless a
child overrides it. Non-text properties (`bg`, `padding`, `radius`, ...)
do not inherit.

## Skins

Lumen ships four embedded skins, all opt-in (no skin = the blank
framework):

| Name | Look |
|---|---|
| `default` | The neutral dark-first skin. |
| `macos` | macOS 14/15-era Aqua: 20px bezel buttons with **no hover state**, pill switch, accent menus, soft accent focus halo, SF font stack at 13px. |
| `windows` | Windows 11 / WinUI 3 (Fluent 2): 4px radii, elevation bottom edge, accent primary buttons, TextBox focus underline, keyboard-only double focus ring, Segoe UI Variable at 14px. |
| `linux` | libadwaita-leaning neutral: flat fg-alpha fills, bold suggested-action accent, 46x26 pill switch, 12px popovers, 50 %-opacity disabled, Adwaita Sans / Inter at 15px. |

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
declaration -- regardless of specificity. So an author `.editor { bg:
... }` overrides a skin `textarea:hover { bg: ... }` even though the
skin selector is more specific; you never need a higher-specificity or
`!important` hack to retint a skinned widget. (`!important` still lifts a
declaration above its origin's normal block per the cascade.)

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
`lumenc/src/skins/{default,macos,windows,linux}.css`. The per-OS skins
add accent-state and focus tokens on top of the default set:
`--lumen-accent-hover`, `--lumen-accent-active`, `--lumen-on-accent`,
`--lumen-border-strong`, `--lumen-focus-ring`,
`--lumen-focus-ring-width`, `--lumen-input-bg`, `--lumen-window-bg`,
`--lumen-knob`, and (Windows) the elevation-edge pair.

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
- **Layout properties don't transition.** `transition: width 200ms`
  parses but is dropped with a warning - v1 animates colors
  (`bg` / `color` / `border-color`) and `opacity` only.
- **`box-shadow: 0 0 0 transparent` is a no-op.** The fill optimization
  drops fully-transparent draws. Use `radius` + `bg` instead.
- **Adjacent / general-sibling combinators (`+`, `~`) parse but don't
  match.** Descendant (` `) and child (`>`) combinators do work.
