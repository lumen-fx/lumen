# `ease-in`, `ease-out` and `ease-in-out` follow the CSS curves on the desktop

This affects desktop apps whose transitions name `ease-in`, `ease-out` or
`ease-in-out`, and every built-in skin, which uses them.

The three named curves used to be cubic power curves on the desktop, steeper
than the ones a browser runs. They are now the cubic Beziers CSS defines, so
a transition moves the same on the desktop and on the web target. A transition
named with them starts and settles a little more gently than before. Colors
also blend the way CSS blends them, with premultiplied alpha: a fade from
`transparent` keeps its hue instead of passing through grey.

To keep the old feel, name the curve yourself:

| Before | Closest CSS curve |
| --- | --- |
| `ease-in` | `cubic-bezier(0.32, 0, 0.67, 0)` |
| `ease-out` | `cubic-bezier(0.33, 1, 0.68, 1)` |
| `ease-in-out` | `cubic-bezier(0.65, 0, 0.35, 1)` |
