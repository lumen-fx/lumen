# Styling

Lumen styles apps with a CSS subset: familiar selectors, a cascade ordered by
specificity, custom properties, and `@media`. This page covers the model. The complete list of
selectors, properties, and value forms is in [CSS](../reference/css.md).

## Where the stylesheet comes from

Put a `main.css` next to `main.lmn` and it is picked up automatically. Nothing
in the markup references it.

Split it across files with `@import`:

```css
@import "tokens.css";
@import "widgets.css";

.card { bg: var(--surface); radius: var(--radius); }
```

`@import` lines go at the top of the file, before any rule. Paths resolve
against the importing file, imports may nest, and an import cycle is reported
with the whole chain. Imported rules are spliced in ahead of the importing
file's own rules, so at equal specificity the file doing the importing wins.
Every imported file is watched, so editing one reloads the running app.

## Selectors

A selector is a chain of compounds joined by combinators. A compound is an
optional tag name followed by any number of `.class`, `#id`, and `:pseudo`
parts:

```css
button { padding: 0 12; }
.card .title { font-size: 18; font-weight: 600; }
.list > .row:nth-child(odd) { bg: var(--surface-2); }
#save:disabled { opacity: 0.5; }
```

Combinators are descendant (whitespace), child (`>`), adjacent sibling (`+`),
and general sibling (`~`). `*` matches any tag. A sibling combinator looks at
elements before it in the same parent, so `.row + .row { margin: 8 0 0 0 }`
spaces a list without touching its first item.

Available pseudo-classes: `:hover`, `:focus`, `:focus-visible`, `:active`,
`:disabled`, `:checked`, `:selected`, `:drag-over`, `:root`, `:first-child`,
`:last-child`, `:only-child`, `:empty`, `:nth-child(an+b)` (including `odd`
and `even`), `:is()`, `:where()`, and `:not()`.

## The cascade

Rules resolve the way CSS Cascade 5 says they do: origin first, then
importance, then specificity, then source order, with the later rule winning a
tie. `:where()` contributes no specificity; `!important` lifts a declaration
above the normal declarations of its origin.

Four layers stack, lowest first:

1. The built-in colour palette, exposed as custom properties.
2. The always-on baseline that gives controls their minimum sizes.
3. The skin, when the app opts into one.
4. Your own CSS.

The first three share the user-agent origin, and your CSS is the author origin.
A normal author declaration therefore beats a normal built-in declaration
whatever their specificities are, so you never need `!important` to override a
skin.

Above all of that sit inline markup attributes. `<tile width="50px" />` wins
over `.tile { width: 100px }`. That is deliberate: an attribute is a statement
about one element, and it stays true no matter which stylesheet loads later. It
holds for every property both surfaces can write; the layout tags' own
direction is the exception, and CSS can change that with `flex-direction`.

## Custom properties and theming

Declare custom properties on `:root` and read them with `var()`:

```css
:root {
  --bg: #0c0d10;
  --surface: #161922;
  --text: #e6e9ef;
  --radius: 10;
}

.app { bg: var(--bg); text-color: var(--text); }
.card { bg: var(--surface); radius: var(--radius); }
```

`var(--name, fallback)` supplies a value when the property is undefined.
Properties may reference other properties, and a chain that loops is reported
rather than followed.

Markup attributes read them too, so a token stays a token even where you set a
value on one element: `<tile bg="var(--surface)" />`. Only `:root` properties
resolve there; a property declared under some other selector reaches an element
through the stylesheet, not through an attribute.

Properties also cascade from an ancestor, which is what makes theming a single
class flip. Declare a second scope on the root and every descendant reading
`var(--bg)` picks up the new value:

```css
:root { --bg: #0c0d10; --text: #e6e9ef; }
.theme-light { --bg: #f8f9fb; --text: #1a1d23; }
```

Lumen keeps `theme-dark` or `theme-light` on the root element in sync with the
effective color scheme, so the pair above follows the OS with no script. A
script can override the choice with `set_color_scheme`, which accepts
`"default"`, `"force-light"`, `"force-dark"`, `"prefer-light"`, and
`"prefer-dark"`.

## Flipping classes from a script

Styling reacts to class changes. A script can set the root's class list, set
the class list of an element by id, or add, remove, and toggle a class on a
node it holds. Any of those re-resolves the affected elements against the
stylesheet, including the custom-property scopes they inherit:

```rust
// candela
import "lumen.cdl";

fn on_click(id) {
    lumen::set_root_class("theme-light");
}
```

Toggling a class is the idiomatic way to express selection, validity, expanded
state, and anything else you would otherwise drive by rewriting inline styles.
See [Scripting](scripting.md).

## Skins

Four skins ship with Lumen. A skin dresses the built-in controls: fills, radii,
borders, focus rings, and accent behaviour. `macos`, `windows`, and `linux`
follow the conventions of the desktop they are named after; `default` is a
neutral look that belongs to no platform. `auto` resolves to the skin matching
the machine the app is running on.

Opt in from markup:

```html
<root skin="auto">
```

or from `lumen.toml`:

```toml
[skin]
name = "macos"
```

Markup wins when both are present, which lets one app preview any platform's
look without editing its config. With neither, no skin applies and your CSS
starts from the baseline.

Override a skin by writing ordinary rules in `main.css`. Because the skin is a
user-agent-origin sheet, your rule wins without `!important`. Restyling a
skin's tokens is usually enough:

```css
:root { --lumen-accent: #7aa2f7; }
```

## Responding to the environment

`@media` gates a block of rules on the environment:

```css
@media (prefers-color-scheme: dark) {
  .card { bg: #161922; }
}

@media (max-width: 760px) {
  .sidebar { width: 0; }
}
```

Supported features are `prefers-color-scheme` (`dark`, `light`,
`no-preference`), `prefers-reduced-motion` (`reduce`, `no-preference`),
`prefers-contrast` (`more`, `less`, `custom`, `no-preference`), and the
viewport widths `min-width`, `max-width`, and `width`. Combine them with `and`.
Blocks may nest.

The color scheme and the viewport are live. The motion and contrast
preferences are not read from the OS yet, so those two features only match
`no-preference`; offer an in-app switch instead.

When the OS theme changes or the window crosses a width you wrote a rule for,
the affected elements re-resolve.

## What is deliberately not here

- **Pseudo-elements.** `::before` and `::after` are a parse error. Add an
  element instead; it is cheaper to reason about and a script can reach it.
- **Attribute selectors.** Select on a class.
- **Keyframe animations.** An `@keyframes` block is skipped with a warning, as
  is any other at-rule beyond `@import` and `@media`. Lumen animates through
  CSS transitions, started by a state change or by a script flipping a class;
  see [Animations](animations.md).
- **Inline `!important`.** Importance is authorable in a stylesheet, not in a
  markup attribute, because an attribute already outranks the stylesheet.

## Where to look things up

- Every selector, property, and value form: [CSS](../reference/css.md)
- The attribute spelling of the same properties:
  [Tags and attributes](../reference/tags.md)
- `[skin]` and the rest of the config: [lumen.toml](../reference/lumen-toml.md)
- Transitions: [Animations](animations.md)
