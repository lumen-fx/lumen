# lumen.toml reference

`lumen.toml` sits in the app directory and declares everything static about
the app: its entry file, window, skin, locale, script engine, build hooks, and
subsystem settings. The file is optional; every key has a default.

Unknown top-level sections and unknown keys inside a section are rejected with
a parse error naming the offending key. A parse error aborts the command.

Command line flags override the matching config key.

```toml
[app]
entry = "main.lmn"
locale = "de-DE"

[window]
title = "My App"
size = [1280, 720]

[skin]
name = "auto"

[script]
engine = "candela"
```

## [app]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `entry` | string | `"main.lmn"` | Markup entry filename, relative to the app directory. |
| `id` | string | the app directory name | Stable identifier for per-app state directories, and the app id notifications are attributed to. |
| `kind` | `"markup"`, `"rust"`, `"cpp"`, `"python"` | auto-detected | Pins the build and run route instead of letting the directory contents decide. |
| `locale` | BCP-47 tag | the OS locale, else `en-US` | The locale the app starts in. Selects which `locale/<tag>.ftl` catalogue `translatable` markup and the scripts' `t()` builtin resolve against; every catalogue in the directory is loaded regardless. A tag that is not valid BCP-47 is a parse error. |

Auto-detection for `kind` looks for a `Cargo.toml` depending on `lumen`
(`rust`), a `CMakeLists.txt` (`cpp`), or a `.py` file importing `lumen`
(`python`), and falls back to `markup`.

## [window]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `title` | string | the app directory name | Window title. |
| `size` | `[w, h]` integers | `[960, 720]` | Window size in logical pixels. `lumenc run --size` overrides it. |
| `remember_state` | bool | `false` | Persists window position, size, and maximised state on close and restores them on the next launch. |

## [pages]

Multi-page navigation. See [Pages](../guides/pages.md).

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `entry` | string | see below | Home page key: a filename stem with no `.lmn`. Ignored when no page has that key. |
| `enabled` | bool | on when more than one `.lmn` file is present | Forces multi-page mode on or off. |
| `include` | array of strings | directory discovery | Explicit ordered page-file list. When set, only these files are pages. |

Without `include`, every `.lmn` file in the app directory is a page except
`layout.lmn`, which contributes its `<template>` preamble to every page
instead of becoming one.

The entry key resolves in this order: `[pages] entry` when it names an
existing page, then `index`, then the `[app] entry` stem, then `main`, then
the first page alphabetically.

## [skin]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `name` | `"default"`, `"macos"`, `"windows"`, `"linux"`, `"auto"` | none | Applies an embedded platform skin beneath the app's own CSS. Equivalent to `<root skin="...">`. `auto` picks the skin matching the host OS. |

With no skin named, no platform skin applies. A small user-agent stylesheet
setting per-tag sizing floors applies to every app either way.

## [script]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `engine` | `"candela"`, `"rhai"`, `"lua"` | per file | Forces every script in the app onto one host. Matched case-insensitively; an unrecognised value falls back to candela. |

With the key absent, each script file picks its host from its own extension: a
`.cdl` file runs under candela, a `.lua` file under Lua, a `.rhai` file under
Rhai. An app holding more than one language runs one host per language. An
inline `<script>` block has no extension to read; it joins the app's one
external language when there is exactly one, and candela otherwise. Set
`engine` when that is not the host you want, most often for an inline script
written in something other than candela.

## [mcp]

