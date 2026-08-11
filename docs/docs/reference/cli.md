# lumenc command reference

`lumenc` is the Lumen command line tool: it runs apps, checks them, compiles
them ahead of time, scaffolds new ones, and drives a running app for
automation.

```
lumenc <command> [arguments]
lumenc --help
lumenc --version
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success. |
| 1 | The command ran and failed (parse error, I/O error, lint findings, no match). |
| 2 | Usage error: unknown command, missing argument, bad flag value. |

`lumenc --help`, `-h`, and `help` print usage and exit 0. `lumenc --version`
and `-V` print `lumenc <version>` and exit 0. An unknown command prints usage
on stderr and exits 2.

## run

```
lumenc run <dir> [--profile chrome|tracy|stderr]
                 [--headless [--size WxH] [--dpr N] [--ticks N]]
                 [--artifact <file>] [--assets <file.lpak>] [--no-hooks]
```

Runs the app in `<dir>`. The directory must contain `main.lmn` unless
`--artifact` is given; `main.css` is optional.

| Flag | Value | Default | Effect |
|------|-------|---------|--------|
| `--profile` | `chrome`, `tracy`, `stderr` | `[profile] mode`, else off | Installs the tracing profiler. `chrome` writes `lumen-trace.json` in the current directory; `tracy` connects to a running `tracy-profiler`; `stderr` prints per-system spans live. |
| `--headless` | - | off | Runs the whole pipeline (layout, GPU render, scripting, MCP, screenshots) with no window. |
| `--size` | `WxH` | `[window] size`, else `960x720` | Logical viewport size. Requires `--headless`. Zero dimensions are rejected. |
| `--dpr` | positive number | `1.0` | Scales the offscreen render target; screenshot pixels are logical size times dpr. Requires `--headless`. |
| `--ticks` | integer | unbounded | Runs exactly N ticks, then exits through the graceful-close path. Requires `--headless`. |
| `--artifact` | path | none | Loads a precompiled `.lmna` artifact instead of parsing source. Disables hot reload. |
| `--assets` | path to a `.lpak` | none | Reads images, icons, and sounds from a `lumenc bundle` archive, keyed by the path relative to `<dir>`. A path the archive does not carry falls back to disk; fonts always come from disk. An unreadable archive exits 1. |
| `--no-hooks` | - | off | Skips the `prebuild` and `prerun` hooks. |

Both `--flag value` and `--flag=value` are accepted for `--profile`,
`--size`, `--dpr`, `--ticks`, `--artifact`, and `--assets`.

Without `--headless`, passing `--size`, `--dpr`, or `--ticks` is a usage
error.

`run` executes the app's `[[hooks]]` entries: every `prebuild` hook, then
every `prerun` hook. See [lumen.toml](lumen-toml.md#hooks).

If the directory is a Rust, C++, or Python SDK app (detected from its
contents, or declared with `[app] kind`), `run` hands off to `cargo`,
`cmake`, or the Python interpreter. Combining a handoff with `--headless`,
`--artifact`, `--assets`, `--size`, `--dpr`, or `--ticks` is a usage error.

`--profile` needs a `lumenc` built with the `profiling` cargo feature, and
`--profile tracy` additionally needs `profiling-tracy`. A default build
reports this and exits 1.

Under `--headless`, SIGINT and SIGTERM (Ctrl+C, Ctrl+Break, or console close
on Windows) exit 0 through the graceful-close path.

## check

```
lumenc check <dir>
```

Parses and validates the app without opening a window and without running
hooks. Prints `<dir>: ok (N elements, script: yes|none)` and exits 0, or
prints the parse error and exits 1. A missing `<dir>` exits 2.

## build

```
lumenc build <app_dir> <out.lmna> [--no-hooks]
```

Compiles the app ahead of time into a `.lmna` artifact: parses the entry
`.lmn` file and `main.css` once, runs the cascade, resolves asset and include
paths, bakes the script source, and records which engine runs each part of it.
Prints the element count, the output path, and the artifact size.

Runs the app's `prebuild` hooks first unless `--no-hooks` is given.

A multi-page app compiles whole: every page goes into the artifact behind the
gate that mounts it, together with the page set navigation resolves against.

An `<app_dir>` that is not a directory, a missing output path, or an extra
positional argument exits 2. For an SDK app the output path is ignored and the
native build tool runs instead.

Run the result with `lumenc run <dir> --artifact <out.lmna>`. See
[Packaging](../guides/packaging.md).

## package

```
lumenc package <app_dir> [<out_dir>] [--name <name>] [--target <target>]
                         [--lib-dir <dir>] [--no-hooks]
