# Per-app config (`lumen.toml`)

The runtime reads `<app-dir>/lumen.toml` once at startup. Every key is
optional and CLI flags override config. Parser:
[`lumenc/src/config.rs`](https://github.com/lumen-ui/lumen/blob/main/lumenc/src/config.rs)
(`LumenToml`).

Unknown keys are rejected (`deny_unknown_fields`) so typos surface as
parse errors instead of silent no-ops.

## Full surface

```toml
[app]
entry = "main.lmn"
id    = "com.example.myapp"

[window]
title          = "My App"
size           = [960, 720]
remember_state = false

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

[capabilities]           # compile-time trim toggles for `bundle --static`
audio      = false
http-fetch = false
mcp        = false
async      = false
```

## `[app]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `entry` | string | `"main.lmn"` | Markup entry filename relative to the app dir. |
| `id` | string | app dir name | Stable identifier — Reverse-DNS recommended (`"com.example.myapp"`). Used as the per-app state directory (window state, future plugin caches). |

## `[window]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | app dir name | Window title override. |
| `size` | `[u32, u32]` | `[960, 720]` | Initial size in logical pixels. |
| `remember_state` | bool | `false` | Save `(position, size, maximized)` to the state dir on close, restore on next launch. |

State path resolution:

| OS | `<state_dir>` |
|---|---|
| Linux | `~/.local/share/<app-id>/window-state.toml` |
| macOS | `~/Library/Application Support/<app-id>/window-state.toml` |
| Windows | `%APPDATA%\<app-id>\window-state.toml` |

## `[skin]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | none | Opt into an embedded user-agent stylesheet. Equivalent to `<root skin="…">` but lives in config so apps can avoid the markup attr (an explicit markup `skin=` wins when both are set). |

Shipped skins: `"default"`, `"macos"`, `"windows"`, `"linux"`, and
`"auto"` (resolves from the running OS at startup; non-mac/non-windows
resolves to `linux`). Forcing a concrete name works on any OS — that's
the cross-platform preview path. Custom skins land once the public
skin registration API is settled.

## `[mcp]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `port` | u16 | env `LUMEN_MCP_PORT` or off | TCP port for the introspection server. Set `0` to disable. |

The MCP server exposes entity-tree snapshots, signal state, and the
input message ring to external tools — used by the planned visual
inspector.

## `[profile]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mode` | string | `"off"` | One of `off`, `chrome`, `tracy`, `stderr`. Enables span tracing. |

| Mode | Output |
|---|---|
| `off` | No tracing. |
| `chrome` | `lumen-trace.json` next to the binary — open in `chrome://tracing`. |
| `tracy` | Tracy profiler protocol on localhost:8086. |
| `stderr` | Span timings to stderr. |

`RUST_LOG` overrides the EnvFilter; default `bevy_ecs=trace,lumen=trace,lumenc=trace`.

All non-`off` modes require a lumenc built with the `profiling` cargo
feature (`cargo build -p lumenc --features profiling`); `tracy` needs
`profiling-tracy`. Default builds carry no span instrumentation — the
per-system spans come from `bevy_ecs/trace`, which those features enable —
and exit with a rebuild hint when a profile mode is requested.

## `[perf]`

Per-cache memory caps. Each cap evicts in LRU order.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `images_mb` | u32 | 64 | Decoded image cache (MB). Raise for image-heavy galleries. |
| `shape_entries` | u32 | 512 | cosmic-text shape LRU (entries — not bytes). Raise when scrolling long lists with unique per-row text. |
| `scene_fragments` | u32 | 256 | vello sub-scene cache (entries). Raise for visually-rich UIs with many distinct rects / shadows / outlines. |

Tuning notes:

- `images_mb` caps the **decoded** image bytes-on-CPU. GPU texture
  upload is a separate budget governed by wgpu.
- `shape_entries` is the most impactful for scroll-heavy apps; doubling
  it from 512 → 1024 typically takes a list with 1k unique strings
  from 60% hit-rate to ~95%.
- `scene_fragments` benefits from the empirical hit-rate logged by
  the criterion bench suite (`cargo bench -p lumen-benches`).

## `[asset_roots]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `paths` | `[string]` | `[]` | Extra directories scanned for relative `src=`. Relative paths resolve against the app dir; absolute paths are used verbatim. |

Resolution order: app dir → each path in `paths`, first hit wins. Apply
to `<image src>`, `set_src(id, path)`, and `tray_icon(id, path, …)`.

```toml
[asset_roots]
paths = ["icons", "../shared/icons", "/var/lib/myapp/themes"]
```

## `[capabilities]`

Per-app **compile-time** subsystem trim toggles for the static
`lumenc bundle --static` build (runtime tree-shaking, Part B). Unlike
`[runtime]` — which gates *initialization* inside the always-full shared
runtime — `[capabilities]` selects the cargo *feature set* the per-app static
seam (`lumen-ffi`) is compiled with, so an unused subsystem's crate is dropped
from the binary entirely.

These keys affect ONLY `lumenc bundle --static`. The shared, dlopen'd
`liblumen_ffi` cdylib and the dev `lumenc run` path always ship every
subsystem, so a plain `lumenc run` ignores this section.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `audio` | bool | inferred | Include the audio subsystem (rodio / cpal / symphonia). `None` = infer from `audio_*` builtins / audio-file references. |
| `http-fetch` | bool | inferred | Include the scripts' HTTP `fetch()` builtin (ureq + rustls + ring). `None` = infer from a `fetch(` call. |
| `mcp` | bool | `false` | Include the MCP introspection server. Dev-only; never inferred into a release bundle. |
| `async` | bool | `false` | Include the async (tokio) bridge. The markup runtime never installs it itself. |

Every field is optional. `None` lets `lumenc` **infer** the capability from a
bounded source scan; an explicit value always wins. Inference is
**conservative**: a subsystem is inferred OFF only on a reliable *unused*
signal — anything ambiguous forces it ON.

The single compiled script host is selected by `[script] engine` (or inferred
from the app's script file extensions: a `.lua` file → the Lua host, a `.cdl`
file → the candela host), not here. Rhai is the always-compiled default host,
so a Rhai app adds no host feature.

```toml
[capabilities]
audio      = false   # explicit wins over inference
http-fetch = false
mcp        = false
async      = false
```

## CLI overrides

CLI flags always win over config. Document them with `lumenc --help`;
the most common:

| Flag | Overrides |
|---|---|
| `--window-size W,H` | `[window] size` |
| `--profile <mode>` | `[profile] mode` |
| `--mcp-port <port>` | `[mcp] port` |

## Example — a weather app

```toml
[app]
entry = "main.lmn"
id    = "com.example.lumen-weather"

[window]
title          = "Lumen Weather"
size           = [800, 600]
remember_state = true

[skin]
name = "default"

[profile]
mode = "off"

[perf]
images_mb     = 32        # weather icons are small; trim cache
shape_entries = 1024      # forecast strings vary by hour

[asset_roots]
paths = ["icons"]
```

This is the actual config shipped by [`apps/weather`](https://github.com/lumen-ui/lumen/tree/main/apps/weather).
The `set_src("hero-icon", "icons/...")` Rhai call works because
`icons/` is a configured asset root.

## Reading config from script

The current Rhai surface has no `cfg(...)` accessor — config is a
runtime concern, not an authoring concern. If your script needs a
runtime knob, drive it through a signal and seed the signal in
`on_start()`. For environment-specific config that genuinely needs
to live outside the script, use OS env vars and read them from a
plugin written in Rust.
