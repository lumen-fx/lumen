# Debug a Lumen layout issue

A Lumen app is showing a UI bug. Diagnose it with the snapshot-first
workflow. Do **not** open the screenshot before the text tools have
narrowed the problem - screenshots burn 10-30x more agent tokens.

## Recommended order

1. `lumenc lint` - surfaces zero-size visibles, text without style,
   focusable nodes without labels, gradient mistakes, dropped clicks,
   and child entities overflowing their parent. Most layout bugs are
   already flagged here.

2. `lumenc snapshot --text | head -50` - orients you in the tree.
   Lines are `id role label x,y wxh state`, sorted by hierarchy then
   absolute position.

3. `lumenc find --text "<label>"` - locate the suspect element by its
   visible text (or `--role` / `--id`).

4. `lumenc inspect <id>` - full component dump for one entity.
   Defaults to a summary; pass `--fields` for deep nested data.

5. `lumenc element-at <x> <y>` - confirm what's actually at the screen
   coordinate, useful when the user reports "click here does X" but
   the press misses.

6. `lumenc screenshot debug.png --lint` - last resort, only when the
   text picture isn't enough. Adds neon-magenta outlines around every
   lint finding so spatial bugs jump out.

## Things to keep in mind

- The MCP snapshot is throttled to ~1 Hz by default. If you need a
  fresher view immediately after a script update, wait one tick before
  the next call.
- Hot reload preserves state for `TextInput`, `TextContent`,
  `Toggleable`, `SliderValue`, `ScrollOffset` and signals. If a value
  unexpectedly resets, that's not hot reload - look for a script
  command that re-set it.
- `ChildOf` + `Children` are the source of truth for hierarchy; the
  snapshot mirrors them under `parent` / `children` on each entity.
