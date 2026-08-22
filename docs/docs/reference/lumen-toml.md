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
`layout.lmn`, which contributes its `<template>` declarations to every page
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
| `http-fetch` | bool | inferred from a `fetch(` call in the app | Compiles the HTTP client behind the scripts' `fetch()` and `http()` builtins in. Without it both calls report the missing client. |
| `mcp` | bool | `false` | Compiles the introspection server in. Never inferred. |
| `async` | bool | inferred from a file-dialog call in the app | Compiles the async bridge in. File dialogs resolve on it, and on macOS they do not open without it. |

Inference is conservative: a capability is left out only on a reliable
unused signal, and anything ambiguous keeps it in. An explicit value always
wins.

The script host is selected by `[script] engine` or inferred from the app's
script files, not here.

## [web]

What `lumenc web` needs that only a site has. The rest of the file still
describes the app: `[window] title` is the documents' title, `[app]` gives the
entry file and the locale, `[pages]` the page set, `[asset_roots]` where an
asset comes from, and `[script] engine` which engine runs the app's code.

`[capabilities]` does not apply. A site loads one prebuilt runtime that ships
with the toolchain, so there is nothing per-app to compile or trim.

| Key | Type | Default | Effect |
|-----|------|---------|--------|
| `out_dir` | string | `dist/web` | Where the site is written, relative to the app directory unless absolute. |
| `base_path` | string | `/` | URL prefix the site is served under. Every link and asset reference hangs off it. |
| `url` | string | none | Absolute site URL. The canonical link, the social metadata and the sitemap need it; without it they are left out. |
| `description` | string | none | Description any page without one of its own carries. |
| `og_image` | string | none | Image for social previews, relative to the site root or absolute. |
| `canonical` | string | `url` | Absolute URL the pages declare as canonical, for a site published at more than one address. |
| `locales` | array of BCP-47 tags | the app's locale | Emit one document tree per locale. |
| `default_locale` | BCP-47 tag | `[app] locale`, else `en-US` | The locale served from the site root; the others sit under `/<tag>/`. |
| `skin` | string | `[skin] name`, else `default` | Skin the site is styled with. `auto` is not read here: it means the machine's own OS, and a site is served to every OS. |
| `css` | `"sheet"`, `"computed"` | `sheet` | `sheet` emits the stylesheet the app was written with. `computed` writes the values Lumen's cascade resolved onto each element instead, which answers what Lumen resolved but loses states, media queries and anything created later. |
| `widgets` | `"semantic"`, `"verbatim"` | `semantic` | Which shape a widget the parser built out of smaller elements is emitted as. Today both emit the parts. |
| `render` | `"static"`, `"csr"`, `"ssr"` | `csr` | Where a page's document comes from. `csr` writes it, along with the runtime, the compiled app and the manifest the pages load. `static` writes it and nothing to run it. `ssr` writes what a render needs and no documents: a page is produced for the request that asks, by `lumenc web --serve` or by a server built on `lumen-ssr`. Every value writes the whole markup tree, so a reader and a crawler get the same document. |
| `runtime` | bool | what `render` implies | Whether the documents carry the browser runtime. `render = "static"` already means `false` and `render = "csr"` already means `true`, so saying the opposite alongside either is refused, naming the value that means it. `render = "ssr"` is the one that leaves the question open: with `false`, a page is produced for the request that asks and carries no wasm and no boot script, so it is read and never taken over. |
| `prerender` | `"seeds"`, `"run"`, `"none"` | `seeds` | Where the state the pages are rendered with comes from. `seeds` uses `[web.seed]` and the defaults the markup declares; `run` starts from those and then runs the app during the build, writing each page with the state it settles into; `none` renders the markup alone. `run` with `render = "ssr"` is refused: a rendered page settles its own state per request. |
| `hash_assets` | bool | `false` | Add a content hash to asset file names. Not applied yet. |
| `debug_attrs` | bool | `false` | Write the extra `data-lm-*` attributes naming what an element came from. Not written yet. |
| `menubar` | `"omit"`, `"nav"` | `omit` | What an app menu bar becomes in a document. |
| `sitemap` | bool | on when `url` is set | Write `sitemap.xml`. |
| `host` | `"static"`, `"netlify"`, `"vercel"`, `"apache"`, `"nginx"` | `static` | Where the site is deployed. A named host also gets the file that makes it serve a deep path with a 200 (`_redirects`, `vercel.json`, `.htaccess`, `nginx.conf`); `static` relies on the emitted `404.html`, which every host serves. Under `render = "ssr"` no rewrite file is written, because a render answers a deep path itself. |
| `navigation` | `"soft"`, `"hard"` | `soft` | Whether a link to another page of the same site is swapped in place or loaded by the browser. |

