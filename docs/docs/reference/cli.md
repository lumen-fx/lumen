# CLI reference (`lumenc`)

`lumenc` is the whole developer-facing interface to Lumen: it runs an app,
compiles one ahead of time, packs it for distribution, scaffolds a new one,
and drives a running app for testing and automation. If you never open the
Rust source, this is the tool you use.

Every command below is a `lumenc` subcommand. Run `lumenc --help` for the
built-in summary and `lumenc --version` to print the installed version.

A markup app directory needs `main.lmn`; `main.css` is optional, and a
`<script>` tag inside `main.lmn` loads the script host, candela by default
(see [Scripting](../authoring/scripting.md) for Rhai and Lua). `run` and
`build` auto-detect an app written against one of the SDKs instead - a Rust
project depending on the `lumen` crate, a CMake C++ project, or a Python
script importing `lumen` - and hand off to that project's own toolchain
(the Rust path needs `cargo`, the C++ path needs `cmake`, in each case a
toolchain this CLI does not provide). Set `[app] kind = "markup"` (or
`"rust"` / `"cpp"` / `"python"`) in `lumen.toml` to override the guess. The
rest of this page covers the markup path, which is what `lumenc new`
scaffolds.

## Running and checking an app

### `lumenc run <dir>`

Runs `<dir>/main.lmn` (+ optional `main.css`), opening a window. Edits to
the markup, stylesheet, or script hot-reload in place while it runs,
preserving state where it can - see the hot-reload walkthrough in
[Your first app](../getting-started/first-app.md).

Flags:

