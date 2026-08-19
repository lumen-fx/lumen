# Tooling reference

Three tools ship alongside `lumenc`: an in-window devtools overlay, a language
server for editors, and an MCP server that lets an agent inspect and drive a
running app.

## Devtools

A panel docked to the right edge of the window, showing the live element
tree, signal values, and captured network requests. While it is open the
app reflows into the remaining width, like a browser's docked devtools; it
never covers the app.

### Availability

Every default `lumenc` build carries the overlay; a
`--no-default-features` compiler build drops it with the rest of the
runtime. It mounts only for a run from source. An app started from a
precompiled `.lmna` artifact has no overlay, and packaged apps never
contain it.

### Opening it

| Trigger | Effect |
|---------|--------|
| `F12` | Toggles the overlay. |
| `LUMEN_DEVTOOLS_OPEN` | Any value other than empty or `0` opens the overlay at startup. |

There is no config key and no CLI flag.

### Panels

Switch panels by clicking a tab. There is no keyboard shortcut for switching.

| Tab | Contents |
|-----|----------|
| Elements | Live element tree, one row per entity, syntax-colored the way a browser colors markup: tag, `#id.class`, `[WxH]`, and `:hover`/`:focus`/`:press` state each in their own color. Hovering a row overlays that element in the app with a tinted box and a `<tag>#id WxH` chip; clicking selects it and opens the inspect pane. The panel's own entities are excluded. Capped at 400 rows. |
| Signals | Frame number, tick time, entity count, then one `name = value (kind)` row per global signal. |
| Network | HTTP exchanges captured from script `fetch()` calls, oldest first: status, method, URL, and tag. Holds the last 128. |

The Pick button arms hover-to-inspect on the app itself: the element under
the pointer is overlaid, and clicking it selects it in the tree. The click
that picks still reaches the app.

### Editing the running app

The inspect pane under the tree shows the selected element's box, layout
style, fill, and text facts, and edits the element in place:

| Control | Effect |
|---------|--------|
| text field + Apply | Replaces the element's text content and re-lays it out. Only elements that already show text take the edit. |
| Hide | Toggles the element's visibility. |
| Delete | Despawns the element and its subtree. |

Edits change the live world only - source files are untouched, and a
hot-reload or a script that rewrites the same element overwrites them.

The Elements panel needs the snapshot pipeline, which the introspection server
provides. With that server off the panel says so instead of showing a tree.

## Language server

`lumen-lsp` provides completion, diagnostics, navigation, and formatting for
Lumen projects.

### Running it

The binary is `lumen-lsp`. It speaks LSP over stdio, takes no arguments, and
logs to stderr. Point any LSP-capable editor at it as a stdio server.

It treats files by extension: `.lmn` as markup, `.css` as stylesheets, and
`.rhai` as script. It discovers the rest of a project from the sibling files in
the same directory, preferring `main.lmn`, `main.css`, and `main.rhai`, which
is what lets it resolve an id in a script back to the element that defines it.

### What it provides

| Feature | Markup (`.lmn`) | CSS (`.css`) | Script (`.rhai`) |
|---------|-----------------|--------------|------------------|
| Diagnostics | Parse errors and every lint finding, with severity | Parse errors and cascade warnings, ranged at the property | Script compile errors |
| Completion | Tag names, attribute names, and constrained values for the attributes that take a fixed set | none | Builtin functions with signature, documentation, and snippet; element ids inside an id argument |
| Signature help | none | none | Builtin signatures |
| Hover | Documentation for the tag or attribute under the cursor | none | Documentation for the builtin under the cursor |
| Go to definition | A tag resolves to its `<template name="...">` in the same file | none | An id string resolves to the element that defines it, across files |
| Find references | An id resolves to every use across markup, CSS, and script | `#id` selectors participate | id arguments participate |
| Rename | Rewrites the id in all three file kinds | same | same |
| Document symbols | Elements | none | Functions |
| Formatting | Whole-document reformat | none | none |

Completion triggers on `<`, space, `"`, and `.`; signature help on `(` and
`,`.

The markup, CSS, and cross-file id features do not depend on a script language.
The `.rhai` column is the `lang-rhai` build feature, which is on by default; a
server built without it keeps every other column and answers script requests
with nothing. candela (`.cdl`) and Lua (`.lua`) buffers have no intelligence
yet.

### VS Code extension

The extension lives in `tools/vscode-lumen`. Build it with `npm install &&
npm run compile`, package it with `vsce package`, and install the resulting
`.vsix`.

It activates on a `.lmn`, `.rhai`, or Lumen CSS file, and on any workspace
containing a `.lmn` file or a `lumen.toml`. It contributes syntax highlighting
for markup (with embedded script and CSS), for Lumen CSS, and for Rhai. A
`.css` file is treated as Lumen CSS when it sits next to a `main.lmn`.

Commands, all under the `Lumen` category:

| Command | Action |
|---------|--------|
| Run App | `lumenc run` on the workspace app |
| Check (parse gate) | `lumenc check` |
| Format Markup File | Formats the active `.lmn` |
| Build AOT Artifact (.lmna) | `lumenc build` |
| New App from Template | Scaffolds with `lumenc new` |
| Open Live Preview | Headless run plus screenshot, shown in a panel |
| Restart Language Server | Restarts `lumen-lsp` |

