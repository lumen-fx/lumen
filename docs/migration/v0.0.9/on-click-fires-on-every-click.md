# `on_click` fires on every click, a double-click included

This affects scripts that define both `on_click(id)` and `on_double_click(id)`
for the same element, in every script language. A quick pair of clicks used to
call `on_double_click` alone, with neither click reaching `on_click`. Now both
clicks reach `on_click` and `on_double_click` runs once after the second, the
order a browser fires `click` and `dblclick` in. A counter, a "next" button, or
a row delete driven by `on_click` no longer loses the second of two fast clicks.

A script that relied on the old behaviour to give one element a single-click
action and a different double-click action now runs the single-click action
twice before the double-click one. Make the double-click action undo or
supersede what the clicks did, or move the single action to a separate control.

A row that selects on a click and opens on a double-click keeps working as
written in candela:

```rust
fn on_click(id: string) { select(id); }
fn on_double_click(id: string) { open(id); }
```

`select` now runs for both clicks, which is harmless when selecting is
idempotent; `open` still runs once.
