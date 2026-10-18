# `linear-gradient` angles paint the way CSS reads them

This affects desktop apps that use `linear-gradient` with an angle, or with no
angle. The desktop painted the angle turned a quarter: `90deg` ran top to
bottom and the default `180deg` ran right to left. It now paints what CSS
says and what a browser shows: `0deg` bottom to top, `90deg` left to right,
`180deg` top to bottom.

A gradient whose angle was chosen to look right on the old desktop now turns.
Write the angle CSS means:

```css
/* before: worked around the old desktop direction */
.panel { bg: linear-gradient(90deg, #202028, #101014); }
/* after: top to bottom, on the desktop and the web alike */
.panel { bg: linear-gradient(180deg, #202028, #101014); }
```