```

Assembles a folder that runs on a machine with no Lumen installation: the app
executable, the Lumen runtime library where the app needs one, `lumen.toml`,
and every other file from `<app_dir>` at the same relative path. Dotfiles, the
output directory, and the app's build inputs and build tree are skipped. Prints
one line naming the executable it wrote and how many app files travelled with
it.

For a markup app the executable is the launcher with the compiled app inside
it, and the markup, stylesheet, and scripts are compiled in rather than copied.
A multi-page app packages whole, routing included.

For an SDK app the app's own toolchain builds it first, exactly as `lumenc
build` would, and the folder is assembled around what that produced:

| Kind | Executable | Runtime library | Detected from |
|------|-----------|-----------------|---------------|
| Rust | the binary `cargo build --release` reports | linked in, not copied | `Cargo.toml` depending on `lumen` |
| C++ | the executable in the CMake build tree | copied beside it | `CMakeLists.txt` |
| Python | none; exits 2 saying to ship the directory with the runtime library | - | a `.py` importing `lumen` |

`[app] kind` in `lumen.toml` overrides detection. An SDK app's markup,
stylesheet, and scripts are read at run time rather than compiled in, so those
files travel with it. A C++ build that produced several executables packages
the most recent and names the rest.

| Flag | Value | Default | Effect |
|------|-------|---------|--------|
| `<out_dir>` | path | `<app_dir>/dist/<name>` | Where the folder is written. Created if missing; existing files are overwritten. |
| `--name` | string | the app directory's name | Names the executable, and the default output directory. |
| `--target` | see below | this machine's platform | Packages for another platform. |
| `--lib-dir` | path | none | Directory holding the launcher stub and the runtime library to use, instead of looking them up. |
| `--no-hooks` | - | off | Skips the `prebuild` hooks. |

Targets are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`,
and `windows-x86_64`. Any host can package a markup app for any of them. An
unrecognised name exits 2 and lists the ones that exist, and `--target` with an
SDK app exits 2: cross-compilation belongs to that app's own toolchain.

The launcher stub and the runtime library are looked up in this order:
`--lib-dir`, then, for this machine's own platform, the directory holding the
running `lumenc` and then `$LUMEN_LIB_DIR`, then the download cache. When none
of them has both files and the target is another platform, the release matching
this `lumenc` version is downloaded, checked against the `sha256sums.txt`
published with it, and unpacked into the cache; a release that publishes no
checksum for the archive, or no launcher in it, exits 1 rather than installing
anything. Set `LUMEN_GH_REPO` to fetch from a different repository. When
nothing can be found and nothing can be fetched, the error names every
directory it looked in.

Windows and Linux packages carry the compiled app appended to the executable. A
macOS package built on macOS links it in as a Mach-O section, which needs `cc`
from the Xcode Command Line Tools; a macOS package built anywhere else ships it
as `<name>.lmna` beside the executable instead.

Runs the app's `prebuild` hooks first unless `--no-hooks` is given. An
`<app_dir>` that is not a directory, an extra positional argument, or an output
directory that is the app directory itself, exits 2. A failed build, or a build
that produced no executable, exits 1.

A packaged app accepts `--headless [--ticks N]`, which runs it window-free for
N ticks and exits; every other argument is left to the app.

## bundle

```
lumenc bundle <app_dir> <out.lpak> [--no-hooks]
lumenc bundle --static <app_dir> <out_dir> [--no-hooks]
```

Without `--static`, packs every regular file under `<app_dir>` into a single
`.lpak` archive, skipping dotfiles and `target/` directories, and prints the
file count. Entries are keyed by their path relative to `<app_dir>`. Run
against the archive with `lumenc run <app_dir> --assets <out.lpak>`.

With `--static`, resolves the app's capability set from `[capabilities]` plus
a source scan, maps it to a cargo feature list, builds the trimmed runtime
library with only those subsystems, and copies the result into `<out_dir>`.
It prints each resolved capability and the feature list. This needs the Lumen
source tree; set `LUMEN_WORKSPACE_DIR` to point at it.

Runs the app's `prebuild` hooks first unless `--no-hooks` is given. A missing
argument or an extra positional argument exits 2.

## new

```
lumenc new <name> [template]
lumenc new --list
```

Scaffolds a directory `<name>` from a template. The template argument is
optional and defaults to `blank`. Every template writes `main.lmn`,
`lumen.toml`, and a README.

