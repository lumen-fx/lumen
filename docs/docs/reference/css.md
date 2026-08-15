# CSS

Every selector, property, and value form Lumen's CSS subset accepts. For
the task-level introduction see [Styling](../guides/styling.md); for the
markup surface see [Tags and attributes](tags.md).

Lumen implements a subset of CSS Selectors 4, Cascade 5, and Media
Queries 5, plus a few properties of its own for parts that standard CSS
cannot address. Anything outside the subset is reported as a warning and
the declaration is dropped; the rest of the stylesheet still applies.

## Grammar

```text
stylesheet    := at_import* (at_media | rule)*
at_import     := "@import" '"' path '"' ";"
at_media      := "@media" media_query "{" rule* "}"
rule          := selector_list "{" declaration* "}"
selector_list := selector ("," selector)*
selector      := compound (combinator compound)*
combinator    := " " | ">" | "+" | "~"
compound      := tag? ("." class | "#" id | ":" pseudo_class)*
declaration   := ident ":" value "!important"? ";"
```

`/* ... */` comments are stripped anywhere. `!important` is matched
case-insensitively at the end of a value.

`@import "other.css";` splices another stylesheet in place. Paths are
relative to the importing file. Imports must come before any rule; one
written later is an error, as is a cycle. They nest. Imported rules are
placed before the importing file's own rules, so at equal specificity the
importing file wins.

`@media` blocks nest up to 32 levels, as do `:is()`, `:where()`, and
`:not()` arguments.

`@import` and `@media` are the at-rules Lumen implements. Any other one,
`@keyframes` and `@font-face` included, is skipped with a warning along
with its whole block, and the rest of the stylesheet applies.

Pseudo-elements (`::before` and friends) are a parse error, not a warning.

## Selectors

### Simple selectors

| Form | Matches |
| --- | --- |
| `tag` | Elements with that tag name. |
| `.class` | Elements listing that class. |
| `#id` | The element with that `id`. |
| `*` | Any element. |

A compound writes the tag first: `button.primary#save`.

### Combinators

| Combinator | Meaning |
| --- | --- |
| `A B` | `B` with an `A` ancestor. |
| `A > B` | `B` whose immediate parent is `A`. |
| `A + B` | `B` immediately preceded by a sibling `A`. |
| `A ~ B` | `B` preceded anywhere by a sibling `A`. |

Each step takes the nearest candidate that matches it and does not
reconsider: `.a .b .c` binds `.b` to the closest matching ancestor, and
`.a ~ .b ~ .c` binds `.b` to the closest matching earlier sibling. A
selector that would only match through a farther candidate misses.

Sibling steps resolve when the stylesheet is applied to the document.
Restyles that run against one element on its own (a `<for>` row, a class
change from a script, an OS theme flip) walk ancestors only, so a rule
that needs a sibling step is not reapplied there.

### Pseudo-classes

State pseudo-classes select a routing slot rather than filtering
elements: a rule written with one always applies, and the runtime swaps
the value in when the element enters that state.

| Pseudo-class | Matches |
| --- | --- |
| `:hover` | Pointer over the element. |
| `:focus` | Element has keyboard focus. |
| `:focus-visible` | Focus arrived from the keyboard. |
| `:active` | Element is pressed. |
| `:disabled` | Element is disabled. |
| `:checked` | `<toggle>`, `<switch>`, or `<checkbox>` is on. |
| `:selected` | Tab strip button of the active tab. |
| `:drag-over` | An acceptable in-app drag hovers this drop target. |

Structural pseudo-classes filter at compile time:

| Pseudo-class | Matches |
| --- | --- |
| `:root` | The document root, or any element whose tag is `root`. |
| `:first-child` | First element child of its parent. |
| `:last-child` | Last element child. |
| `:only-child` | The sole child. |
| `:empty` | No element children and no text. |
| `:nth-child(an+b)` | 1-based position. `odd`, `even`, `n`, `-n`, and plain integers are accepted. |
| `:is(list)` | Any selector in the list. |
| `:where(list)` | Any selector in the list, contributing zero specificity. |
| `:not(list)` | No selector in the list. |

`:is()`, `:where()`, and `:not()` take a comma-separated list of
selectors, each of which may use combinators. The element being tested is
the subject of every argument, so `.row:not(.list > .row)` matches a
`.row` that is not a child of a `.list`.