Open Live Preview is bound to `Ctrl+Shift+V` (`Cmd+Shift+V` on macOS) in a
markup file. It drives the app through the introspection server, so the app
needs `[mcp] simulate = true` in `lumen.toml`; without it the preview has
nothing to capture.

Settings:

| Setting | Default | Effect |
|---------|---------|--------|
| `lumen.serverPath` | unset | Explicit path to `lumen-lsp`. |
| `lumen.lumencPath` | unset | Explicit path to `lumenc`. |
| `lumen.serverAutoDiscover` | `true` | Searches for a built server before falling back to `PATH`. |
| `lumen.run.flags` | `[]` | Extra flags passed to `lumenc run`. |
| `lumen.run.headless` | `false` | Runs the app headless. |
| `lumen.preview.size` | `"960x720"` | Preview viewport. |
| `lumen.preview.dpr` | `1` | Preview device pixel ratio. |
| `lumen.trace.server` | `"off"` | LSP trace verbosity: `off`, `messages`, or `verbose`. |

With auto-discovery on, the extension looks for the server in
`$CARGO_TARGET_DIR`, then in each workspace folder's `target/` directory
(release before debug), then for `lumen-lsp` on `PATH`.

### JetBrains plugin

The plugin lives in `tools/jetbrains-lumen`. Build it with `./gradlew
buildPlugin`, then install the zip from `build/distributions/` through
Settings | Plugins | Install Plugin from Disk. It works in every IntelliJ-based
IDE from 2024.2 on, Community editions included.

It needs [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij), the LSP
client it talks to `lumen-lsp` through. Install that from the Marketplace
first.

The plugin highlights `.lmn` (with embedded script and CSS) and `.rhai` using
the same TextMate grammars as the VS Code extension. Every other feature is the
server's: diagnostics, completion, hover, signature help, go to definition,
find usages, rename, structure view, and reformatting a `.lmn` file. A `.css`
file reaches the server when it sits next to a `main.lmn`. Server status and
the LSP traffic are in View | Tool Windows | Language Servers.

Settings, under Settings | Languages & Frameworks | Lumen:

| Setting | Default | Effect |
|---------|---------|--------|
| Path to `lumen-lsp` | unset | Explicit path to the server binary. |
| Look for a locally built server | on | Searches `$CARGO_TARGET_DIR` and the project's `target/` directories (release before debug) before falling back to `PATH`. |

Changing either setting restarts the server.

The plugin has no `lumenc` commands and no live preview.

### Neovim, Helix, and Zed

These three editors highlight `.lmn` from a tree-sitter grammar, which lives
in `tools/tree-sitter-lumen` with its queries. Highlighting covers tags,
attributes, `{interpolation}` placeholders and their `$signal` and `row.field`
reference forms, comments, and entity references; `<for>`, `<if>`,
`<template>`, `<use>`, `<slot>`, and `<include>` read as keywords, and
`bind-*` and `on-*` are distinguished from plain attributes. An inline
`<script>` body is injected as Rhai, so install a Rhai grammar to highlight
it. A `<script src="...">` file is opened as its own language, which is where
Lua and candela scripts belong.

The generated parser is committed, so no editor needs the tree-sitter CLI.
Every setup below pins a revision of the grammar directory; update that pin
after the grammar changes.

**Neovim** (0.11 or newer, with `nvim-treesitter`): copy
`tools/tree-sitter-lumen/editors/nvim/lumen.lua` to
`~/.config/nvim/lua/lumen.lua` and call `require('lumen').setup()`. It
registers `.lmn` as the `lumen` filetype, registers the grammar so
`:TSInstall lumen` builds it, and enables `lumen-lsp`. The server is found the
way the VS Code extension finds it: `$CARGO_TARGET_DIR`, then the project's
`target/` directory, then `PATH`, with the `server_path` option overriding all
of it. Pass `grammar_path` to build the grammar from a local Lumen checkout.
The same directory also holds `lsp/lumen_lsp.lua`, the server definition on
its own in the layout `nvim-lspconfig` uses.

**Helix**: append `tools/tree-sitter-lumen/editors/helix/languages.toml` to
`~/.config/helix/languages.toml`, copy the queries into
`~/.config/helix/runtime/queries/lumen/`, then run `hx --grammar fetch` and
`hx --grammar build`. `hx --health lumen` reports what Helix found.
`lumen-lsp` has to be on `PATH`, or named by an absolute path in the
`[language-server.lumen-lsp]` section.

**Zed**: the extension is in `tools/zed-lumen`. Install it from the Extensions
view with "Install Dev Extension", pointed at that directory; Zed builds the
grammar and the extension itself. `lumen-lsp` is taken from `PATH`, or from
`lsp.lumen-lsp.binary.path` in your Zed settings.

## MCP server

A running Lumen app exposes its UI over a local JSON-RPC socket. An agent
reaches it through `lumen-mcp-server`, a bridge that speaks MCP on stdin and
stdout and forwards to the app.

