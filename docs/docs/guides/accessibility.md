# Accessibility

Lumen publishes an accessibility tree to the operating system, so screen
readers and other assistive tools can see your app. It is on in every app, on
Linux, macOS, and Windows, with nothing to configure.

Most of what you get is automatic. What you control is the accessible name of
each control, the keyboard order, and the focus ring.

## Names come from your text

A control's accessible name is its `text` attribute:

```html
<button id="save" text="Save"/>
```

That button announces as "Save". If a control has no `text`, its `id` is used
instead, which is rarely what you want a person to hear. An input's
`placeholder` is published separately from the name.

There is no `role` attribute and no `aria-*` attributes today. Roles are
inferred from what a widget is: a text input, a slider, a checkbox, a switch, a
scrollable region, a focusable control, or a piece of text. Unrecognised
attributes are dropped without a warning, so writing `aria-label` has no
effect.

The practical consequence: a control that shows only an image has no accessible
name. Give it a `text` as well, or pair it with a `label` that says what it
does.

## Keyboard navigation

Tab moves focus forward, Shift+Tab back, and focus wraps at the ends. Order is
by `tab-index` first, then by the order elements appear in your markup:

```html
<input id="city" tab-index="0" placeholder="City"/>
<button id="search" tab-index="1" text="Search"/>
```

These are focusable without you writing anything: `input`, `textarea`,
`button`, `toggle`, `switch`, `slider`, `checkbox`, the buttons of a `tabs`
strip, and a `dropdown`. A radio group is one Tab stop; arrow keys move between
its options. Anything else, including `tile` and container elements, becomes
focusable when you give it a `tab-index`.

Disabled elements and hidden subtrees are skipped. While a `dialog` is open,
Tab stays inside it.

Activation follows the usual desktop rules:

- Enter activates the focused control immediately.
- Space activates on release, so you can press and move away to cancel.
- Enter in a single-line input commits its value; in a multiline input,
  Shift+Enter commits and Enter inserts a newline.
- Sliders use the arrow keys rather than Enter or Space.
- Escape cancels a press in progress, and closes an open dropdown or dialog.

Arrow keys work inside widgets that own a selection: left and right along a
tabs strip (stopping at the ends), around a radio group (wrapping), through a
dropdown's options, and over the content of a scrollable region, where Page
Up/Page Down move a screenful and Home/End jump to the ends. A slider steps by
its `step`, or by a hundredth of its range if you set none, with Page Up/Page
Down for larger jumps and Home/End for the extremes.

Tabbing into a text input selects all of its text, so typing replaces it.

## The focus ring

Lumen distinguishes focus from keyboard focus. Style both with pseudo-classes:

```css
button:focus-visible { outline: 2 var(--lumen-accent); outline-offset: 2; }
input:focus          { outline: 2 var(--lumen-accent); }
```

`:focus-visible` matches only when focus arrived from the keyboard or from an
assistive tool, which is what you want for buttons; clicking one should not
leave a ring behind. `:focus` matches either way, which suits text fields.
Outlines are drawn outside the border box and never affect layout.

The built-in skins already ship focus rules for every control. If you style
your app from scratch without a skin, write your own; without them, a keyboard
user cannot see where they are. See [styling](styling.md).

## System preferences

`@media (prefers-color-scheme: dark)` follows the desktop's light or dark
setting and updates live when it changes.

`prefers-reduced-motion` and `prefers-contrast` are accepted by the stylesheet
parser but never match, so rules inside them do not apply. Until they do, offer
your own setting for anything heavily animated. See
[animations](animations.md).

## Limits today

- No `role` or `aria-*` attributes, and no way to announce a message
  programmatically.
- `disabled` is respected by focus and input, and is not yet published to the
  accessibility tree.
- Composite widgets such as tabs, radio groups, and dropdowns navigate
  correctly with the keyboard, and report as generic controls rather than as
  tab lists, radio groups, and combo boxes.
