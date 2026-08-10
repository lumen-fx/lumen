# Per-app config (`lumen.toml`)

The runtime reads `<app-dir>/lumen.toml` once at startup. Every key is
optional and CLI flags override config.

Unknown keys are rejected (`deny_unknown_fields`) so typos surface as
parse errors instead of silent no-ops.

## Full surface

```toml
[app]
entry  = "main.lmn"
id     = "com.example.myapp"
kind   = "markup"
locale = "de-DE"

[pages]
entry   = "index"
enabled = true
include = ["index.lmn", "settings.lmn"]

[window]
title          = "My App"
size           = [960, 720]
remember_state = false

[script]
engine = "candela"

[skin]
name = "default"

[mcp]
port     = 7878
simulate = false

[profile]
mode = "off"

[perf]
images_mb       = 64
shape_entries   = 512
scene_fragments = 256

[asset_roots]
paths = ["icons", "../shared"]

[runtime]                # startup subsystem overrides; all optional
audio      = false
mcp        = true        # keep MCP on for a headless/bounded run
hot_reload = true
threads    = 4

[capabilities]           # compile-time trim toggles for `bundle --static`
audio      = false
http-fetch = false
mcp        = false
async      = false

[signals]                # typed schema for `lumenc lint --signals`
count = "i64"
theme = "string"

[[hooks]]                # project build/setup commands
when    = "prebuild"
os      = "linux"
run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
inputs  = ["md.c"]
outputs = ["libmd.so"]
```

## `[app]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `entry` | string | `"main.lmn"` | Markup entry filename relative to the app dir. |
| `id` | string | app dir name | Stable identifier - Reverse-DNS recommended (`"com.example.myapp"`). Used as the per-app state directory (window state, future plugin caches). |
| `kind` | string | auto-detect | Which toolchain `run` / `build` route the app through: `"markup"`, `"rust"`, `"cpp"`, or `"python"`. Absent, `lumenc` guesses from the directory contents (a `Cargo.toml` depending on `lumen`, a `CMakeLists.txt`, a `.py` importing `lumen`). Set it when the guess is wrong. Any other value is a parse error. |
| `locale` | string | OS locale | BCP-47 tag naming the locale the app starts in, e.g. `"de-DE"`. Selects which `locale/<tag>.ftl` catalogue translations resolve against; see [Translation](./i18n.md). Absent, the app follows the OS and falls back to `en-US`. A tag that is not valid BCP-47 is a parse error. |

## `[pages]`

File-based navigation. Every `.lmn` file in the app dir is a page keyed by
its filename stem; these keys pin the parts an app wants fixed.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `entry` | string | `"index"` | Home page key. Falls back to the `[app] entry` stem, then `main`. |
| `enabled` | bool | auto | Force multi-page on or off. When absent, multi-page turns on as soon as more than one page file is present. |
| `include` | `[string]` | auto | Explicit ordered page list. When set, only these files are pages. |

## `[window]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | app dir name | Window title override. |
| `size` | `[u32, u32]` | `[960, 720]` | Initial size in logical pixels. |
| `remember_state` | bool | `false` | Save `(position, size, maximized)` to the state dir on close, restore on next launch. |

State path resolution (the state file always sits under a `lumen/` folder
inside the OS state directory, then the app id):

| OS | Full path |
|---|---|
| Linux | `$XDG_STATE_HOME/lumen/<app-id>/window-state.toml`, or `~/.local/state/lumen/<app-id>/window-state.toml` when `$XDG_STATE_HOME` is unset |
| macOS | `~/Library/Application Support/lumen/<app-id>/window-state.toml` |
| Windows | `%LOCALAPPDATA%\lumen\<app-id>\window-state.toml` |

No per-user state directory (a sandboxed CI container with no `$HOME`, for
example) makes `remember_state` a silent no-op rather than an error.

