# Your first app

This walkthrough builds a click counter end-to-end: scaffold it, read the
candela script it ships, run it, and watch hot-reload swap state-preserved
markup and script. Every command is `lumenc`; there is no Rust compiler
involved at any point.

## Scaffold

```bash
lumenc new counter my-counter
```

`lumenc new counter` writes into `my-counter/`:

```text
my-counter/
|-- main.lmn    # markup tree (required)
|-- main.css    # styling (optional)
|-- main.cdl    # script logic (optional)
|-- lumen.toml  # per-app config (optional)
`-- README.md   # what the template demonstrates
```

Once the app runs you see a large `0` with two buttons under it, `+1` and
`reset`. Clicking `+1` increments the counter; the label re-renders from a
`clicks` signal. `reset` sets it back to zero.

## Walkthrough - `main.lmn`

```xml
<root bg="#0c1c30" padding="32" gap="20" align="center" justify="center">
  <label class="display" id="counter" width="100%" height="120px" text="0"
         bind-text="clicks" />
  <row gap="14" justify="center">
    <button class="primary" id="bump"  width="120px" height="48px" text="+1" />
    <button class="primary" id="reset" width="120px" height="48px" text="reset" />
  </row>
  <script src="main.cdl" />
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
- `<row>` is a horizontal flex container; children sit left-to-right
  with a 14 px gap.
- `<button>` is a focusable tile with click dispatch wired in. Each has
  an `id` (`bump`, `reset`) so the script can find it in the tree.
- `<script src="main.cdl" />` pulls in the script file. It's captured
  at parse time, not rendered.

See the [tag reference](../authoring/tags.md) for every tag and its
attribute surface.

## Walkthrough - `main.cdl`

```candela
import "lumen.cdl";

fn on_start() {
    lumen::signal_set_int("clicks", 0);
    lumen::on("click", "bump", "handle_bump");
    lumen::on("click", "reset", "handle_reset");
}

fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
}

fn handle_reset(id) {
    lumen::signal_set_int("clicks", 0);
}

fn main() {}
```

- `import "lumen.cdl";` is the whole setup. It pulls in the Lumen host
  surface: signals, events, the dynamic DOM, and the rest. `main()` is
  the program entry point; a Lumen app leaves it empty and does its work
  in the lifecycle handlers.
- `on_start()` runs once at app construction, before the first tick. Seed
  signals and register event routes here. Its sibling `on_ready()` runs
  on the first tick, once the element tree is mounted, and is where you
  look elements up.
- `lumen::on(event, id, handler)` routes one event on one element to one
  function. Clicks on any element with no route fall through to a global
  `on_click(id)` instead, which is the pattern to reach for when ids are
  generated rather than known up front.
- A routed handler takes the id of the element that fired, so the same
  function can serve several ids.
- `lumen::signal_get_int` / `lumen::signal_set_int` read and write a
  named entry in the reactive store. Because the `<label>` carries
  `bind-text="clicks"`, every write re-renders the label; there is no
  explicit redraw call.

Events are only half the story. The same script can build and change the
tree through the DOM API: `get_by_id("bump")` returns a `Node` wrapping
one element, `spawn("label")` mints a new one, `parent.append(child)`
puts it in place, and `node.set_text(...)` / `node.class_add(...)` /
`node.set_style(...)` edit it. A lookup that misses returns a `Node` with
handle `0` rather than raising, so check `.exists()` when you are not
sure an element is there. To tag the pressed button so a `.hot` rule in
`main.css` can style it, add a line to `handle_bump`:

```candela
fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
    get_by_id(id).class_add("hot");
}
```

The full builtin surface is in [Scripting](../authoring/scripting.md).
The candela language itself, its syntax, types, and standard library, is
documented at <https://candela.lumenfx.dev/>.

## How Lumen picks the script host

`lumenc` reads the app's `[script] engine` in `lumen.toml` when it is set;
otherwise it infers the host from the script file extensions in the app
directory. A `.cdl` file selects candela outright, even with a `.rhai` or
`.lua` file sitting next to it, and a directory with no script at all
defaults to candela too. The counter scaffold ships `main.cdl` and no
`[script]` block, so it runs on candela with nothing to configure.

