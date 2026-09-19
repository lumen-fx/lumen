# Debug a Lumen layout issue

A Lumen app is showing a UI bug. Diagnose it with the snapshot-first
workflow. Do not open the screenshot before the text tools have narrowed the
problem; screenshots cost far more agent tokens than the text views.

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

4. `lumen_inspect_entity { id }` - full component dump for one entity. This
   one has no `lumenc` verb; call the MCP tool, or the `lumen.inspect_entity`
   JSON-RPC method on the app's port.

5. `lumenc element-at <x> <y>` - confirm what is at the screen
   coordinate, useful when the user reports "click here does X" but
   the press misses.

6. `lumenc screenshot debug.png --lint` - last resort, only when the
   text picture is not enough. Adds bright magenta outlines around every
   lint finding so spatial bugs jump out.

## Things to keep in mind

- The MCP snapshot is throttled to 1 Hz on a windowed app. If you need a
  fresher view immediately after a script update, wait one tick before
  the next call. A headless run with input simulation enabled snapshots every
  tick instead.
- Hot reload preserves state for `TextInput`, `TextContent`,
  `Toggleable`, `SliderValue`, `ScrollOffset` and signals. If a value
  unexpectedly resets, that is not hot reload; look for a script
  command that re-set it.
- `ChildOf` and `Children` are the source of truth for hierarchy; the
  snapshot mirrors them under `parent` and `children` on each entity.