### Specificity

Specificity is the triple `(a, b, c)` compared lexicographically:

- `a` counts `#id` selectors.
- `b` counts classes and pseudo-classes.
- `c` counts tag names.

`:is()` and `:not()` take the specificity of their most specific
argument. `:where()` contributes nothing.

## Cascade

Declarations are ordered by, in decreasing priority:

1. Origin. The built-in skin sheets are user-agent origin; your CSS is
   author origin and wins, whatever the specificity.
2. Importance, per declaration. `!important` applies to the one
   declaration it is written on, not to its siblings in the same rule.
3. Specificity.
4. Source order, then position within a selector list. The last matching
   declaration wins.

Markup attributes beat CSS, for every property both surfaces can set.
`<tile width="50px"/>` keeps its width against `.t { width: 100px }`.

`flex-direction` is the one property CSS wins: `<row>`, `<column>`,
`<scroll>`, and the other layout tags carry a direction that comes from
the tag rather than from anything you wrote, so a stylesheet can change
it. `<overlay>` and `<dialog>` take `position: absolute` and a full
`inset` from their tag too, but those two count as markup and a
stylesheet cannot move them.

One exception keeps skins from repainting your surfaces: when you set a
resting `bg` (in CSS or as an attribute), a skin's `:hover` and `:active`
background for that element is dropped. `:checked`, `:selected`,
`:disabled`, and `:drag-over` backgrounds from the skin still stand until
you name that state yourself.

### Inheritance

These properties inherit from parent to child: `text-color` (`color`),
`selection-color`, `selection-text-color`, `caret-color`, `font-size`,
`font-family`, `font-weight`, `line-height`, `text-align`, `wrap`, and
`max-lines`. Custom properties inherit through their own scope. Every
other property applies only where it matches.

An inherited value acts as a zero-specificity base, so any matching rule
or markup attribute on the child overrides it.

### Error recovery

A declaration with an unknown property, an unparseable value, or an
unresolvable `var()` is dropped and reported as a warning naming the
selector and the property. The rest of the stylesheet applies. Duplicate
warnings are reported once.

A declaration under a state pseudo-class that is not one of the
state-routable properties below is accepted and has no effect.

## Custom properties

Any declaration whose name starts with `--` is a custom property. It is
stored, not applied, and inherits to descendants.

```css
:root {
  --accent: #33c7ce;
  --panel: #0c2c4a;
}

.card {
  bg: var(--panel);
  border: 1px solid var(--accent);
}
```

`var(--name)` substitutes the value; `var(--name, fallback)` uses the
fallback when the name is unset. References resolve recursively up to 16
levels; a cycle or an unknown name with no fallback drops the
declaration with a warning.

Custom properties cascade like other declarations, including
`!important`, and a rule matched on an ancestor puts its values in scope
for descendants. That makes `:root.theme-dark { --panel: ... }` a working
theme switch.

The runtime reads two custom properties directly: `--lumen-tooltip-delay`
(milliseconds) and `--lumen-tooltip-offset` (pixels), because `<tooltip>`
collapses at parse time and no selector can reach it. The built-in skins
define the `--lumen-*` token set they theme themselves with; redeclaring
those names in your own `:root` block retints the skin.

## @media

```css
@media (prefers-color-scheme: dark) and (min-width: 700px) {
  .card { bg: #101014; }
}
```

Features are joined with `and`. Comma-separated query lists, `or`, and
`not` are not supported.

| Feature | Values | Matches when |
| --- | --- | --- |
| `prefers-color-scheme` | `dark`, `light`, `no-preference` | The OS color scheme equals the value. With no known scheme, only `no-preference` matches. |
| `prefers-reduced-motion` | `reduce`, `no-preference` | Always `no-preference`; see below. |
| `prefers-contrast` | `more`, `less`, `custom`, `no-preference` | Always `no-preference`; see below. |
| `min-width` | length in px | The viewport is at least that wide. |
| `max-width` | length in px | The viewport is at most that wide. |
| `width` | length in px | The viewport is that wide. |

Lumen reads the OS color scheme and the window size. It does not read the
motion or contrast preference from the OS yet, so `prefers-reduced-motion`
and `prefers-contrast` only ever match `no-preference`. Give people an
in-app switch for anything heavily animated or low-contrast.