## `[script]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `engine` | string | `"candela"` | Which script host runs the app's scripts: `"candela"`, `"rhai"`, or `"lua"`. Matched case-insensitively; an unrecognised value falls back to candela. |

```toml
[script]
engine = "rhai"
```

`engine` picks the host explicitly. Leaving `[script]` out does not always
mean candela: the runtime first checks for an explicit `engine`, then infers
from the app's script file extensions (checked in this order - a `.cdl` file
selects candela, else a `.lua` file selects Lua, else a `.rhai` file selects
Rhai), and only falls back to candela when none of those match, for example
an app whose script lives entirely in an inline `<script>` tag with no
extension to read. This inference applies to `lumenc run` as much as to
`lumenc build` and `lumenc bundle` - it is not a bundle-only behavior.

`lumenc bundle --static` compiles exactly one alternate host into the binary
(Rhai itself is always linked). It resolves which one with the same
`[script] engine` / extension-inference rule above. Setting the key
explicitly is the way to be sure which host a bundle carries.

See [Scripting](./scripting.md) for the surface each host exposes.

## `[skin]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | none | Opt into an embedded user-agent stylesheet. Equivalent to `<root skin="...">` but lives in config so apps can avoid the markup attr (an explicit markup `skin=` wins when both are set). |

Shipped skins: `"default"`, `"macos"`, `"windows"`, `"linux"`, and
`"auto"` (resolves from the running OS at startup; non-mac/non-windows
resolves to `linux`). Forcing a concrete name works on any OS - that's
the cross-platform preview path. Custom skins land once the public
skin registration API is settled.

## `[mcp]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `port` | u16 | `7878` | TCP port for the MCP introspection server. Set `0` to disable it outright. |
| `simulate` | bool | `false` | Let the MCP `lumen.simulate` queue inject pointer, key, and scroll events each tick - the automation-driver mode `lumenc click` / `type` / `key` / `scroll` need. Also keeps the server running on a headless or bounded run; see below. |

The MCP server is a JSON-RPC server over TCP exposing entity-tree
snapshots, signal state, and the input message ring. `lumenc snapshot`,
`lumenc screenshot`, and the rest of the MCP tooling all talk to it. See
also [Devtools](../reference/devtools.md).

A headless or otherwise bounded run (`lumenc run --headless`, `--ticks N`,
the FFI test contract) turns the server off by default, so `lumenc
snapshot` / `screenshot` / `click` and friends have nothing to talk to
against one unless you opt back in: set `[mcp] simulate = true` (an
automation driver that also wants input injection) or `[runtime] mcp =
true` (force the server on with no simulate queue). See
[Headless mode](../reference/headless.md) for the full precedence order.

## `[profile]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mode` | string | `"off"` | One of `off`, `chrome`, `tracy`, `stderr`. Enables span tracing. |

| Mode | Output |
|---|---|
| `off` | No tracing. |
| `chrome` | `lumen-trace.json` next to the binary - open in `chrome://tracing`. |
| `tracy` | Tracy profiler protocol on localhost:8086. |
| `stderr` | Span timings to stderr. |

`RUST_LOG` overrides the EnvFilter; default `bevy_ecs=trace,lumen=trace,lumenc=trace`.

The prebuilt toolchain carries no span instrumentation, so every non-`off`
mode exits with a rebuild hint on it. Building a `lumenc` with profiling
enabled is a source-build task; see
[Building Lumen from source](../contributing/building-lumen.md) for the
feature flags that turn it on.

## `[perf]`

Per-cache memory caps. Each cap evicts in LRU order.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `images_mb` | u32 | 64 | Decoded image cache (MB). Raise for image-heavy galleries. |
| `shape_entries` | u32 | 512 | cosmic-text shape LRU (entries - not bytes). Raise when scrolling long lists with unique per-row text. |
| `scene_fragments` | u32 | 256 | vello sub-scene cache (entries). Raise for visually-rich UIs with many distinct rects / shadows / outlines. |

Tuning notes:

