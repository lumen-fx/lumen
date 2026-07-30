# Project layout

A Lumen app lives in a single directory. The minimum is `main.lmn`;
everything else is optional and resolves relative to that directory.

```text
my-app/
├── main.lmn        # required — markup root
├── main.css        # optional — styling
├── main.rhai       # optional — scripting
├── lumen.toml      # optional — per-app config
├── icons/          # optional — image assets, resolved relative to main.lmn
└── assets/         # any extra dirs listed in [asset_roots]
```

The compiler reads:

1. `lumen.toml` if it exists (otherwise defaults).
2. The entry markup file — `[app] entry = "main.lmn"` by default.
3. Any `<script src="...rhai" />` referenced from markup, plus
   `main.rhai` if present.
4. `<link rel="stylesheet" href="..." />` is not yet supported;
   stylesheets are picked up implicitly: if `main.css` is next to
   the entry file, it's applied.

## `lumen.toml`

```toml
[app]
entry = "main.lmn"
id    = "com.example.myapp"

[window]
title          = "My app"
size           = [960, 720]
remember_state = true

[skin]
name = "default"

[mcp]
port = 7878

[profile]
mode = "off"

[perf]
images_mb       = 64
shape_entries   = 512
scene_fragments = 256

[asset_roots]
paths = ["icons", "../shared"]
```

Every key is optional. CLI flags override config values. The
full surface is documented in [Per-app config](../authoring/lumen-toml.md).

Unknown keys are rejected (`deny_unknown_fields`) so typos surface as
parse errors rather than silent no-ops.

## Hot-reloadable files

| File | Behaviour on save |
|---|---|
| `main.lmn` | Re-parse + re-spawn, preserving stateful components by `LumenId`. Text input cursor, toggle / slider state, scroll position survive. |
| `main.css` | Re-apply styling. A class-invalidation set fast-rejects no-op class flips. |
| `main.rhai` | `RhaiHost::replace_ast` swaps the AST and keeps the live `Scope`, so signals + ArraySignals + handler registrations survive. |
| `lumen.toml` | Read once at startup. Restart `lumenc run` to pick up changes. |
| Assets in `[asset_roots]` | Image cache invalidates on next decode pull; `set_src(id, path)` re-resolves through the configured roots. |

`lumenc run` polls the modification time of the entry markup, its
`main.css`, and every referenced `.rhai` file once per tick. When a
timestamp changes it reloads only the affected path; unchanged ticks
cost one `stat` per watched file and nothing else.

## Window-state persistence

With `[window] remember_state = true`, Lumen saves
`(position, size, maximized)` to
`<state_dir>/<app-id>/window-state.toml` on close and restores it on
next launch.

`<state_dir>` resolves to the OS-standard local data dir:

| OS | Path |
|---|---|
| Linux | `~/.local/share/<app-id>/window-state.toml` |
| macOS | `~/Library/Application Support/<app-id>/window-state.toml` |
| Windows | `%APPDATA%\<app-id>\window-state.toml` |

`<app-id>` is `[app] id`, falling back to the app directory name.

## Asset resolution

Image `src="…"`, `set_src(id, path)`, and CSS `bg: url(...)` (deferred)
all resolve relative paths through the same lookup:

1. The app directory itself.
2. Each path listed in `[asset_roots] paths`, in order. Relative paths
   resolve against the app directory; absolute paths are used verbatim.

Hits short-circuit, so an `icons/sun.png` in the app dir wins over a
sibling file in `../shared/icons/sun.png`.

## Where the FFI / multi-window stories land

Currently *outside* the per-app directory:

- The C-ABI lives in `lumen-ffi` (workspace member). See the
  [FFI guide](../reference/ffi.md).
- Multi-window is on the roadmap (entity-subgraph-per-window). Today an
  app is one window.

When those land they layer on top of this directory shape — no
incompatible changes planned.
