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
tag. To work in candela instead, replace those two with a `main.cdl` and a
`<script src="main.cdl" />` tag, and name the host in `lumen.toml`:

```toml
[script]
engine = "candela"
```

The markup, the CSS, and the concepts in the table above are unchanged;
only the script language differs. Start a `.cdl` script with
`import "lumen.cdl";` to reach the whole builtin surface, and see
[your first app](./first-app.md) for the counter template written this
way. For a full app to read, `apps/notes`, `apps/music`, `apps/tracker`,
and `apps/widget-garden` in the repo are candela.

Every scaffolded app also serves the [browser inspector](../reference/devtools.md)
on its MCP port while running - `http://127.0.0.1:7878/` - which is the
fastest way to see the signals these templates write.