## Value forms

| Form | Accepted |
| --- | --- |
| length | `auto`, `<n>`, `<n>px`, `<n>%`. A bare number is pixels. |
| number | `<n>`, `<n>px`. The `px` suffix is accepted and stripped. |
| color | `#rrggbb`, `#rrggbbaa`. |
| edges | 1 to 4 terms in top-right-bottom-left rotation; each `<n>`, `<n>px`, or `<n>%`. |
| border width | `thin` (1px), `medium` (3px), `thick` (5px), or a px length. Percentages are rejected. |
| duration | `<n>ms` or `<n>s`. |
| easing | `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, `cubic-bezier(a, b, c, d)`. |
| track size | `<n>`, `<n>px`, `<n>fr`, `auto`, `min-content`, `max-content`, `minmax(<min>, <max>)`. |

### Gradients

The `bg` property accepts three gradient functions. Each stop is a colour
with an optional position, given as a percentage or a 0..1 number. Stops
without a position are distributed evenly, and all stops are sorted by
position.

```css
.a { bg: linear-gradient(90deg, #101014, #202028 60%, #303040); }
.b { bg: radial-gradient(#ffffff, #000000); }
.c { bg: conic-gradient(from 45deg, #ff0000, #00ff00, #0000ff); }
```

- `linear-gradient([<angle>deg,] <stop>, <stop>, ...)`. The angle
  defaults to `180deg`, top to bottom. At least two stops.
- `radial-gradient(<stop>, <stop>, ...)`. The centre is fixed at the
  middle of the box and the radius covers it. At least two stops.
- `conic-gradient([from <angle>[deg],] <stop>, <stop>, ...)`. The start
  angle defaults to `0`. At least two stops.

### Shadows

```text
box-shadow: [inset] <x> <y> <blur> [<spread>] <#color> [, ...]
```

`inset` draws the shadow inside the box. Multiple shadows are comma
separated and paint in order. `shadow` is an accepted synonym.

## Property catalogue

The Lumen short names and the standard CSS names below are
interchangeable: `color` for `text-color`, `background` and
`background-color` for `bg`, `border-radius` for `radius`, `flex-grow`
for `grow`, `flex-shrink` for `shrink`, `justify-content` for `justify`,
`object-fit` for `fit`, and `white-space` for `wrap`.

### Layout

| Property | Values | Default |
| --- | --- | --- |
| `display` | `flex`, `grid`, `none` | `flex` |
| `width`, `height` | length | content size |
| `min-width`, `min-height`, `max-width`, `max-height` | length | unconstrained |
| `aspect-ratio` | number | none |
| `padding`, `margin` | edges | `0` |
| `inset` | edges | auto |
| `position` | `relative`, `absolute` | `relative` |
| `box-sizing` | `border-box`, `content-box` | `border-box` |
| `flex-direction` | `row`, `column`, `row-reverse`, `column-reverse` | from the tag |
| `flex-wrap` | `nowrap`, `wrap`, `wrap-reverse` | `nowrap` |
| `flex` | `none`, `auto`, `initial`, or `<grow> [<shrink>] [<basis>]` | |
| `grow`, `flex-grow` | number | `0` |
| `shrink`, `flex-shrink` | number | `1` |
| `flex-basis` | length | `auto` |
| `gap` | `<both>` or `<row> <column>`; each may be a percentage | `0` |
| `row-gap`, `column-gap` | number or percentage | `0` |
| `align`, `align-items` | `start`/`flex-start`, `end`/`flex-end`, `center`, `stretch`, `baseline` | `stretch` |
| `align-self` | same as `align-items` | from the container |
| `align-content` | `start`/`flex-start`, `end`/`flex-end`, `center`, `stretch`/`normal`, `space-between`, `space-around`, `space-evenly` | `stretch` |
| `justify`, `justify-content` | `start`, `end`, `center`, `between`/`space-between`, `around`/`space-around`, `evenly`/`space-evenly` | `start` |
| `justify-items` | same as `align-items` | grid only |
| `justify-self` | same as `align-items` | from `justify-items` |
| `z-index` | integer or `auto` | document order (`auto` is `0`) |
| `overflow`, `overflow-x`, `overflow-y` | `visible`, `hidden`, `scroll` | `visible` |
| `layout-boundary` | `true`, `yes` | automatic |

`overflow: scroll` makes an element a live scroll container.

### Logical properties

Each takes a px number and resolves against the element's writing
direction (`dir` in markup).

- `padding-inline-start`, `padding-inline-end`, `padding-block-start`, `padding-block-end`
- `margin-inline-start`, `margin-inline-end`, `margin-block-start`, `margin-block-end`
- `inset-inline-start`, `inset-inline-end`, `inset-block-start`, `inset-block-end`
- `border-inline-start-width`, `border-inline-end-width`, `border-block-start-width`, `border-block-end-width`

### Grid

| Property | Values | Default |
| --- | --- | --- |
| `grid-template-rows`, `grid-template-columns` | whitespace-separated track sizes | none |
| `grid-row`, `grid-column` | `<start>` or `<start> / <end>` | auto placement |

Track lists take plain track sizes. `repeat()`, named lines, and
`span` are not supported. Line numbers outside the range of a 16-bit
integer are clamped with a warning.

### Visuals

| Property | Values | Default |
| --- | --- | --- |
| `bg` | color or gradient | transparent |
| `radius` | 1 to 4 numbers, top-left / top-right / bottom-right / bottom-left | `0` |
| `border-top-left-radius`, `border-top-right-radius`, `border-bottom-right-radius`, `border-bottom-left-radius` | number | from `radius` |
| `opacity` | number, clamped to 0..1 | `1` |
| `shadow`, `box-shadow` | shadow list | none |
| `fit`, `object-fit` | `fill`, `cover`, `contain`, `none`, `scale-down` | `fill`; `<image>` only |
| `disabled-opacity` | number, clamped to 0..1 | `0.5` |
| `knob-color` | color | built-in knob fill |
| `knob-inset` | number | `4` |
| `thumb-size` | number | `16` |
| `popup-gap` | number | `4` |
| `draggable` | `true`, `yes` | `false` |

`disabled-opacity` sets how much a disabled element dims when neither
`:disabled { bg }` nor `:disabled { opacity }` was authored.
`knob-color`, `knob-inset`, `thumb-size`, and `popup-gap` reach widget
parts that have no selector of their own.

### Borders and outlines

| Property | Values | Default |
| --- | --- | --- |
| `border` | `<width> \|\| <style> \|\| <color>` in any order, or `none` | none |
| `border-width` | 1 to 4 border widths | `medium` when a style is set |
| `border-style` | `none`, `hidden`, `solid` | none |
| `border-color` | color | the element's text color, else black |
| `border-top-color`, `border-right-color`, `border-bottom-color`, `border-left-color` | color | from `border-color` |
| `border-top`, `border-right`, `border-bottom`, `border-left` | per-side shorthand | none |
| `border-top-width`, `border-right-width`, `border-bottom-width`, `border-left-width` | border width | `0` |
| `focus-outline` | `<width> <#color>` | none |
| `outline-offset` | number | `0` |
| `hover-border`, `focus-border` | border shorthand | none |

`solid` and `none` are the supported border styles; `dashed`, `dotted`,
and the rest are rejected with a warning. A shorthand that omits the
style but names a width or a colour resolves to `solid`. Without a style
there is no border: the computed width is zero and nothing paints.

Borders participate in layout under `box-sizing`.

### Text

| Property | Values | Default | Inherited |
| --- | --- | --- | --- |
| `text-color` | color | the built-in text color | yes |
| `font-size` | number | `16` | yes |
| `font-family` | family list | platform sans-serif | yes |
| `font-weight` | `normal` (400), `bold` (700), 1..1000 | `400` | yes |
| `line-height` | number (multiplier) or `<n>px` | runtime ratio | yes |
| `text-align` | `start`/`left`, `center`, `end`/`right` | `start` | yes |
| `wrap` | `none`/`nowrap`, `word`/`normal`, `glyph`/`char` | `none` | yes |
| `max-lines` | non-negative integer | unlimited | yes |
| `text-overflow` | `clip`, `ellipsis` | `clip` | no |

The shaper resolves the first available family in a `font-family` list,
honouring the generic keywords, and falls back to the platform
sans-serif. `lighter` and `bolder` are rejected: they need the parent's
computed weight.

`text-overflow: ellipsis` elides overflowing single-line text unless you
also author `wrap` and `max-lines`, in which case your multi-line clamp
is ellipsized instead.

### Text input

These apply to `<input>` and `<textarea>`.

| Property | Values | Default |
| --- | --- | --- |
| `selection-color` | color | text fill at reduced alpha |
| `selection-text-color` | color | selected glyphs keep their fill |
| `caret-color` | color | the text fill |
| `caret-width` | number | `2` |
| `caret-blink` | duration | `530ms` |
| `password-character` | exactly one character | a bullet |

The caret blink phase is shared by the whole app, so `caret-blink` sets
one period for every input; it is applied when styles are reapplied, not
at startup.

### Interaction states

State-routable properties, and where each state's value lands:

| Property | `:hover` / `:focus` / `:focus-visible` | `:active` | `:checked` | `:selected` | `:disabled` | `:drag-over` |
| --- | --- | --- | --- | --- | --- | --- |
| `bg` | yes, one shared slot | yes | yes | yes | yes | yes |
| `text-color` | separate slot per state | yes | no | no | yes | yes |
| `opacity` | separate slot per state | yes | no | no | yes | yes |
| `shadow`, `box-shadow` | separate slot per state | yes | no | no | yes | yes |
| `border` | yes; `:focus` wins over `:hover` | no | no | no | no | no |
| `outline` | `:focus` and `:focus-visible` only | no | no | no | no | no |
| `outline-offset` | `:focus` and `:focus-visible` only | no | no | no | no | no |

`bg` under `:hover`, `:focus`, and `:focus-visible` shares one slot, so
the three cannot differ. `outline` under `:focus-visible` gets its own
slot and wins over `:focus` while focus came from the keyboard.

The same swaps are reachable as plain properties, which is often shorter:

| Property | Equivalent |
| --- | --- |
| `hover-bg` | `:hover { bg }` |
| `press-bg` | `:active { bg }` |
| `hover-border` | `:hover { border }` |
| `focus-border` | `:focus { border }` |
| `focus-outline` | `:focus { outline }` |

### Progress

| Property | Values | Default |
| --- | --- | --- |
| `progress-duration` | integer milliseconds, greater than zero | `1200` |
| `progress-chunk` | number 0..1 | `0.3` |

`progress-duration` is the indeterminate sweep period and
`progress-chunk` the fraction of the track the moving chunk covers.

### Transitions

```css
.card {
  transition: bg 150ms ease-out, opacity 100ms linear;
}
```

| Property | Values | Default |
| --- | --- | --- |
| `transition` | `<property> <duration> [<easing>]`, comma separated | none |
| `transition-property` | comma-separated property names, `all`, or `none` | none |
| `transition-duration` | comma-separated durations | `0` |
| `transition-timing-function` | comma-separated easings | `ease` |

Animatable properties: `opacity`, `background-color` (`background`,
`bg`), `color` (`text-color`), and `border-color`. `all` stands for all
four. Any other name in a transition list is ignored with a warning;
layout properties are deliberately excluded so a transition never re-runs
layout every frame.

An entry that names no easing gets `ease`. A second duration in an entry
is a delay, which Lumen warns about and ignores: a transition always
starts on the tick the value changes. `transition-delay` is warned about
and ignored for the same reason.

The `transition` shorthand resets the longhands. Otherwise
`transition-property` defines the list and the duration and easing lists
cycle over it; a duration list without a property list produces nothing.

### Scrollbars

| Property | Values | Default |
| --- | --- | --- |
| `scrollbar-color` | `auto`, or `<thumb> [<track>]` colors | `auto` |
| `scrollbar-width` | `auto`, `thin`, `none` | `auto` |
| `scrollbar-thickness` | number | `8` |
| `scrollbar-thickness-thin` | number | `4` |
| `scrollbar-margin` | number | `2` |
| `scrollbar-min-thumb` | number | `24` |
| `scrollbar-track-hover` | color | no track tint |
| `scrollbar-hover-boost` | number | `1.6` |
| `scrollbar-fade-delay` | duration | `1s` |
| `scrollbar-fade-duration` | duration | `250ms` |

`scrollbar-color` and `scrollbar-width` are the standard CSS properties;
the rest are Lumen's own geometry and timing controls for the overlay
scrollbar.

### Scrolling and focus

| Property | Values | Default |
| --- | --- | --- |
| `scroll` | `x`, `y`, `both` | none |
| `sensitivity` | number | `1.0` |
| `inertia` | number | `0.4` |
| `tab-index` | integer | from the tag |

## Web target mapping

Built for the web, an app ships its stylesheet as real CSS and the
browser runs the cascade. Most of what you write arrives unchanged; this
section is the rest.

### Selectors

A tag selector becomes the class the element carries in the document,
inside `:where()`: `button` is emitted as `:where(.lm-button)`. The
wrapper keeps the tag weighing nothing, so it still loses to a class the
way it does everywhere else.

The four states no browser selector covers are mirrored onto attributes:

| Lumen | Emitted as |
| --- | --- |
| `:selected` | `[data-lm-selected]` |
| `:checked` | `:is(:checked, [data-lm-checked])` |
| `:disabled` | `:is(:disabled, [data-lm-disabled])` |
| `:drag-over` | `[data-lm-drag-over]` |

`:checked` and `:disabled` keep the browser's own state beside the
mirror, because a `<checkbox>` is a real checkbox there and a `<toggle>`
is a button. Every other selector, `:hover` and the structural
pseudo-classes included, is emitted as written.

Rules are written out in the order Lumen's cascade puts them in, so where
the browser cannot tell two rules apart it picks the one Lumen picks.

### Property names

The Lumen short names are emitted under their standard spelling:

| Lumen | Emitted as |
| --- | --- |
| `bg` | `background` |
| `text-color` | `color` |
| `radius` | `border-radius` |
| `grow` | `flex-grow` |
| `justify` | `justify-content` |
| `fit` | `object-fit` |
| `shadow` | `box-shadow` |
| `align` | `align-items` |
| `wrap` | `white-space` and `overflow-wrap` |
| `scroll` | `overflow-x`, `overflow-y` |
| `max-lines` | the `-webkit-line-clamp` block |

`justify: between`, `around`, and `evenly` are emitted as
`space-between`, `space-around`, and `space-evenly`.

### States written as properties

A property that names a state becomes a rule of its own, written beside
the rule it came from:

| Property | Emitted as |
| --- | --- |
| `hover-bg` | `:hover { background }` |
| `press-bg` | `:active { background }` |
| `hover-border` | `:hover { border }` |
| `focus-border` | `:focus { border }` |
| `focus-outline` | `:focus { outline }` |
| `selection-color` | `::selection { background }` |
| `selection-text-color` | `::selection { color }` |

### Lengths

A bare number is pixels in Lumen and nothing at all in CSS, so
`padding: 8 16` is emitted as `padding: 8px 16px`. A length that reaches
a property through `var()` is emitted as written, so give a custom
property holding a length an explicit unit: `--gap: 8px`, not `--gap: 8`.
Gradient stop positions travel as written; write them as percentages.

### Properties with no browser equivalent

The knobs that reach widget parts no selector can address are emitted as
custom properties, `--lm-` plus the property name: `knob-color` becomes
`--lm-knob-color`. The browser ignores them and Lumen reads them back off
the element, so they keep working, and you can read one yourself with
`var(--lm-knob-color)`.

This covers `knob-color`, `knob-inset`, `thumb-size`, `popup-gap`,
`caret-width`, `caret-blink`, `password-character`, `disabled-opacity`,
`progress-duration`, `progress-chunk`, `sensitivity`, `inertia`, and the
`scrollbar-*` geometry and timing properties. `scrollbar-color` and
`scrollbar-width` are standard CSS and are emitted as themselves; a
`scrollbar-color` naming only a thumb gains `transparent` for the track,
which CSS requires.

### What is not emitted

| Property | Why |
| --- | --- |
| `tab-index` | The document already carries it as `tabindex`. |
| `draggable` | The document already carries it as `draggable`. |
| `transition-delay` | Lumen ignores it: a transition starts on the tick the value changes. |
| `layout-boundary` | A hint to Lumen's layout engine; the browser lays the page out itself. |

Selector matching differs in one way worth knowing. Lumen's matcher takes
the nearest candidate for each step and does not reconsider, and a
browser's does; a selector that relies on that difference matches more
elements on the web than it does on the desktop.
