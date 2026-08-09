# Template gallery

`lumenc new <template> <name>` scaffolds a runnable app directory from one of
seven built-in templates. Every one ships `main.lmn`, a script, `lumen.toml`,
and a `README.md` explaining the concepts it demonstrates; all but `hello`
also ship a `main.css`. They all run as scaffolded:

```sh
lumenc new --list          # print this gallery
lumenc new todo my-todo
lumenc run my-todo         # or: lumenc run my-todo --headless
```

| Template | What it is | What it teaches |
|---|---|---|
| `hello` | The smallest runnable app: one label and a script that says hi. | `<root>`, `<label>`, `<script>`, `lumen.toml` |
| `counter` | Click-to-bump counter, scripted in candela. | signals, `bind-text`, per-id click routing, CSS custom properties |
| `form` | Two-way bound form with a live status line. | `bind-text` / `bind-checked` / `bind-value`, `on_text_input` / `on_toggle` / `on_slider`, focus styling |
| `todo` | The canonical tutorial app: a reactive list with add, toggle, and remove. | array signals, `<for each key>`, `{row.field}`, per-row action ids routed through the global `on_click`, `<scroll>` |
| `dashboard` | Stat tiles, progress bars, and an activity feed driven by a timer. | `<tile>` composition, `<progress bind-value>`, `set_interval` / `cancel_timer`, bounded array feeds |
| `settings` | A settings panel: grouped controls plus a derived summary. | `<checkbox>`, `<radio group>`, `<dropdown>` + `<option>`, `<slider step>`, `derive()`, state pseudo-classes |
| `hotkeys` | A native-shell showcase. | `register_hotkey` / `on_hotkey`, `tray_icon` / `on_tray`, `notify`, re-arming from a toggle |

A good path through them: `hello`, then `counter`, then `todo` for the full
reactive-list pattern, `form` and `settings` for input handling, `dashboard`
for timers and feeds, and `hotkeys` when you need the OS shell.

[Your first app](./first-app.md) walks the `counter` template line by line.

## Which language each template is in

`counter` ships a candela script (`main.cdl`). The other six ship Rhai
(`main.rhai`). Both run as scaffolded, because Lumen picks the host from the
script file extension.

To move one to candela, replace `main.rhai` with a `main.cdl` and point the
`<script>` tag at it. No `lumen.toml` edit is needed: a `.cdl` file wins over
a `.rhai` file sitting next to it. Set `[script] engine = "candela"` to pin
the choice regardless of which files are present.

The markup, the CSS, and the concepts carry over unchanged; only the script
language differs. Start a `.cdl` script with `import "lumen.cdl";` to reach
the whole builtin surface, and go through the `Node` and `Event` methods that
import defines (`get_by_id(id).set_text(...)`, `event(ev).target()`) rather
than the prefixed `lumen::node_*` / `lumen::event_*` free functions
underneath them.

The one thing that does not port directly is `todo`'s array signal. candela
has no `signal_array`, so a candela version of that app builds its rows as
real elements through the DOM API. See
[the dynamic DOM API](../authoring/scripting.md#the-dynamic-dom-api).

## Full apps to read

The templates are small on purpose. For complete apps, the repo ships
[`apps/notes`](https://github.com/lumen-fx/lumen/tree/main/apps/notes),
[`apps/music`](https://github.com/lumen-fx/lumen/tree/main/apps/music),
[`apps/tracker`](https://github.com/lumen-fx/lumen/tree/main/apps/tracker), and
[`apps/widget-garden`](https://github.com/lumen-fx/lumen/tree/main/apps/widget-garden),
all in candela, plus
[`apps/kanban`](https://github.com/lumen-fx/lumen/tree/main/apps/kanban) in
Rhai and
[`apps/weather`](https://github.com/lumen-fx/lumen/tree/main/apps/weather) in
Lua.