| Template | Contents |
|----------|----------|
| `blank` | A bare `<root>` and a `lumen.toml`. |
| `hello` | One label and a script. |
| `counter` | Buttons, `bind-text`, per-id click routing. Scripted in candela. |
| `form` | Input, toggle, slider, live status line. |
| `todo` | List, input, `<for>` loop, array signals. |
| `dashboard` | Stat tiles, progress bars, activity feed driven by a timer. |
| `settings` | Checkbox, radio, dropdown, and slider groups with `derive()`. |
| `hotkeys` | Global hotkeys, tray icon, OS notifications. |

`counter` is scripted in candela; the rest are scripted in Rhai.

`--list` (or `-l`) prints the gallery with one-line descriptions and exits 0.
An existing `<name>` exits 1 without writing anything. An unknown template
exits 2 and names the available set.

## fmt

```
lumenc fmt <file> [--check]
```

Reformats a `.lmn` file in place and prints `lumenc fmt: rewrote <file>` when
the bytes changed. With `--check` nothing is written; the command exits 1 when
the file is not formatted and 0 when it is. A missing file argument or an
unknown flag exits 2.

## i18n extract

```
lumenc i18n extract <app_dir> [--lang <tag>]
```

Scans `.lmn`, `.rhai`, `.lua`, and `.cdl` files under `<app_dir>` for
translation keys and writes `<app_dir>/locale/<tag>.ftl`. `--lang` defaults to
`en-US` and also accepts `--lang=<tag>`.

Recognised call shapes: `t("key")` and `tr("key")` (including candela's
`lumen::t("key")`), `t!(i18n, "key", ...)` and `tr!(i18n, "key", ...)`, and
the `translatable="key"` markup attribute. Keys built at runtime are invisible
to the scan.

The extractor is idempotent: existing entries are preserved verbatim and only
new keys are appended, each with a placeholder value. `target`,
`node_modules`, `.git`, and `locale` directories are skipped. The command
prints the total and new key counts.

`lumenc i18n` with no subcommand, or any subcommand other than `extract`,
exits 2.

## Automation commands

These drive an already-running app over its JSON-RPC TCP server. Each opens a
connection, sends one request, prints the reply, and exits. Start the app
first, in another shell or in the background. See
[Testing](../guides/testing.md) for a worked example.

Every command in this group accepts `--port <n>` and `--app <dir>`.

### Port resolution

In order, first match wins:

1. `--port <n>`
2. `LUMEN_MCP_PORT`
3. `[mcp] port` in `<dir>/lumen.toml`, when `--app <dir>` is given
4. `7878`

The connect timeout is one second and the read timeout five seconds. A
connection failure exits 1 with a hint that the app may not be running.

### snapshot

```
lumenc snapshot [--text|--json] [--max-lines N] [--cursor C]
                [--include-invisible] [--port P] [--app <dir>]
```

Prints an accessibility-tree-style text dump of the live UI. `--text` is the
default; `--json` prints the raw result. `--max-lines` truncates and prints a
cursor to resume from; pass it back with `--cursor`. `--include-invisible`
(also spelled `--no-omit-invisible`) keeps entities that are not visible.
Exits 0 on any successful call.

### find

```
lumenc find [--text S] [--role R] [--id N] [--limit N] [--json]
            [--port P] [--app <dir>]
```

Searches the live snapshot. Prints one row per hit: id, role, label, position,
size, state. Exits 1 with `no matches` when nothing matches.

### element-at

```
lumenc element-at <x> <y> [--json] [--port P] [--app <dir>]
```

Prints the topmost entity at the logical-pixel point. Exits 1 on a miss.

### click

```
lumenc click <x> <y> [--button primary|secondary|middle] [--wait-for R]
             [--json] [--port P] [--app <dir>]
```

Injects a click at the logical-pixel point. `--wait-for` names a message ring
to wait on before returning, for example `ClickEvent`. Requires
`[mcp] simulate = true`; without it the command exits 1 and prints the hint.

### type

```
lumenc type <text> [--wait-for R] [--json] [--port P] [--app <dir>]
```

Types a string into the focused entity.

### key

```
lumenc key <name> [--shift] [--ctrl] [--alt] [--super] [--wait-for R]
           [--json] [--port P] [--app <dir>]
```

Injects one key press, for example `Enter`, `Tab`, `Escape`, or `a`. `--cmd`
is an alias for `--super`.

### scroll

```
lumenc scroll <x> <y> <dx> <dy> [--wait-for R] [--json]
              [--port P] [--app <dir>]
```

