# Streak - Habit tracker

A weekly habit grid: four habits across seven days, each cell a toggle. The
header keeps live stats, and a settings dialog holds the weekly goal and a
clear-week action.

## Run

```
cargo run -p lumenc -- run apps/tracker
```

## What it demonstrates

- **CSS grid week** - the grid is a real `display: grid` with
  `grid-template-columns: 150px 1fr 1fr 1fr 1fr 1fr 1fr 1fr`; the header
  cells and 28 habit toggles auto-place into it.
- **`:checked` cell styling** - each cell is a `<toggle bind-checked=...>`;
  the `.cell:checked` rule paints the lime "done" fill.
- **`derive()` streak stats** - `on_toggle` recomputes total checks, the
  longest run of perfect days, and completion %; derived labels format them
  for the header.
- **Settings dialog** - `<dialog open="settings_open">` with a goal
  `<slider>` and a clear-week button; `on_slider` recomputes goal progress.
- **`@media` width query** - below 760px the shell tightens its padding and
  shrinks the stat tiles and grid gutter.
- **Global hotkey** - `register_hotkey("open-settings", "CommandOrControl+,")`
  opens the settings dialog; `on_hotkey` handles it.

## Design

Calm graphite with a single vivid lime reserved for "done". Stats sit in
quiet tiles; the perfect-day streak is the one lime number.

## Note

The streak/stat labels bind to computed signals. In the current build the
reactive `bind-text` render path is being reworked upstream; the grid,
toggles, `:checked` styling, `@media` query, and stat logic are complete.
