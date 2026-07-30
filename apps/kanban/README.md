# Lanes - Kanban board

A three-column board (Backlog / In progress / Done) with add, edit, move,
delete, and live search. Built to show a real data flow, not a static
mock: one `cards` ArraySignal is the single source of truth, and the three
visible lists are rebuilt from it on every change.

## Run

```
cargo run -p lumenc -- run apps/kanban
```

## What it demonstrates

- **`<for>` over per-column views**: the board keeps a master `cards`
  array and derives `view_backlog` / `view_doing` / `view_done` from it,
  filtered by the search box.
- **Add / edit dialog**: `<dialog open="editor_open">` with `<input>` and
  two `<dropdown>`s (tag, column). One dialog serves both add and edit,
  keyed by an `editor_mode` signal.
- **Move between columns**: per-card icon buttons (left/right arrow PNGs
  from `icons/`) carry an interpolated id (`ml|{row.id}`); the global
  `on_click` parses the prefix and shifts the card's `col`.
- **`derive()` counts**: each lane header and the board summary bind to
  derived labels recomputed from the per-column count signals.
- **Live search**: `bind-text="search"` on the filter `<input>` mirrors
  every keystroke into the `search` signal on its own; a
  `derive("search_filter", ["search"], ...)` re-runs `refresh_board()`
  whenever that signal changes, so the board refilters reactively as you
  type. Lumen is reactive-only (there is no per-frame script hook), so
  the refilter rides `derive()`'s dependency tracking instead of polling.
- **Token-first styling**: the app retints the embedded `skin="default"`
  by overriding `--lumen-*` variables, then layers component classes for
  cards, tags, and the dialog.

## Design

Prussian-ink workspace, elevated slate lanes, a single warm amber accent
for primary actions, and status encoded by a colored dot per lane. Cards
carry a category tag pill (feature / bug / design / chore).
