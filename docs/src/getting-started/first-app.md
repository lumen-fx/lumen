# Your first app

This walkthrough builds a click counter end-to-end: scaffold it, run it,
read the generated markup / style / script, and watch hot-reload swap
state-preserved markup and script.

## Scaffold

```bash
cargo run -p lumenc -- new counter my-counter
cargo run -p lumenc -- run my-counter
```

`lumenc new counter` writes four files into `my-counter/`:

```text
my-counter/
├── main.lmn    # markup tree (required)
├── main.css    # styling (optional)
├── main.rhai   # script logic (optional)
└── lumen.toml  # per-app config (optional)
```

You should see a large `0` with two buttons below it — `+1` and
`reset`. Clicking `+1` increments the counter; the label re-renders
from a `clicks` signal. `reset` sets it back to zero.

## Walkthrough — `main.lmn`

```xml
<root bg="#0c1c30" padding="32" gap="20" align="center" justify="center">
  <label class="display" id="counter" width="100%" height="120px" text="0"
         bind-text="clicks" />
  <row gap="14" justify="center">
    <button class="primary" id="bump"  width="120px" height="48px" text="+1" />
    <button class="primary" id="reset" width="120px" height="48px" text="reset" />
  </row>
  <script src="main.rhai" />
</root>
```

- `<root>` is the single document root. Every `.lmn` file has exactly
  one. Lumen rejects unknown tags at parse time, so a typo like
  `<colum>` fails fast with a byte offset. Here it carries layout attrs
  directly: `padding`, `gap`, and centered `align` / `justify`.
- `<label class="display" bind-text="clicks">` is a text leaf. The
  `bind-text="clicks"` attribute pushes the value of the `clicks` signal
  into the label whenever it changes, so the initial `text="0"` is just
  the value shown before the script runs.
- `<row>` is a horizontal flex container — children sit left-to-right
  with a 14 px gap.
- `<button>` is a focusable tile with click dispatch wired in. Each has
  an `id` (`bump`, `reset`) so the script can route clicks to it.
- `<script src="main.rhai" />` pulls in the script file. It's captured
  at parse time, not rendered.

See the [tag reference](../authoring/tags.md) for every tag and its
attribute surface.

## Walkthrough — `main.rhai`

```rhai
fn on_start() {
    on("click", "bump",  "handle_bump");
    on("click", "reset", "handle_reset");
}

fn handle_bump(id) {
    let n = signal("clicks", 0);
    n.set(n.get() + 1);
}

fn handle_reset(id) {
    signal("clicks", 0).set(0);
}
```

- `on_start()` runs exactly once after the layout tree spawns. It's
  where you register handlers, seed signals, and call any one-shot
  Rhai builtin.
- `on("click", "bump", "handle_bump")` routes the click on entity
  `bump` to `handle_bump(id)` instead of the global `on_click(id)`
  fallback. This replaces the `if id == "bump" { … }` chain you'd
  otherwise write inside a single `on_click`.
- `signal("clicks", 0)` returns a handle into the host-local reactive
  store, auto-initialising the entry to `0` if no other call has
  touched it. The handle has `.get()` / `.set(v)` methods; you can
  also use `.value` (Vue-style accessor sugar).
- Because the `<label>` carries `bind-text="clicks"`, every `.set` on
  the `clicks` signal re-renders the label automatically — no explicit
  redraw call.

The full builtin surface is in [Rhai builtins](../authoring/rhai-builtins.md).

## Walkthrough — `main.css`

The scaffold ships styling too:

```css
:root {
  --color-accent:  #5fd9e0;
  --color-bg:      #163459;
  --color-hover:   #1d4477;
  --color-active:  #0e2c52;
  --color-on-bg:   #ffffff;
  --radius-pill:   24;
}

.display { text-align: center; font-size: 96; text-color: var(--color-on-bg); }

.primary {
  bg:        var(--color-bg);
  hover-bg:  var(--color-hover);
  press-bg:  var(--color-active);
  text-color: var(--color-on-bg);
  radius:    var(--radius-pill);
  text-align: center;
  font-size: 18;
}
.primary:focus { outline: 2 var(--color-accent); }
```

- `:root { --foo: x; }` declares CSS custom properties — Lumen resolves
  `var(--foo)` against the nearest `:root` declaration.
- `.display` and `.primary` are class selectors matched against the
  `class` attribute in the markup. Inline markup attributes still win
  over CSS, so anything you wrote inline stays put.
- `.primary:focus` styles the focused state — the `:focus` pseudo-class
  routes to the focus outline.

Save the file while the app is running and it picks up the change on
save — no restart, no flicker. See the [CSS subset](../authoring/css.md)
for the property list.

## Hot reload demo

With the app still running:

1. Open `main.lmn` and change one button's text from `+1` to `add`.
   Save. The button label flips immediately. The `clicks` value (the
   running counter) is preserved because Lumen snapshots stateful
   components by `LumenId` and restores them after re-spawn.

2. Open `main.rhai` and change how much `+1` adds:

   ```rhai
   fn handle_bump(id) {
       let n = signal("clicks", 0);
       n.set(n.get() + 5);   // was + 1
   }
   ```

   Save. The button now adds 5 per click. Note that
   `signal("clicks", 0)` after the reload still resolves to the same
   live value — `RhaiHost::replace_ast` keeps the persistent `Scope`
   and signal map across reloads, so the running count doesn't blink
   back to zero on every save.

3. Open `main.css` and tweak `--color-accent: #5fd9e0;` to `#f7768e`.
   Save. The focus outline repaints the new color without losing focus
   or hover state.

These are the three hot-reload paths:

| File | Behaviour on save |
|---|---|
| `main.lmn` | Re-parse + re-spawn, preserving stateful components by `LumenId`. |
| `main.css` | Re-apply styling; a class-invalidation set fast-rejects no-op class flips. |
| `main.rhai` | `RhaiHost::replace_ast` swaps the AST and keeps the live `Scope`, so signals, toggles, sliders, and scroll positions survive. |

The runner checks the modification time of `main.lmn`, `main.css`, and
any referenced `.rhai` files each tick; when a timestamp changes it
reloads only the affected path. `lumen.toml` is read once at startup —
restart `lumenc run` to pick up config changes.

## What about signals, exactly?

A *signal* is a named entry in the host-local reactive store. Three
operations matter:

- `.get()` — read the current string value (or `()` if unset).
- `.set(v)` — write a new value; tags the signal name in the
  per-tick dirty set.
- `derive(name, deps, fn)` — register a closure that re-runs whenever
  any dep is in the dirty set. The return value is stringified into
  signal `name`.

The runtime keeps two indices on top: `<label bind-text="name">` pushes
text content from the signal to the label, and `<input bind-text="name">`
or `<toggle bind-checked="name">` or `<slider bind-value="name">`
pushes the user's edits back into the signal. This is the closed loop
that lets you write declarative markup without a custom view-model.

Signals are strings on the wire — that keeps the Rhai surface tiny and
lets the same primitive drive text content, checkbox state, and
slider values without per-type plumbing. The CSS layer reads them too
(`<if signal="foo" mode="hide">` for conditional subtrees).

## Next

- [Project layout](./project-layout.md) — what files belong where.
- [Markup tag reference](../authoring/tags.md) — every shipped tag.
- [Rhai builtins](../authoring/rhai-builtins.md) — every shipped fn.
- The full [widget-garden](https://github.com/lumen-ui/lumen/tree/main/apps/widget-garden)
  app uses everything in one file.