| Flag | Meaning |
|---|---|
| `--headless` | No window; see [Headless mode](headless.md). |
| `--size WxH` | Logical viewport size for a headless run (e.g. `--size 1280x800`). Requires `--headless`. |
| `--dpr N` | Device pixel ratio of the offscreen target for a headless run. Requires `--headless`. |
| `--ticks N` | Run exactly `N` ticks then exit, for a headless run. Requires `--headless`. |
| `--artifact <file>` | Load a precompiled `.lmna` artifact (from `lumenc build`) instead of parsing source. |
| `--profile chrome\|tracy\|stderr` | Capture per-system tick spans. `chrome` writes `lumen-trace.json` (open in `chrome://tracing` or <https://ui.perfetto.dev>); `tracy` connects to a running `tracy-profiler`; `stderr` prints one line per span. All three need a `lumenc` built with the `profiling` cargo feature (`tracy` additionally needs `profiling-tracy`); a standard build errors with a rebuild hint instead of silently recording nothing. |
| `--no-hooks` | Skip the `[[hooks]]` `prebuild` and `prerun` commands declared in `lumen.toml` (see below). |

`--size`, `--dpr`, and `--ticks` only make sense for a headless run and
`run` rejects them without `--headless`.

Before the app starts, `run` executes `lumen.toml`'s `[[hooks]]` entries
tagged `prebuild` and then `prerun`, in declaration order, skipping any
whose declared outputs are already newer than their declared inputs.
`--no-hooks` skips both passes. See
[`[[hooks]]`](../authoring/lumen-toml.md#hooks) for how to declare one -
the notes example compiles a small native library this way. `SIGINT` /
`SIGTERM` exit cleanly through the same close path a window's close button
takes.

### `lumenc check <dir>`

Parses the app and reports element and script counts without opening a
window - a fast CI gate for "does this app still parse." It runs no
build hooks, unlike `run` / `build` / `bundle`.

```
$ lumenc check my-app
my-app: ok (14 elements, script: yes)
```

### `lumenc build <app_dir> <out.lmna> [--no-hooks]`

Ahead-of-time compiles an app: parses `main.lmn` + `main.css` once, runs
the CSS cascade, resolves asset and import paths, and bakes the combined
script source into a single `.lmna` artifact. Reach for this to ship an
app without carrying the markup parser at runtime, or to skip re-parsing
on every launch. Run the result with `lumenc run <dir> --artifact
<out.lmna>`. `--no-hooks` skips the `prebuild` hooks that otherwise run
first.

An `.lmna` file is versioned; a file built by an older `lumenc` can be
rejected by a newer runtime after an incompatible change to the compiled
format (see [FFI](ffi.md) for the version this build understands).
Rebuild with the current `lumenc build` when that happens.

## Packaging

### `lumenc bundle <app_dir> <out.lpak> [--no-hooks]`

Packs every regular file under `app_dir` (markup, CSS, scripts, images,
fonts; dotfiles and `target/` are skipped) into one `.lpak` archive -
the asset-packaging analogue of GTK's `glib-compile-resources` or Qt's
`rcc`. `--no-hooks` skips the `prebuild` hooks that otherwise run first.

### `lumenc bundle --static <app_dir> <out_dir> [--no-hooks]`

Builds a trimmed, app-specific runtime instead of packing assets: it reads
`lumen.toml`'s `[capabilities]` plus a scan of the app's scripts, works out
which runtime subsystems (audio, MCP, the async bridge, `http-fetch`,
which script hosts) the app touches, and compiles `liblumen_ffi`
with only those. Use it for a release build where the extra megabytes of
an unused subsystem matter; the default shared `liblumen_ffi` and
`lumenc run` both stay full-featured.

This step compiles the runtime from source, so it needs the Lumen source
tree and a working Rust build environment on the machine doing the build -
a plain prebuilt-toolchain install does not carry what this needs. If it
cannot find the source tree on its own, point it there with the
`LUMEN_WORKSPACE_DIR` environment variable.

## Scaffolding and formatting

### `lumenc new <template> <name>`

Scaffolds a new app directory `<name>` from a built-in template: `hello`,
`counter`, `form`, `todo`, `dashboard`, `settings`, or `hotkeys`. Both
arguments are required and positional; there is no default template.
Every template ships a runnable `main.lmn`, a script, and a README
explaining what it demonstrates, and all but `hello` ship a stylesheet.
`new` refuses to overwrite an existing directory and has no force flag.

```
$ lumenc new counter my-counter
$ lumenc run my-counter
```

`lumenc new --list` prints the template gallery with its one-line
descriptions.

### `lumenc fmt <file> [--check]`

Reformats a `.lmn` file in place. `--check` exits non-zero if the file
would change, without writing anything - use it as a CI gate.

## Driving a running app

These commands talk to the MCP introspection server a running `lumenc
run` exposes over a local TCP JSON-RPC connection - the same mechanism an
AI agent or a CI job uses to look at and control an app under test without
a screen. A windowed run starts this server on its own; a headless run
does not unless `lumen.toml` turns it on. See [Headless
mode](headless.md#how-it-differs-from-a-windowed-run) for the
`[mcp]` / `[runtime]` settings that control it, and
`lumen/mcp-server/README.md` for the full JSON-RPC tool list.

Every command in this section resolves its port the same way: an explicit
`--port N` flag wins, then the `LUMEN_MCP_PORT` environment variable, then
`lumen.toml`'s `[mcp] port` when `--app <dir>` names the app, then `7878`.
Most also accept `--app <dir>` for that lookup and `--json` for
machine-readable output in place of the default text summary.

### `lumenc snapshot [--text|--json] [--max-lines N] [--cursor C] [--include-invisible]`

Dumps the running app's element tree as a compact, accessibility-tree-style
text listing (or JSON). `--max-lines` / `--cursor` page through a large
tree; `--include-invisible` keeps elements the default view omits.

### `lumenc find [--text S] [--role R] [--id N] [--limit N]`

Selector-style search over the live snapshot: match by visible text,
accessibility role, or entity id. Prints one row per hit (id, role, label,
bounds, state) and exits non-zero on no matches.

### `lumenc element-at <x> <y>`

Reports the topmost element at a logical-pixel point. Exits non-zero on a
miss.

### `lumenc click <x> <y> [--button primary|secondary|middle] [--wait-for <MessageType>]`

Injects a click at a logical-pixel point via input simulation. Requires
`[mcp] simulate = true` in the app's `lumen.toml`; without it the command
reports simulation as disabled and fails. `--wait-for` blocks until a
message of the given type (e.g. `ClickEvent`) has fired, useful for
waiting out an async handler before the next assertion.

### `lumenc type <text> [--wait-for <MessageType>]`

Types a string into the currently focused element. Same `[mcp] simulate`
requirement as `click`.

### `lumenc key <name> [--shift] [--ctrl] [--alt] [--super] [--wait-for <MessageType>]`

Injects a single key press (`Enter`, `Tab`, `Escape`, `a`, ...) with the
given modifiers held. Same `[mcp] simulate` requirement as `click`.

### `lumenc scroll <x> <y> <dx> <dy> [--wait-for <MessageType>]`

Injects a wheel event of `(dx, dy)` pixels at a logical-pixel point. Same
`[mcp] simulate` requirement as `click`.

### `lumenc lint`

Three unrelated checks live under this name:

- `lumenc lint` (no flags beyond `--json`) - fetches the running app's
  snapshot-derived findings over MCP: one line per issue, non-zero exit if
  any is error-severity.
- `lumenc lint --css-cascade [<dir>]` - an offline, no-running-app static
  check that parses `<dir>/main.css` and flags every rule whose resolved
  value would differ between the old first-declaration-wins ordering and
  the CSS Cascade-5 last-wins ordering Lumen now uses. Non-zero exit on
  any divergence.
- `lumenc lint --signals [<app-dir>] [--strict]` - an offline static check
  over `<app-dir>/main.lmn` and the app's script (`main.cdl`, `main.rhai`,
  or `main.lua`) against the optional `[signals]` schema in `lumen.toml`.
  Flags untyped `signal_set` writes
  that should use a typed setter, bare `{name}` interpolation that should
  be `{$name}`, a write whose value type disagrees with the declared
  schema type, a markup binding with no matching write or schema entry,
  and a script write nothing reads. `--strict` upgrades warnings to
  errors. This scan is substring-based, not a full parse, so it can
  misread a signal name that appears inside a comment or an unrelated
  string literal.

### `lumenc diff [tick]`

Shows entity ids added, removed, or changed since the given tick (or since
the previous tick, if omitted).

### `lumenc screenshot [out.png] [--highlight id1,id2,...] [--lint] [--bounds map.json]`

Captures the running app to a PNG on disk (never through stdout, so the
image bytes never land in a calling agent's context). `--highlight` draws
outlines around the listed entity ids; `--lint` outlines every current
lint finding instead. `--bounds` additionally writes a JSON map of entity
bounds alongside the image.

## Internationalization

### `lumenc i18n extract <app_dir> [--lang en-US]`

Scans every `.lmn` and `.rhai` file under `app_dir` for translation call
sites and writes or merges the keys it finds into
`<app_dir>/locale/<lang>.ftl`, a Fluent catalogue. It recognises three
shapes: the `t!(i18n, "key", ...)` and `tr!(i18n, "key", ...)` Rust macros,
a `lumen.tr("key", ...)` script call, and the markup attribute
`translatable="key"`.

Re-running is safe. Entries already in the target file are left untouched,
and only newly discovered keys are appended, each with a placeholder value
for a translator to replace.

The extractor is the only part of translation that is wired today: nothing
loads the catalogue back at runtime, no script host registers a `tr`
function, and `translatable="key"` is inert markup that only the scan
reads. Treat this command as a way to start collecting keys, not as a
working localization pipeline. Candela and Lua sources are not scanned at
all.

## Update checks

The commands you type yourself (`run`, `check`, `build`, `bundle`, `new`,
`fmt`, `i18n`) look for a newer release at most once a day and print one line
on stderr when they find one, followed on a terminal by an `Update now? [y/N]`
prompt that runs the installer for you. The automation subcommands, `--help`,
`--version`, and any `--headless` run stay silent.

Set `LUMEN_NO_UPDATE_CHECK` to any value to turn this off; `CI` in the
environment does the same. A `lumenc` built from source never checks, and
neither does one installed with `install.sh --version`. See
[Install](../getting-started/install.md#staying-up-to-date).

## Exit codes

Every command exits `0` on success. A command-line usage error (a missing
argument, an unknown flag) exits `2`; a failure during the operation
itself (a parse error, a lint finding at error severity, a failed MCP
call) exits `1` or another non-zero value. Treat any non-zero exit as
failure in a script or CI job rather than relying on the exact code.
