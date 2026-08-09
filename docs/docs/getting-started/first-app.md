# Your first app

This walkthrough builds a click counter end to end: scaffold it, run it, read
the four files it generated, change one, and watch hot reload swap it in with
the count intact. Every command is `lumenc`. No Rust compiler is involved at
any point.

## Scaffold it

```bash
lumenc new counter my-counter
```

```
created my-counter/ from template 'counter'.
run it with: lumenc run my-counter
```

The template writes four source files and a README:

```text
my-counter/
|-- main.lmn    # the markup tree
|-- main.css    # styling
|-- main.cdl    # the candela script
|-- lumen.toml  # per-app config
`-- README.md   # what the template demonstrates
```

## Run it

```bash
lumenc run my-counter
```

A 480x360 window opens on a dark blue field: a large `0` with two buttons
under it, `+1` and `reset`. Click `+1` and the number goes up; click `reset`
and it goes back to zero. Leave the window open, because the rest of this page
edits the files underneath it.

## `main.lmn` - the markup

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

`<root>` is the single document root. Every `.lmn` file has exactly one, and
it defaults to a vertical flex container filling the window. Here it carries
layout attributes directly: `padding`, `gap`, and centered `align` / `justify`.

`<label>` is a text leaf. Its `bind-text="clicks"` pushes the value of the
`clicks` signal into the label whenever that signal changes, so the `text="0"`
is only what shows before the script runs. `<row>` lays its children out
left to right. `<button>` is a focusable tile with click dispatch wired in;
each one carries an `id` so the script can name it.

`<script src="main.cdl" />` attaches the script. It is captured at parse time
and never rendered. Nothing points at `main.css`: Lumen loads the `main.css`
sitting next to the entry file on its own.

Unknown tags fail at parse time with a byte offset, so a typo like `<colum>`
stops the run instead of silently rendering nothing. The
[tag reference](../authoring/tags.md) lists every tag and its attributes.

## `main.cdl` - the script

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

`import "lumen.cdl";` is the whole setup. It declares the Lumen host surface
in one line: signals, events, timers, the dynamic DOM, dialogs, and the OS
integrations. `main()` is candela's program entry point; a Lumen app leaves it
empty and works from the lifecycle handlers instead.

`on_start()` runs once at app construction, before the first tick. Seed
signals and register event routes here.

`lumen::on(event, id, handler)` routes one event on one element to one
function. The handler receives the id of the element that fired, so one
function can serve several ids. A click on an element with no route falls
through to a global `on_click(id)` instead, which is what you reach for when
ids are generated rather than known up front.

`lumen::signal_get_int` and `lumen::signal_set_int` read and write a named
entry in the reactive store. The `<label>` carries `bind-text="clicks"`, so
every write re-renders it. There is no redraw call.

A signal is the state that survives between handler calls: every handler is a
fresh call into the program, and locals do not outlive it.
[Scripting](../authoring/scripting.md) covers signals, derived signals, and
the rest of the host surface; the
[candela reference](../reference/scripting-candela.md) lists every function.
The candela language itself, its syntax, types, and standard library, is
documented at <https://candela.lumenfx.dev/>.

## `main.css` - the styling

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

`:root { --name: value; }` declares custom properties, and `var(--name)`
reads them back. Keeping every color in one block is what makes a theme swap
a one-block edit.

`.display` and `.primary` match the `class` attributes in the markup.
`.primary:focus` styles the focused state. Inline markup attributes win over
CSS, so the `bg="#0c1c30"` written on `<root>` stays put no matter what a
stylesheet says; move a value into a class when you want CSS to own it. The
[CSS subset](../authoring/css.md) has the full property list.

## `lumen.toml` - the config

```toml
[app]
entry = "main.lmn"

[window]
title = "Counter"
size = [480, 360]
```

Every key is optional. This one names the entry file (which is already the
default) and sets the window title and size. There is no `[script]` block:
Lumen sees `main.cdl` in the directory and runs the app on candela. See
[Per-app config](../authoring/lumen-toml.md) for the whole surface and
[Choosing a host](../authoring/scripting.md#choosing-a-host) for the selection
rule.

## Change it

With the app still running, add one line to `handle_bump` so the pressed
button picks up a `hot` class:

```candela
fn handle_bump(id) {
    let n = lumen::signal_get_int("clicks");
    lumen::signal_set_int("clicks", n + 1);
    get_by_id(id).class_add("hot");
}
```

`get_by_id(id)` returns a `Node` wrapping one live element, and `.class_add`
edits its class list. That is the dynamic DOM API: `spawn("label")` mints an
element, `parent.append(child)` places it, and `.set_text(...)` /
`.set_style(...)` edit it. A lookup that misses returns a `Node` with handle
`0` rather than raising, so call `.exists()` when you are not sure the element
is there.

Give the class something to do, in `main.css`:

```css
.hot { bg: var(--color-accent); }
```

## Hot reload

Saving any of the three source files reloads that file alone, without
restarting the app:

| File | What happens on save |
|---|---|
| `main.lmn` | Re-parse and re-spawn, preserving stateful components by `LumenId`. Text cursors, toggles, sliders, and scroll positions survive. |
| `main.css` | Re-apply styling. A class-invalidation set fast-rejects a class flip no rule cares about. |
| `main.cdl` | Compile the new source, then swap it in. Signals survive, so the running count does not blink back to zero. A source that fails to compile leaves the running script untouched and logs the error. |

Try the first two:

1. Change one button's text from `+1` to `add` in `main.lmn`. Save. The label
   flips and the count keeps its value.
2. Change `--color-accent` from `#5fd9e0` to `#f7768e` in `main.css`. Save.
   The focus outline repaints without losing focus or hover state.

The watcher covers the entry markup, its stylesheet, every script the markup
references, and every included or imported file. `lumen.toml` is read once at
startup; restart `lumenc run` to pick up a config change.

Editing `main.cdl` has a catch worth knowing before you try it. A script
reload clears the per-id routes `lumen::on(event, id, handler)` registered,
and `on_start` only runs at app construction, so nothing re-registers them.
The new code is live, but the counter's buttons go quiet until you restart
`lumenc run`. Change `n + 1` to `n + 5`, save, restart, and the next click
adds five.

A script that dispatches through the global `on_click(id)` instead of
per-id routes keeps working across reloads, because that handler is
resolved by name on every click.

## Next

- [Project layout](./project-layout.md) - what files belong where.
- [Template gallery](./templates.md) - the other six scaffolds.
- [Markup tags](../authoring/tags.md) - every shipped tag.
- [Scripting](../authoring/scripting.md) - signals, events, and the DOM API.
- [`apps/widget-garden`](https://github.com/lumen-fx/lumen/tree/main/apps/widget-garden)
  exercises every tag and attribute in one file.