```toml
[web]
out_dir   = "dist/web"
base_path = "/"
url  = "https://example.com"
host = "netlify"
locales = ["en-US", "de-DE"]

[web.seed]
count = 3
greeting = "Hello"

[[web.seed.todos]]
id    = "1"
title = "write it down"

[web.pages.settings]
title = "Settings"
description = "Everything you can change"
```

`[web.seed]` gives the signals the pages are rendered with, and the same
values are handed to the runtime so it starts where the page left off. A value
is a string, a number or a boolean. A name written as `[[web.seed.<name>]]`
instead is an array signal: each entry is one row, its keys are the fields a
`<for>` row template reads, and the values are strings. A `<for each="<name>">`
is emitted with those rows in it, and an element bound to a seeded signal with
`bind-text`, `bind-checked`, `bind-value` or `bind-disabled` is emitted showing
that value. A seeded signal beats the default the markup beside the binding
would have set, and a script that publishes the signal itself beats both.
`[web.pages.<key>]` sets one page's `title` and `description`; both fall back
to the site's.

See [the web guide](../guides/web.md).

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

## [[plugins]]

Compiler plugins the app declares. Each entry is one plugin; entries run in
declaration order on every compile path (`run`, `build`, `check`, `package`,
`web`). See [authoring compiler plugins](../contributing/plugins.md#compiler-plugins)
for what a plugin can do.

| Key | Type | Required | Effect |
|-----|------|----------|--------|
| `name` | string | yes | Plugin name; the loaded library must report the same one. |
| `version` | string | one source | Version requirement (cargo semantics: `"1.2"` means `^1.2`), resolved against the plugin cache and pinned in `lumen.lock`. |
| `path` | string | one source | A built cdylib, relative to the app directory (absolute paths work too). Without an extension the platform spellings are probed (`lib<p>.so`, `lib<p>.dylib`, `<p>.dll`, plus the underscored variants cargo produces for a hyphenated name). |
| `config` | table | no | Handed to the plugin verbatim; a key the plugin does not read produces no diagnostic. |

```toml
[[plugins]]
name    = "markdown"
version = "1.2"
config  = { flavor = "gfm" }

[[plugins]]
name = "local-dev"
path = "plugins/local-dev"
```

Declare exactly one source per entry. `git` and `registry` sources are not
supported yet and error saying so.

A `version` source resolves to a prebuilt, per-platform cdylib in the plugin
cache (`~/.lumen/plugins`, `%LOCALAPPDATA%\Programs\Lumen\plugins` on
Windows, `LUMEN_PLUGIN_CACHE` overrides). The resolved version and its
per-platform sha256 are pinned in `lumen.lock` beside `lumen.toml`; commit
that file, and every later build reuses the pinned version and refuses a
cached library whose bytes changed. `lumenc` does not fetch plugins yet; a
version absent from the cache is an error that says so.

Unlike `[[hooks]]`, plugins also run under `lumenc check`, so the tree being
validated is the tree a build produces; emit outputs are discarded there,
though a `version` source may still write `lumen.lock`. A plugin is native
code loaded into the compiler's process, the same trust model as a Cargo
build script.

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
