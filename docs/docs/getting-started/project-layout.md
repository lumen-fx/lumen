# Project layout

A Lumen app lives in a single directory. The minimum is `main.lmn`;
everything else is optional and resolves relative to that directory.

```text
my-app/
|-- main.lmn        # required - markup root
|-- main.css        # optional - styling
|-- main.cdl        # optional - candela script
|-- lumen.toml      # optional - per-app config
|-- icons/          # optional - image assets, resolved relative to main.lmn
`-- assets/         # any extra dirs listed in [asset_roots]
```

Before any of that, `lumenc run` / `build` / `bundle` run the app's
declared `[[hooks]]` `prebuild` commands (`run` also fires `prerun`
afterward) - the step that produces a native artifact a script imports,
for example a C library built from source. `lumenc check` never runs
hooks. See [`[[hooks]]`](../authoring/lumen-toml.md#hooks) for the schema
and `--no-hooks` to skip them.

The script file is `.cdl` for candela, the default language and the one
these docs use throughout. Rhai (`.rhai`) and Lua (`.lua`) scripts work
the same way and expose the same builtins; `[script] engine` in
`lumen.toml` picks which host runs them explicitly. Leave it unset and
`lumenc` infers the host from the script file extensions present in the
app directory (a `.cdl` file wins outright over a `.rhai` or `.lua` file
in the same directory), falling back to candela when the directory
carries no script at all.

The compiler then reads:

1. `lumen.toml` if it exists (otherwise defaults).
2. The entry markup file - `[app] entry = "main.lmn"` by default.
3. Any inline `<script>` body, then every file named by a
   `<script src="..." />` tag, concatenated into one program.
4. `main.css`, if it sits next to the entry file. Additional sheets come
   in through `@import "..."` from that file.

## `lumen.toml`

```toml
[app]
entry = "main.lmn"
id    = "com.example.myapp"

[script]
engine = "candela"   # default; also "rhai" or "lua"

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

Every key is optional; an app that declares no `[script] engine` gets the
host inferred from its script file extensions, defaulting to candela when
none are present. CLI flags override config values. The full surface is
documented in [Per-app config](../authoring/lumen-toml.md).

Unknown keys are rejected (`deny_unknown_fields`) so typos surface as
parse errors rather than silent no-ops.

## Hot-reloadable files

| File | Behaviour on save |
|---|---|
| `main.lmn` | Re-parse + re-spawn, preserving stateful components by `LumenId`. Text input cursor, toggle / slider state, scroll position survive. |
| `main.css` | Re-apply styling. A class-invalidation set fast-rejects no-op class flips. |
| `main.cdl` | Compile the new source, then swap it in. Signals and handler registrations survive; a source that fails to compile leaves the running script untouched. |
| `lumen.toml` | Read once at startup. Restart `lumenc run` to pick up changes. |
| Assets in `[asset_roots]` | Image cache invalidates on next decode pull; `set_src(id, path)` re-resolves through the configured roots. |

`lumenc run` watches the entry markup, its `main.css`, every script it
references, and every included or imported file. A change wakes the loop
for one tick and reloads only the affected path. Set
`LUMEN_HOT_RELOAD_POLL=1` to fall back to mtime polling where a
file-system watcher is unavailable.

## Window-state persistence

With `[window] remember_state = true`, Lumen saves
`(position, size, maximized)` to
`<state_dir>/lumen/<app-id>/window-state.toml` on close and restores it
on next launch.

`<state_dir>` resolves to the OS-standard per-user state dir:

| OS | Path |
|---|---|
| Linux | `$XDG_STATE_HOME`, else `~/.local/state` |
| macOS | `~/Library/Application Support` |
| Windows | `%LOCALAPPDATA%` |

`<app-id>` is `[app] id`, falling back to the app directory name. When no
per-user state dir is available (a container without `$HOME`, say),
`remember_state` does nothing.

## Asset resolution

Image `src="..."`, `set_src(id, path)`, and CSS `bg: url(...)` (deferred)
all resolve relative paths through the same lookup:

1. The app directory itself.
2. Each path listed in `[asset_roots] paths`, in order. Relative paths
   resolve against the app directory; absolute paths are used verbatim.

Hits short-circuit, so an `icons/sun.png` in the app dir wins over a
sibling file in `../shared/icons/sun.png`.

## What lives outside the app directory

- The C-ABI, for embedding Lumen in a C, C++, Python, or Rust host. See
  the [FFI guide](../reference/ffi.md).
- Multi-window is on the roadmap. Today an app is one window.
