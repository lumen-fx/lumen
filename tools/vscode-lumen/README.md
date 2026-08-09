# Lumen - VS Code extension

First-class editor support for the [Lumen](https://github.com/lumen-fx/lumen)
UI framework: `.lmn` markup, Lumen CSS, and `.rhai` scripts, plus one-click
`lumenc` workflows and an in-editor **headless live preview**.

## Features

### Language intelligence (via `lumen-lsp`)

All diagnostics/completion/hover/formatting come from the Rust `lumen-lsp`
server - the extension only transports them, so the editor never disagrees
with the compiler.

- **`lumen-markup`** (`.lmn`) - parse diagnostics + lint findings,
  tag/attribute completion, hover docs, template goto-definition, id
  references/rename, document symbols, and formatting.
- **`lumen-rhai`** (`.rhai`) - builtin-aware diagnostics (compiled with a real
  Lumen engine so `signal` / `derive` / `on` are never flagged), builtin
  completion + hover + signature help, and cross-file id lookups against the
  sibling markup.
- **Lumen CSS** - stylesheet parse errors + apply-time property warnings.

### Syntax highlighting

TextMate grammars ship for:

- `.lmn` - HTML-like tags, `bind-*` / `on-*` binding attributes,
  `<for>` / `<if>` / `<template>` control-flow tags, `{signal}` / `{$signal}`
  interpolation, embedded `<script>` (Rhai) and `<style>` (Lumen CSS) blocks.
- **Lumen CSS** - selectors (class / id / element / pseudo-class), the Lumen
  property vocabulary, `var(--token)`, custom-property declarations, colors,
  and gradients.
- `.rhai` scripts.

Lumen CSS lives in ordinary `.css` files. To avoid hijacking `.css` in
unrelated projects, the extension **only** retags a stylesheet as *Lumen CSS*
when it sits next to a `main.lmn` (i.e. inside a Lumen app directory).

### Commands (Command Palette -> "Lumen: ...")

| Command | Runs |
| --- | --- |
| **Run App** | `lumenc run <dir>` (`+ --headless` / flags from settings) |
| **Check (parse gate)** | `lumenc check <dir>` |
| **Format Markup File** | `lumenc fmt <file>` |
| **Build AOT Artifact (.lmna)** | `lumenc build <dir> <dir>.lmna` |
| **New App from Template** | `lumenc new <template> <name>` (template gallery quick-pick) |
| **Open Live Preview** | headless render -> screenshot -> webview (see below) |
| **Restart Language Server** | restarts `lumen-lsp` |

The app directory is resolved from the active file (nearest ancestor with a
`main.lmn`), the single workspace folder, or a quick-pick of discovered apps.

### Live Preview (the differentiator)

`Lumen: Open Live Preview` (also the editor-title play button, and
<kbd>Ctrl/Cmd+Shift+V</kbd> on a `.lmn`) renders your app **without ever
opening a window**:

1. spawns `lumenc run <dir> --headless --size ... --dpr ...` - headless runs the
   full pipeline (layout + GPU render + MCP server) with **zero** windows, then
   idles;
2. drives `lumenc screenshot --app <dir>` over the app's MCP TCP server;
3. shows the PNG in a themed preview tab. **Refresh** re-captures the still-
   running app; closing the tab kills the headless process.

Because the preview reuses the real headless runtime + MCP screenshot path, it
is pixel-identical to `lumenc run --headless` - there is no second renderer to
keep in sync.

## Build

```sh
npm install
npm run compile      # -> out/extension.js
```

## Package (.vsix)

`@vscode/vsce` is a dev-dependency:

```sh
npm run package      # -> lumen-vscode-<version>.vsix
```

Install with **Extensions -> ... -> Install from VSIX**, or
`code --install-extension lumen-vscode-<version>.vsix`. This is a local/dev
flow - the extension is not published to the Marketplace.

To sideload during development, symlink this folder into your extensions dir:

```sh
ln -s "$PWD" ~/.vscode/extensions/lumen-vscode   # Linux/macOS
```

## Settings

| Setting | Purpose |
| --- | --- |
| `lumen.serverPath` | Absolute path to `lumen-lsp`. Empty -> auto-discover then `$PATH`. |
| `lumen.lumencPath` | Absolute path to `lumenc`. Empty -> auto-discover then `$PATH`. |
| `lumen.serverAutoDiscover` | Probe `target/{release,debug}/` (honors `CARGO_TARGET_DIR`) before `$PATH`. |
| `lumen.run.flags` | Extra flags appended to `lumenc run`. |
| `lumen.run.headless` | Append `--headless` to `lumenc run`. |
| `lumen.preview.size` | Logical viewport `WxH` for the preview render. |
| `lumen.preview.dpr` | Device-pixel-ratio for the preview render. |
| `lumen.trace.server` | LSP trace verbosity (`off` / `messages` / `verbose`). |

Build the server & CLI with:

```sh
cargo build -p lumen-lsp -p lumenc   # binaries land in target/{debug,release}
```

If `lumen-lsp` can't be found, syntax highlighting still works and a status-bar
item plus a toast point you at `lumen.serverPath`.

See [`DESIGN.md`](DESIGN.md) for the architecture and the Slint /
rust-analyzer inspirations.