### Enabling it

The server listens on `127.0.0.1` at `[mcp] port`, defaulting to 7878. It runs
by default for a windowed app. A headless app turns it off unless
`[mcp] simulate = true` or `[runtime] mcp = true`. Setting `[mcp] port = 0`
disables it outright.

```toml
[mcp]
port = 7878
simulate = true
```

`simulate` gates input injection only. Reading the UI works either way.

On startup the app prints its port and a ready-to-paste agent configuration
fragment, or `lumenc: MCP server disabled`.

### Connecting an agent

```sh
lumen-mcp-server [--host 127.0.0.1] [--port 7878]
```

`LUMEN_MCP_HOST` and `LUMEN_MCP_PORT` set the same two values. In an MCP
client configuration:

```json
{
  "mcpServers": {
    "lumen": {
      "command": "lumen-mcp-server",
      "args": ["--host", "127.0.0.1", "--port", "7878"]
    }
  }
}
```

The bridge connects lazily, so it registers cleanly with no app running; the
first call then reports that nothing is listening. Start the app, and
subsequent calls work without restarting the agent.

For one-off queries from a shell, `lumenc` has a subcommand per operation and
needs no bridge. See [Testing](../guides/testing.md).

### Tools

| Tool | Parameters | Returns |
|------|------------|---------|
| `lumen_tick` | none | Current frame number, and `last_tick_micros`: the last tick's wall-clock duration, covering the whole main schedule plus extract and scene encode on ticks that rendered. |
| `lumen_snapshot_text` | `max_lines` (default 200, capped at 2000), `cursor`, `omit_invisible` (default true) | Indented one-line-per-entity text tree, with a resume cursor when truncated. |
| `lumen_snapshot_tree` | `max_nodes` (default 2000, capped at 10000), `omit_invisible` (default false) | The same tree as nested JSON nodes carrying tag, id, classes, role, label, text, rect, and flags. |
| `lumen_find` | `by_text`, `by_role`, `by_id`, `limit` (default 50, capped at 500) | Matching entities with id, role, label, bounds, and state. |
| `lumen_element_at` | `x`, `y` (required) | The smallest entity containing the point, scroll-corrected, or a miss. |
| `lumen_inspect_entity` | `id` (required) | Every component on that entity. |
| `lumen_list_entities` | none | Every entity id with its component type names. |
| `lumen_list_extracted` | none | The rects and text runs queued for drawing. |
| `lumen_resources` | none | Viewport, pointer position, modifier state, and focus. |
| `lumen_signals` | `filter`, `max` (default 500, capped at 5000) | Signal names with value, kind, generation, and last changed frame. |
| `lumen_set_signal` | `name`, `value` | Writes a signal and waits briefly to confirm the app observed it. |
| `lumen_simulate` | `kind` plus that kind's fields, `wait_for` | Injects input. Requires `[mcp] simulate = true`. |
| `lumen_recent_messages` | `type`, `max` (default 32) | The last N entries of one message ring. |
| `lumen_diff_since` | `tick` | Entity ids added, removed, and changed since that tick. |
| `lumen_lint` | none | UI findings with category, severity, fix hint, and entity. |
| `lumen_screenshot` | `highlight_ids`, `highlight_lint`, `include_bounds_map` | A base64 PNG, its size, and optionally an entity bounds map. |
| `lumen_framework_status` | none | Progress read from the project's `TODO.md`. |

Simulate kinds: `pointer_move`, `pointer_down`, `pointer_up`, `click`, `key`,
`type`, and `scroll`. Point kinds take `x` and `y` in logical pixels, button
kinds also take `button`, `key` takes `key` plus optional `shift`, `ctrl`,
`alt`, and `super` modifiers, `type` takes `text`, and `scroll` takes `dx` and
`dy`. One request is drained per tick, in order.

`wait_for` names a message ring the call blocks on until the app records a
matching event, which is how a driver knows an injected event landed rather
than guessing at a delay.

Message ring names, for `wait_for` and `lumen_recent_messages`:
`PointerMoved`, `PointerPressed`, `PointerReleased`, `ClickEvent`,
`KeyPressed`, `KeyReleased`, `MouseWheel`, `FocusedKey`. Each holds its last
256 entries.

Every result carries a short human-readable `summary`, a `confidence` value,
and a `next_suggested_tools` list, so an agent can chain calls without knowing
the surface in advance.

Snapshots refresh once a second by default. With `[mcp] simulate = true`, and
in a headless run, they refresh every tick so a driver sees each frame.

### Resources and prompts

Through the bridge, an agent can also list and read project files: the
`TODO.md` that roots the project, `lumen.toml`, `main.lmn`, `main.css`, the
example apps, and the docs. Reads are restricted to that catalogue.

These come from the bridge, which runs in your checkout. Talking to the
app's socket directly gets tools only.

Two prompts ship with the bridge:

- `debug-layout-issue` walks the snapshot-first workflow for diagnosing a UI
  bug: lint, then the text tree, then a search, then inspection, with
  screenshots last.
- `add-new-component` walks the conventions for adding a new primitive to the
  framework.