Set `[script] engine` when you want to pin the host regardless of which
files are present:

```toml
[script]
engine = "candela"   # "candela" (default) | "rhai" | "lua"
```

## Walkthrough - `main.css`

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

- `:root { --foo: x; }` declares CSS custom properties. Lumen resolves
  `var(--foo)` against the nearest `:root` declaration.
- `.display` and `.primary` are class selectors matched against the
  `class` attribute in the markup. Inline markup attributes still win
  over CSS, so anything you wrote inline stays put.
- `.primary:focus` styles the focused state; the `:focus` pseudo-class
  routes to the focus outline.

Save the file while the app is running and it picks up the change on
save, with no restart and no flicker. See the [CSS subset](../authoring/css.md)
for the property list.

## Run it

```bash
lumenc run my-counter
```

## Hot reload demo

With the app still running:

1. Open `main.lmn` and change one button's text from `+1` to `add`.
   Save. The button label flips immediately. The `clicks` value (the
   running counter) is preserved because Lumen snapshots stateful
   components by `LumenId` and restores them after re-spawn.

2. Open `main.cdl` and change how much `+1` adds:

   ```candela
   fn handle_bump(id) {
       let n = lumen::signal_get_int("clicks");
       lumen::signal_set_int("clicks", n + 5);   // was n + 1
   }
   ```

   Save. The reload compiles the new source first, then swaps it in with
   the signal store intact, so the running count does not blink back to
   zero. A source file that fails to compile leaves the running script
   untouched and logs the error.

3. Open `main.css` and tweak `--color-accent: #5fd9e0;` to `#f7768e`.
   Save. The focus outline repaints the new color without losing focus
   or hover state.

These are the three hot-reload paths:

| File | Behaviour on save |
|---|---|
| `main.lmn` | Re-parse + re-spawn, preserving stateful components by `LumenId`. |
| `main.css` | Re-apply styling; a class-invalidation set fast-rejects no-op class flips. |
| `main.cdl` | Compile the new source, then swap it in. Signals, toggles, sliders, and scroll positions survive. |

A file-system watcher covers `main.lmn`, `main.css`, and every script and
included file, and reloads only the path that changed. `lumen.toml` is read
once at startup; restart `lumenc run` to pick up config changes.

One thing does not survive a reload: every save re-loads the script, and
that clears the per-id routes `lumen::on(event, id, handler)` registered.
`on_start` runs at app construction and not again, so nothing re-registers
them and the counter's buttons go quiet until you restart `lumenc run`. A
script that dispatches through the global `on_click(id)` keeps working
across reloads, since that handler is resolved by name on every click.

## What about signals, exactly?

A *signal* is a named entry in the reactive store. Three operations
matter:

- `lumen::signal_get(name)` - read the current value. Typed readers
  (`signal_get_int`, `signal_get_float`, `signal_get_bool`) return the
  scalar directly.
- `lumen::signal_set(name, value)` - write a new value and mark the name
  dirty for this tick. Typed writers mirror the readers.
- `lumen::derive(name, deps, fn_name)` - register a function that re-runs
  whenever any dep is dirty. Its return value lands in signal `name`.
  candela has no closure value, so the recompute body is named by a
  string, not passed inline.

The runtime keeps two indices on top: `<label bind-text="name">` pushes
text content from the signal to the label, and `<input bind-text="name">`
or `<toggle bind-checked="name">` or `<slider bind-value="name">`
pushes the user's edits back into the signal. This is the closed loop
that lets you write declarative markup without a custom view-model.

Signals carry strings and the scalar types, and stringify on the way into
a `bind-text` label, so the same primitive drives text content, checkbox
state, and slider values without per-type plumbing. The markup layer reads
them too (`<if signal="foo" mode="hide">` for conditional subtrees).

## Next

- [Project layout](./project-layout.md) - what files belong where.
- [Markup tag reference](../authoring/tags.md) - every shipped tag.
- [Scripting](../authoring/scripting.md) - every shipped builtin.
- The full [widget-garden](https://github.com/lumen-fx/lumen/tree/main/apps/widget-garden)
  app uses everything in one file.