Injects a wheel event of `(dx, dy)` pixels at the logical-pixel point.

### lint

```
lumenc lint [--json] [--port P] [--app <dir>]
lumenc lint --css-cascade [<dir>] [--json]
lumenc lint --signals [<app-dir>] [--json] [--strict]
```

Plain `lumenc lint` queries the running app and prints one finding per line as
`<severity> <entity> <category>: <hint>`. It exits 1 when any finding has
error severity.

`--css-cascade` is offline: it parses `<dir>/main.css` and reports every rule
whose resolved value differs between first-wins and last-wins cascade
ordering. It exits 1 when it finds any divergence, and 0 when the app has no
`main.css`.

`--signals` is offline: it reads `<app-dir>/main.lmn`, the app script
(`main.cdl`, `main.rhai`, or `main.lua`), and the optional `[signals]` schema.
Findings are printed as `<severity> <file>:<line>:<col> [<kind>] <signal>:
<message>` with an optional hint line. Kinds: `untyped-write`,
`schema-mismatch`, `bare-interpolation`, `untracked-signal`, `orphan-write`.
`--strict` upgrades warnings to errors. Exits 1 when any finding is an error.

Both offline modes take the directory either positionally right after the flag
or via `--app`, and default to `.`.

### diff

```
lumenc diff [tick] [--json] [--port P] [--app <dir>]
```

Prints entity ids added, removed, and changed since `tick`, or since the
previous tick when omitted.

### screenshot

```
lumenc screenshot [out.png] [--highlight id1,id2,...] [--lint]
                  [--bounds map.json] [--port P] [--app <dir>]
```

Captures the app to a PNG, defaulting to `lumen-screenshot.png`.
`--highlight` outlines the listed entity ids; `--lint` outlines every lint
finding. `--bounds` also writes the entity bounds map as JSON. Prints the
output path and pixel size. A non-integer in `--highlight` exits 2; an
unavailable capture exits 1.

## Update check

An installed `lumenc` looks for a newer release at most once a day and prints
one line on stderr when it finds one. On a terminal it then offers to install
it: the shell installer on Linux and macOS, the `.msi` on Windows.

The check runs only for `run`, `check`, `build`, `bundle`, `new`, `fmt`, and
`i18n`. It is skipped when any of these hold:

- The command line contains `--headless`.
- `LUMEN_NO_UPDATE_CHECK` is set to a non-empty value.
- `CI` is set.
- stderr is not a terminal.
- The copy is not an installed one (a build from source has no install
  receipt, and neither does the portable Windows zip).
- The install is pinned, which `install.sh --version` records. An MSI install
  is never pinned.

The check never changes the command's exit code.

## Environment variables

| Variable | Effect |
|----------|--------|
| `LUMEN_MCP_PORT` | Port the automation commands connect to, below `--port` and above `lumen.toml`. |
| `LUMEN_NO_UPDATE_CHECK` | Any non-empty value turns the update check off. |
| `CI` | Turns the update check off. |
| `LUMEN_THREADS` | Worker-thread budget. Overrides `[runtime] threads`. |
| `LUMEN_DEVTOOLS_OPEN` | Any non-empty value other than `0` opens the devtools overlay at startup instead of waiting for F12. |
| `LUMEN_HOT_RELOAD_POLL` | Forces the hot-reload watcher onto mtime polling instead of filesystem events. |
| `LUMEN_FONT_CACHE` | `0`, `off`, `false`, or `no` disables the persistent font-metadata cache and rescans system fonts every launch. |
| `LUMEN_BOOT_TRACE` | Prints a phase-by-phase startup breakdown on stderr. |
| `LUMEN_GPU_INIT_TRACE` | Prints GPU adapter and device selection detail on stderr. |
| `LUMEN_GPU_INIT_DEADLINE_MS` | GPU init deadline in milliseconds; defaults to 5000. Exceeding it aborts with a diagnostic instead of hanging. |
| `LUMEN_TRACE_FRAME_DIRTY` | Logs which source marked each frame dirty. |
| `LUMEN_WORKSPACE_DIR` | Lumen source tree that `bundle --static` builds the trimmed runtime from. |
| `LUMEN_LIB_DIR` | Directory searched for the shared Lumen library and the launcher stub, after the directory holding `lumenc`. |
| `LUMEN_GH_REPO` | Repository, as `owner/name`, that `package --target` fetches another platform's toolchain files from. Defaults to `lumen-fx/lumen`. |
