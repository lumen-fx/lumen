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
| `counter` | Click-to-bump counter, scripted in candela. | signals, `bind-text`, per-id click routing, CSS custom properties |
| `form` | Two-way bound form with a live status line. | `bind-text` / `bind-checked` / `bind-value`, `on_text_input` / `on_toggle` / `on_slider`, focus styling |
| `todo` | The canonical tutorial app: reactive list with add / toggle / remove. | array signals, `<for each key>`, `{row.field}`, per-row action ids + global `on_click` prefix routing, `<scroll>` |
| `dashboard` | Stat tiles + progress bars + activity feed, animated by a timer. | `<tile>` composition, `<progress bind-value>`, `set_interval` / `cancel_timer`, bounded array feeds |
| `settings` | Settings panel: grouped controls + derived summary. | `<checkbox>`, `<radio group>`, `<dropdown>` + `<option>`, `<slider step>`, `derive()`, state pseudo-classes |
| `hotkeys` | Native shell showcase. | `register_hotkey` / `on_hotkey`, `tray_icon` / `on_tray`, `notify`, live re-arm via toggle |

Recommended path: `hello` -> `counter` -> `todo` (the tutorial app) ->
`form` / `settings` for the forms pattern -> `dashboard` for timers and
feeds -> `hotkeys` when you need the OS shell.

## Script language per template

`counter` ships a candela script (`main.cdl`), and candela is the default
host. The rest of the gallery ships Rhai (`main.rhai`). All of them run as
scaffolded: Lumen picks the host from the script file extension.

To move one of them to candela, replace `main.rhai` with a `main.cdl` and
point the `<script>` tag at it. No `lumen.toml` edit is needed: `lumenc`
infers the host from the script file extensions present in the app
directory, and a `.cdl` file wins outright over a `.rhai` file sitting next
to it. Set `[script] engine = "candela"` if you want to pin the choice
regardless of which files are present.

The markup, the CSS, and the concepts in the table above carry over
unchanged; only the script language differs. Start a `.cdl` script with
`import "lumen.cdl";` to reach the whole builtin surface, and reach the DOM
through the `Node` and `Event` method sugar it defines
(`get_by_id(id).set_text(...)`, `event(ev).target()`) rather than the
prefixed `lumen::node_*` / `lumen::event_*` free functions.

[Your first app](./first-app.md) walks the `counter` template line by
line. For full apps to read, `apps/notes`, `apps/music`, `apps/tracker`,
and `apps/widget-garden` in the repo are candela.
