# Template gallery

`lumenc new <template> <name>` scaffolds a runnable app directory from a
built-in template. Every template ships `main.lmn`, `main.css` (where
styling matters), a script, `lumen.toml`, and a `README.md` explaining
the concepts it demonstrates. All of them run out of the box:

```sh
lumenc new --list          # print this gallery
lumenc new todo my-todo
lumenc run my-todo         # or: lumenc run my-todo --headless
```

| Template | One-liner | Concepts it teaches |
|---|---|---|
| `hello` | Smallest runnable app: one label + a script that says hi. | `<root>`, `<label>`, `<script>`, `lumen.toml` |
| `counter` | Click-to-bump counter. | signals, `bind-text`, per-id click routing, CSS custom properties |
| `form` | Two-way bound form with a live status line. | `bind-text` / `bind-checked` / `bind-value`, `on_text_input` / `on_toggle` / `on_slider`, focus styling |
| `todo` | The canonical tutorial app: reactive list with add / toggle / remove. | array signals, `<for each key>`, `{row.field}`, per-row action ids + global `on_click` prefix routing, `<scroll>` |
| `dashboard` | Stat tiles + progress bars + activity feed, animated by a timer. | `<tile>` composition, `<progress bind-value>`, `set_interval` / `cancel_timer`, bounded array feeds |
| `settings` | Settings panel: grouped controls + derived summary. | `<checkbox>`, `<radio group>`, `<dropdown>` + `<option>`, `<slider step>`, `derive()`, state pseudo-classes |
| `hotkeys` | Native shell showcase. | `register_hotkey` / `on_hotkey`, `tray_icon` / `on_tray`, `notify`, live re-arm via toggle |

Recommended path: `hello` -> `counter` -> `todo` (the tutorial app) ->
`form` / `settings` for the forms pattern -> `dashboard` for timers and
feeds -> `hotkeys` when you need the OS shell.

## Writing a template in candela

The templates emit a `main.rhai` script and a `<script src="main.rhai" />`
tag - candela is the default host, but the built-in gallery predates that
change. To work in candela instead, replace those two with a `main.cdl` and
a `<script src="main.cdl" />` tag. No `lumen.toml` edit is needed: `lumenc`
infers the host from the script file extensions present in the app
directory, and a `.cdl` file wins outright over a `.rhai` file sitting next
to it. Delete the scaffolded `main.rhai` once `main.cdl` covers the same
logic, or set `[script] engine = "candela"` explicitly if you want to pin
the choice regardless of which files are present.

The markup, the CSS, and the concepts in the table above are unchanged;
only the script language differs. Start a `.cdl` script with
`import "lumen.cdl";` to reach the whole builtin surface, use the `Node`
and `Event` method sugar it defines (`node.set_text(...)`,
`event.target()`) rather than the prefixed `lumen::node_*` /
`lumen::event_*` free functions, and see [your first app](./first-app.md)
for the counter template written this way. For a full app to read,
`apps/notes`, `apps/music`, `apps/tracker`, and `apps/widget-garden` in
the repo are candela.
