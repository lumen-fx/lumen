# Project layout

A Lumen app lives in a single directory. The only required file is
`main.lmn`; everything else is optional and resolves relative to that
directory.

- `main.lmn` - the markup root.
- `main.css` - styling, picked up automatically.
- `main.cdl` - the script.
- `lumen.toml` - per-app config.
- `settings.lmn` - a second page.
- `icons/` - image assets.
- `assets/` - any extra directory listed in `[asset_roots]`.

The script file is `.cdl` for candela, the default language and the one
these docs use throughout. Rhai (`.rhai`) and Lua (`.lua`) work the same
way; see [Choosing a host](../authoring/scripting.md#choosing-a-host) for
how Lumen decides which one runs.

Drop a second `.lmn` file next to the first and the app becomes multi-page,
with each file addressable as a route. See [Pages](../authoring/pages.md).

## What gets loaded, in order

1. `lumen.toml`, if it exists. Otherwise the defaults.
2. The entry markup file, `main.lmn` unless `[app] entry` says otherwise,
   with every `<include src="...">` spliced in.
3. Every inline `<script>` body, then every file named by a
   `<script src="...">` tag, concatenated in document order into one
   program.
4. `main.css`, if it sits next to the entry file. Nothing in the markup
   references it; the loader looks for it by name. Further stylesheets come
   in through `@import` from that file.

Before any of that, `lumenc run` / `build` / `bundle` run the app's
declared `[[hooks]]`, the step that produces a native artifact the app
needs on disk. `lumenc check` never runs hooks. See
[`[[hooks]]`](../authoring/lumen-toml.md#hooks).

## `lumen.toml`

```toml
[app]
entry = "main.lmn"
id    = "com.example.myapp"

[window]
title          = "My app"
size           = [960, 720]
remember_state = true

[asset_roots]
paths = ["icons", "../shared"]
```

Every key is optional, and CLI flags override config values. Unknown keys
are rejected, so a typo surfaces as a parse error rather than a silent
no-op. [Per-app config](../authoring/lumen-toml.md) documents every table.

## What reloads while the app runs

`lumenc run` watches the entry markup, its stylesheet, every script it
references, and every included or imported file. A change reloads only the
path that changed; [Your first app](./first-app.md#hot-reload) walks
through what each one does.

Two things sit outside that loop:

| File | Behaviour |
|---|---|
| `lumen.toml` | Read once at startup. Restart `lumenc run` to pick up a change. |
| Assets under `[asset_roots]` | The image cache invalidates on the next decode, and `set_src(id, path)` re-resolves through the configured roots. |

Set `LUMEN_HOT_RELOAD_POLL=1` to fall back to mtime polling where a
filesystem watcher is unavailable.

## Window-state persistence

With `[window] remember_state = true`, Lumen saves the window's position,
size, and maximized state on close and restores it on the next launch,
under `<state_dir>/lumen/<app-id>/window-state.toml`.

| OS | `<state_dir>` |
|---|---|
| Linux | `$XDG_STATE_HOME`, else `~/.local/state` |
| macOS | `~/Library/Application Support` |
| Windows | `%LOCALAPPDATA%` |

`<app-id>` is `[app] id`, falling back to the app directory name. Where no
per-user state directory exists, a container without `$HOME` for example,
`remember_state` does nothing.

## Asset resolution

Image `src="..."`, `set_src(id, path)`, and `tray_icon(id, path, ...)` all
resolve a relative path the same way:

1. The app directory itself.
2. Each path listed in `[asset_roots] paths`, in order. Relative paths
   resolve against the app directory; absolute paths are used as written.

The first hit wins, so an `icons/sun.png` in the app directory beats a
sibling `../shared/icons/sun.png`.