The introspection and automation server. See [Testing](../guides/testing.md)
and [Tooling](tooling.md#mcp-server).

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `port` | integer | `7878` | TCP port the server listens on, bound to `127.0.0.1`. `0` disables the server outright. |
| `simulate` | bool | `false` | Lets the server inject pointer, key, and scroll events. Required by `lumenc click`, `type`, `key`, and `scroll`. |

The server runs by default for a windowed run. A headless run turns it off
unless `simulate = true` or `[runtime] mcp = true`.

With `simulate` on, the server snapshots every tick instead of once a second,
so an automation driver sees each frame.

## [profile]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `mode` | `"off"`, `"chrome"`, `"tracy"`, `"stderr"` | `"off"` | Installs the tracing profiler for `lumenc run`. `--profile` overrides it. |

`chrome` writes `lumen-trace.json` in the current directory, `tracy` connects
to a running `tracy-profiler`, and `stderr` prints per-system spans live. All
three need a `lumenc` built with the `profiling` cargo feature; `tracy` also
needs `profiling-tracy`.

## [asset_roots]

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `paths` | array of strings | empty | Extra directories scanned for relative `src=` paths. Relative entries resolve against the app directory; absolute entries are used as given. |

## [perf]

Per-cache memory budgets.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `images_mb` | integer | `64` | Decoded-image cache cap, in MB. |
| `shape_entries` | integer | `512` | Text shape-cache cap, in entries. |
| `scene_fragments` | integer | `256` | Scene-fragment cache cap, in entries. |

## [runtime]

Forces a subsystem on or off at startup instead of letting the runtime decide.
Every key is optional; leaving one out keeps the automatic behaviour.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `audio` | bool | detected from app usage | Starts the audio subsystem, or keeps it off. |
| `mcp` | bool | on for a windowed run, off for a headless one | Runs the introspection server. `[mcp] port = 0` still disables it. |
| `hot_reload` | bool | on only for a windowed run from source | Watches source files and reloads on change. Off for headless, bounded, and artifact runs. |
| `threads` | integer | `min(cpu count, 4)` | Worker-thread budget. `LUMEN_THREADS` overrides this. |

These gate startup, not linkage: the code is still in the binary. To leave a
subsystem out of a build, use `[capabilities]`.

## [capabilities]

Compile-time subsystem selection for `lumenc bundle --static`, which builds a
runtime carrying only the listed subsystems. The shared runtime and
`lumenc run` always ship everything, and ignore this section.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `audio` | bool | inferred from `audio_*` calls and audio files in the app | Compiles the audio subsystem and its playback backend in. |
| `http-fetch` | bool | inferred from a `fetch(` call in the app | Compiles the scripts' HTTP `fetch()` builtin in. |
| `mcp` | bool | `false` | Compiles the introspection server in. Never inferred. |
| `async` | bool | inferred from a file-dialog call in the app | Compiles the async bridge in. File dialogs resolve on it, and on macOS they do not open without it. |

Inference is conservative: a capability is left out only on a reliable
unused signal, and anything ambiguous keeps it in. An explicit value always
wins.

The script host is selected by `[script] engine` or inferred from the app's
script files, not here.

## [[hooks]]

Build and setup commands the app declares for itself. Each `[[hooks]]` entry
is one command.

| Key | Type | Required | Effect |
|-----|------|----------|--------|
| `when` | `"prebuild"`, `"prerun"` | yes | Trigger point. |
| `run` | string | yes | The command line. Must not be empty or whitespace-only. |
| `os` | `"linux"`, `"macos"`, `"windows"` | no | Restricts the hook to one platform. Absent runs everywhere. |
| `inputs` | array of strings | no | Files the command reads. Used only for the staleness check. |
| `outputs` | array of strings | no | Files the command produces. Used only for the staleness check. |

```toml
[[hooks]]
when    = "prebuild"
os      = "linux"
run     = "cc -shared -fPIC -O2 -o libmd.so md.c"
inputs  = ["md.c"]
outputs = ["libmd.so"]
```

`prebuild` hooks fire for `lumenc run`, `build`, and `bundle`. `prerun` hooks
fire for `lumenc run` only, after every `prebuild` hook. `lumenc check` never
runs hooks.

Matching hooks run in declaration order with the app directory as the working
directory, through `sh -c` on Linux and macOS and `cmd /C` on Windows. A hook
that exits non-zero aborts the command; later hooks do not run.

A hook is skipped when both `inputs` and `outputs` are non-empty, every listed
file exists, and every output is at least as new as the newest input. A hook
missing either list, or one whose listed files are not all on disk, always
runs.

Hooks run shell commands read from a file in the app directory, the same trust
model as a Cargo build script. `lumenc run --no-hooks` skips them.

An unknown `when` or `os` value, or an empty `run`, is a parse error naming
the offending value.

## [signals]

An optional typed schema for the app's signals, read by
`lumenc lint --signals`. Nothing else consumes it, and a missing entry is not
an error; it downgrades the lint severity.

Each key maps a signal name to a type: `i64` (also `int`, `integer`), `f64`
(also `float`, `number`), `bool` (also `boolean`), `string` (also `str`,
`text`), `color`, `vec2`, `array`, or `object` (also `map`). An unknown type
is a parse error.

A value can also be a table. An inline table with no `type` key is an object
whose entries are its field types. An explicit `type = "array"` or
`type = "object"` with a `fields` table types the record at the leaf.

```toml
[signals]
count = "i64"
theme = "string"
user = { name = "string", email = "string" }

[signals.users]
type = "array"
fields = { id = "i64", name = "string", email = "string" }
```
