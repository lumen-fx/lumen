# Templates

`lumenc new` scaffolds an app directory from a small gallery of starters. Each
one runs as written, is short enough to read in a sitting, and ships a README
naming the ideas it demonstrates.

```sh
lumenc new my-app            # blank
lumenc new my-app counter    # a named template
lumenc new --list            # the gallery with one-line descriptions
```

The template argument is optional and defaults to `blank`. `lumenc new` refuses
to write into a directory that already exists.

Every template ships `main.lmn` and `lumen.toml`. Most also ship a `main.css`
and a script. `counter` is written in candela; the other scripted templates are
written in Rhai, which reads closely enough that either one is a fine starting
point.

## blank

An empty starting point: a bare `<root>` and a `lumen.toml`, nothing else.

Use it when you know what you are building. The README points at the three ways
to grow it: add children to `<root>`, drop a `main.css` beside the markup, and
attach a script with `<script src="main.cdl" />`.

## hello

The smallest runnable app: one label filling the window, and a script that
prints a line at startup.

Shows the shape of an app in three files: the `<root>` element every app has,
a `<script>` tag with an `on_start()` handler that runs once when the app
loads, and the `[window]` block in `lumen.toml` that sets the title and size.

## counter

Click-to-bump counter with `+1` and `reset` buttons, scripted in candela.

Shows signals and bindings, the core of how a Lumen UI updates: the script
writes a named `clicks` signal, the label carries `bind-text="clicks"`, and
that is the entire connection. It also shows the DOM side: the script looks
each button up with `get_by_id` and binds a click handler on the handle it gets
back, so the `+1` button's handler is its own function rather than a branch
inside a shared handler. That lookup happens in `on_ready`, which runs once the
tree is mounted. Colours and the corner radius are custom properties in
`:root`, so re-theming is one block of edits.

This is the template the [first app walkthrough](first-app.md) reads line by
line.

## form

A profile form: a text field, a toggle, a slider, and a status line that
re-derives from all three on every edit.

Shows two-way bindings, where a control both reads and writes its signal
(`bind-text` on the input, `bind-checked` on the toggle, `bind-value` on the
slider), and the per-control lifecycle callbacks that fire alongside them. It
also puts the controls in the Tab order with `tab-index` and styles the focus
ring with `:focus`.

## todo

The canonical tutorial app: a list with add, toggle, remove, and clear-done,
plus a live count.

Shows list rendering. Rows live in an array signal, and `<for each="todos"
key="id">` spawns one subtree per item. Row fields are read with `{row.field}`,
and per-row buttons carry an interpolated id so one handler can serve every
row. Presentation flags are computed in the script so the markup stays
declarative, and the list sits in a `<scroll>` so it stays usable at any
length.

## dashboard

Stat tiles, progress bars, and an activity feed, all moving on a repeating
timer, with a pause button.

Shows time-driven UI: `set_interval` plus an `on_timer` handler, cancelled and
restarted by the pause button. Progress bars track numeric signals through
`bind-value` and `max`, and the feed keeps itself bounded by trimming the array
signal it pushes onto. The metrics come from a small simulation, and the README
points at where to swap it for a live endpoint.

## settings

A settings panel: theme dropdown, density radio group, UI scale slider,
notification checkboxes, and a summary line.

Shows the rest of the form controls (`<checkbox>`, `<radio>`, `<dropdown>` with
`<option>`, `<slider>` with a step) and their state pseudo-classes in CSS. The
summary line uses a derived value: one declaration lists the signals it depends
on, and it recomputes when any of them changes, which replaces a callback per
control.

## hotkeys

Native shell showcase: OS-global hotkeys, a tray icon with a context menu, and
desktop notifications carrying a button, with an in-app event log.

Shows the integrations that reach outside the window. Hotkeys are registered by
accelerator string and fire while another app has focus; a toggle registers and
unregisters them live, and press and release are separate handlers. The log
keeps everything observable where a shell surface is unavailable, which matters
because these features vary by platform; see the
[OS integration guide](../guides/os-integration.md) for what each one supports
where.

## Next

- [What each file in the scaffolded directory is](project-layout.md).
- [Every `lumenc` subcommand and flag](../reference/cli.md).