- `images_mb` caps the **decoded** image bytes-on-CPU. GPU texture
  upload is a separate budget governed by wgpu.
- `shape_entries` is the most impactful cap for scroll-heavy apps: raise it
  when a long list of mostly-unique row text keeps re-shaping as it scrolls.
- `scene_fragments` matters most for visually-rich UIs with many distinct
  rects, shadows, or outlines.

Raise a cap when profiling shows it evicting under normal use; the defaults
cover a typical form or dashboard app.

## `[asset_roots]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `paths` | `[string]` | `[]` | Extra directories scanned for relative `src=`. Relative paths resolve against the app dir; absolute paths are used verbatim. |

Resolution order: app dir -> each path in `paths`, first hit wins. Apply
to `<image src>`, `set_src(id, path)`, and `tray_icon(id, path, ...)`.

```toml
[asset_roots]
paths = ["icons", "../shared/icons", "/var/lib/myapp/themes"]
```

## `[runtime]`

Per-app overrides for startup subsystem decisions the runtime otherwise
makes automatically. Every key is optional: `None` (the key absent) keeps
the automatic behavior; `true` / `false` forces it.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `audio` | bool | auto-detect | Force the audio subsystem (output stream + position ticker) on or off. Auto-detect scans the app's script and markup for audio usage. |
| `mcp` | bool | auto | Force the MCP introspection server on or off. Auto is on for an interactive windowed run, off for a headless or bounded run unless `[mcp] simulate = true`. `[mcp] port = 0` hard-disables the server even when this is `true`. |
| `hot_reload` | bool | auto | Force the source-file watcher on or off. Auto is on only for an interactive run from source; a headless, bounded, precompiled-artifact, or in-memory run defaults off. |
| `threads` | integer | `min(available_parallelism, 4)` | bevy_ecs worker-thread budget. The `LUMEN_THREADS` env var overrides this at runtime. |

`[runtime] mcp = true` is the key to reach for when a headless or bounded
run (`--headless`, `--ticks N`, the FFI test contract) needs the MCP
server without also turning on `[mcp] simulate`'s input-injection queue -
see [Headless mode](../reference/headless.md).

These gate initialization inside the full runtime. To drop a subsystem from a
binary entirely, use `[capabilities]` below.

## `[capabilities]`

Which subsystems a static `lumenc bundle --static` binary carries. A subsystem
turned off here is left out of the binary rather than merely left uninitialised,
which is how a bundled app stays small.

These keys apply only to `lumenc bundle --static`. A plain `lumenc run` ships
every subsystem and ignores this section.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `audio` | bool | inferred | Include the audio subsystem. Inferred from `audio_*` calls and audio-file references. |
| `http-fetch` | bool | inferred | Include the scripts' HTTP `fetch()` builtin. Inferred from a `fetch(` call. |
| `mcp` | bool | `false` | Include the MCP introspection server. A development capability, never inferred into a release bundle. |
| `async` | bool | `false` | Include the async (tokio) bridge. |

Every field is optional; leaving one out lets `lumenc` infer the capability from
a bounded source scan, and an explicit value always wins. Inference is
conservative: a subsystem drops out only on a reliable unused signal, and
anything ambiguous keeps it in.

The script host a bundle compiles is selected by `[script] engine`, not here.

```toml
[capabilities]
audio      = false   # explicit wins over inference
http-fetch = false
mcp        = false
async      = false
```

## `[signals]`

An optional typed schema for your signals, read by `lumenc lint --signals` to
flag untyped writes and type mismatches. Each entry names a signal and its
expected type: `i64`, `f64`, `bool`, `string`, `color`, `vec2`, `array`,
`object`, an inline table for a record, or an explicit array record. The
common aliases work too (`int` / `integer`, `float` / `number`, `boolean`,
`str` / `text`, `map`); an unrecognised type name is a parse error listing the
accepted set. Signals you leave out are not errors; they just get a weaker
lint.

