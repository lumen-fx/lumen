# Your first app

This walks through the `counter` template: a number, two buttons, and a script
that ties them together. It is the shortest complete example of how a Lumen app
is put together, and every idea in it scales up unchanged.

## Create and run it

```sh
lumenc new my-app counter
lumenc run my-app
```

A window opens with a large `0` and two buttons. Click `+1` and the number
goes up; click `reset` and it goes back to zero.

Four files were written into `my-app/`: the markup, a stylesheet, a script, and
a small config file. Take them one at a time.

## The markup

`main.lmn` is the element tree. It is the only required file in an app.

```html
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

Reading it top to bottom:

- `<root>` is the window's content. Every app has exactly one, and layout
  attributes on it apply to its children: `padding` insets them, `gap` spaces
  them out, `align` and `justify` centre them. By default children stack
  vertically.
- `<label>` draws text. `text="0"` is what it shows before anything runs, and
  `bind-text="clicks"` says the label follows a value named `clicks`: whenever
  `clicks` changes, the label redraws. Nothing sets its text by hand.
- `<row>` lays its children out horizontally. Its vertical counterpart is
  `<column>`.
- `<button>` is clickable. `id` is the name the script uses to talk about a
  specific element.
- `<script src="main.cdl" />` attaches the app's script.

`class` picks up styling, and `width` / `height` size an element. `100%` is a
share of the parent; `120px` is a fixed logical pixel size.

Full tag and attribute list: [Tags reference](../reference/tags.md). The
[markup guide](../guides/markup.md) covers the tree in depth.

## The styling

`main.css` sits next to `main.lmn` and is picked up automatically. You never
link it from the markup.

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

Selectors, the cascade, and pseudo-classes work the way they do on the web.
`:root` holds custom properties, so changing the palette is one block of edits
rather than a search through the file. `hover-bg` and `press-bg` give a button
its hover and pressed backgrounds without writing a `:hover` rule for each.

Every property and value form: [CSS reference](../reference/css.md). Theming and
skins: [styling guide](../guides/styling.md).

## The script

`main.cdl` is written in candela, Lumen's default scripting language.

```rust
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

`import "lumen.cdl";` brings in the Lumen host surface, which is where
`lumen::signal_set_int` and the rest come from. `main()` stays empty: a Lumen
app does its work in lifecycle handlers, not in a top-level program.

`on_start()` runs once after the first layout. It does two things:

- Creates the `clicks` signal with the value `0`. A signal is a named value the
  UI can follow; the label's `bind-text="clicks"` is what makes the connection.
- Routes clicks. `lumen::on("click", "bump", "handle_bump")` sends clicks on
  the element with `id="bump"` straight to `handle_bump`.

`handle_bump` reads the signal, adds one, and writes it back. That write is the
whole update: the label is already following `clicks`, so it redraws itself.
There is no code that touches the label.

The signal and binding model: [reactivity guide](../guides/reactivity.md). What
scripts can call: [scripting guide](../guides/scripting.md) and the
[candela reference](../reference/scripting-candela.md).

!!! note "Other languages"
    Rhai and Lua work the same way. Name the file `main.rhai` or `main.lua` and
    Lumen picks the matching host from the extension.

## The config

`lumen.toml` describes everything static about the app.

```toml
[app]
entry = "main.lmn"

[window]
title = "Counter"
size = [480, 360]
```

`entry` names the markup file to start from, and `[window]` sets the title and
the starting size. Every key: [lumen.toml reference](../reference/lumen-toml.md).

## Change something

Leave the app running and edit a file. Save `main.css` with a different
`--color-accent`, or add a button to `main.lmn`, and the window updates without
a restart.

Two more commands are worth knowing early:

```sh
lumenc check my-app
lumenc run my-app --headless --ticks 5
```

`check` parses the app and reports errors without opening a window, which is
what you run in CI. `--headless` runs the whole app, layout and rendering
included, with no window at all; see the [testing guide](../guides/testing.md).

## Next

- [What each file in an app directory is](project-layout.md).
- [The other templates](templates.md), including list rendering, forms, and
  native shell integration.
- [Package an app for other people](../guides/packaging.md).