```toml
[signals]
count = "i64"
theme = "string"
user  = { name = "string", email = "string" }

[signals.users]
type   = "array"
fields = { id = "i64", name = "string" }
```

## `[[hooks]]`

A TOML array of tables declaring the commands that produce an app's native
artifacts - a C library imported via a script's `dylib` block, a bundled
asset generated by another toolchain, anything the app directory needs on
disk before it can load. Each `[[hooks]]` entry is one command:

```toml
[[hooks]]
when    = "prebuild"
os      = "linux"
run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
inputs  = ["md.c"]
outputs = ["libmd.so"]
```

| Key | Type | Required | Meaning |
|---|---|---|---|
| `when` | string | yes | Trigger point: `prebuild` or `prerun`. Any other value is a config error naming the value and the accepted set. |
| `os` | string | no | Restrict the hook to `linux`, `macos`, or `windows` (matched against the running platform). Omitted runs on every platform. An unknown value is a config error. |
| `run` | string | yes | The command line. Runs via `sh -c` on Linux/macOS and `cmd /C` on Windows, with the app directory as the working directory. Empty or whitespace-only is a config error. |
| `inputs` | `[string]` | no | Files the command reads. Used only for the staleness check below. |
| `outputs` | `[string]` | no | Files the command produces. Used only for the staleness check below. |

Relative `inputs` / `outputs` resolve against the app directory, regardless
of the caller's working directory.

Trigger points:

| `when` | Fires for |
|---|---|
| `prebuild` | `lumenc run`, `lumenc build`, `lumenc bundle` (including `bundle --static`) |
| `prerun` | `lumenc run` only, after every `prebuild` hook has run |

`lumenc check` never runs hooks - a check stays side-effect free.

Hooks in the same file run in declaration order. A hook exiting non-zero
aborts the command immediately with the hook's command line and exit status;
later hooks do not run. Child stdout/stderr are inherited, so compiler
diagnostics reach you unfiltered.

**Staleness**: when a hook declares both `inputs` and `outputs`, it is
skipped if every listed output already exists and is at least as new as the
newest listed input. A hook missing either list always runs, and so does one
whose declared input doesn't exist on disk - the command itself gives a
better error than a hook runner guessing about a missing source file.

Pass `--no-hooks` to `run`, `build`, or `bundle` to skip every hook - useful
for a source tree you already built, or for a CI cache hit.

A hook runs whatever command the app's `lumen.toml` names, the same trust
model as a Cargo build script: `lumenc run <dir>` on an app from an
untrusted source runs that app's hooks. Use `--no-hooks` when you don't
trust the source.

## CLI overrides

CLI flags always win over config. Document them with `lumenc --help`;
the most common:

| Flag | Overrides |
|---|---|
| `--profile <mode>` | `[profile] mode` |
| `--headless --size WxH` | `[window] size` (headless offscreen viewport only; a windowed run always takes `[window] size`) |
| `--no-hooks` | Every `[[hooks]]` entry - see [`[[hooks]]`](#hooks) above |

## Example - a weather app

```toml
[app]
entry = "main.lmn"

[script]
engine = "lua"

[window]
title = "Lumen - Weather"
size  = [1280, 800]

[asset_roots]
paths = ["icons"]
```

This is the actual config shipped by [`apps/weather`](https://github.com/lumen-fx/lumen/tree/main/apps/weather).
It picks the Lua script host explicitly (`[script] engine = "lua"`) and
otherwise leans on defaults - no `[skin]`, `[profile]`, or `[perf]`
override. The `set_src("hero-icon", "icons/...")` Lua call works because
`icons/` is a configured asset root.

## Reading config from script

There is no `cfg(...)` accessor: config is a runtime concern, not an authoring
concern. If your script needs a runtime knob, drive it through a signal and seed
the signal in `on_start()`. For environment-specific values that have to live
outside the script, read OS env vars from a plugin written in Rust.
