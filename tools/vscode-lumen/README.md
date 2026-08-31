# Lumen - VS Code extension

[![open vsx](https://img.shields.io/open-vsx/v/lumen-fx/lumen-ui)](https://open-vsx.org/extension/lumen-fx/lumen-ui)
[![tools](https://github.com/lumen-fx/lumen/actions/workflows/tools.yml/badge.svg)](https://github.com/lumen-fx/lumen/actions/workflows/tools.yml)
[![docs](https://img.shields.io/badge/docs-reference%2Ftooling-blue)](https://docs.lumenfx.dev/reference/tooling/)
[![license](https://img.shields.io/badge/license-MPL--2.0-blue)](https://github.com/lumen-fx/lumen/blob/main/LICENSE)

Editor support for the [Lumen](https://github.com/lumen-fx/lumen) UI
framework: `.lmn` markup, Lumen CSS, and `.rhai` scripts, plus one-click
`lumenc` workflows and an in-editor headless live preview.

## Features

### Language intelligence (via `lumen-lsp`)

Diagnostics, completion, hover, and formatting all come from the Rust
`lumen-lsp` server; the extension only transports them, so the editor never
disagrees with the compiler.

- **`lumen-markup`** (`.lmn`) - parse diagnostics and lint findings,
  tag/attribute completion, hover docs, template goto-definition, id
  references and rename, document symbols, and formatting.
- **`lumen-rhai`** (`.rhai`) - builtin-aware diagnostics, compiled with the
  same engine the runtime uses so `signal`, `derive`, and `on` are never
  flagged as unknown functions. Also builtin completion, hover, signature
  help, and cross-file id lookups against the sibling markup.
- **Lumen CSS** - stylesheet parse errors and apply-time property warnings.

See [`crates/dev/lsp/README.md`](../../crates/dev/lsp/README.md) for the full
server surface and its known gaps.

### Syntax highlighting

TextMate grammars ship for:

- `.lmn` - HTML-like tags, `bind-*` and `on-*` binding attributes,
  `<for>` / `<if>` / `<template>` control-flow tags, `{signal}` and `{$signal}`
  interpolation, and embedded `<script>` (Rhai) and `<style>` (Lumen CSS)
  blocks.
- Lumen CSS - selectors (class, id, element, pseudo-class), the Lumen property
  vocabulary, `var(--token)`, custom-property declarations, colors, and
  gradients.
- `.rhai` scripts.

Lumen CSS lives in ordinary `.css` files. To avoid hijacking `.css` in
unrelated projects, the extension retags a stylesheet as Lumen CSS only when it
sits in an app's `src` directory, beside the markup.

### Commands (Command Palette -> "Lumen: ...")

| Command | Runs |
| --- | --- |
| Run App | `lumenc run <dir>` (plus `--headless` and flags from settings) |
| Check (parse gate) | `lumenc check <dir>` |
| Format Markup File | `lumenc fmt <file>` |
| Build AOT Artifact (.lmna) | `lumenc build <dir> <dir>/<name>.lmna` |
| New App from Template | `lumenc new <name> [template]`, with a template quick-pick |
| Open Live Preview | headless render -> screenshot -> webview (see below) |
| Restart Language Server | restarts `lumen-lsp` |

The template quick-pick offers the same gallery as `lumenc new --list`, in the
same order: blank, hello, counter, form, todo, dashboard, settings, hotkeys.
`blank` comes first and is what `lumenc new <name>` scaffolds when you give no
template.

The app directory is resolved from the active file (nearest ancestor holding a
`lumen.toml` or a `src/main.lmn`), the single workspace folder, or a quick-pick
of discovered apps.

### Live preview

`Lumen: Open Live Preview` (also the editor-title play button, and
<kbd>Ctrl/Cmd+Shift+V</kbd> on a `.lmn`) renders your app without opening a
window:

1. spawns `lumenc run <dir> --headless --size ... --dpr ...`, which runs
   layout and GPU rendering with no window, then idles;
2. drives `lumenc screenshot <tmp.png> --app <dir>` over the app's MCP TCP
   server;
3. shows the PNG in a themed preview tab. Refresh re-captures the still-running
   app; closing the tab kills the headless process.

The preview drives the headless runtime and the MCP screenshot path, so it
matches `lumenc run --headless` pixel for pixel; there is no second renderer to
keep in sync.

A headless run leaves the MCP server off unless the app asks for it, so the
preview needs the app's `lumen.toml` to carry an `[mcp]` section with
`simulate = true` (or `[runtime] mcp = true`). Without it the panel reports
that it could not capture a frame.

## Build

```sh
npm install
npm run compile      # -> out/extension.js
```

## Package (.vsix)

`@vscode/vsce` is a dev dependency:

```sh
npm run package      # -> lumen-ui-<version>.vsix
```

Install with Extensions -> ... -> Install from VSIX, or
`code --install-extension lumen-ui-<version>.vsix`. The extension is not
published to the Marketplace.

To sideload during development, symlink this folder into your extensions
directory:

```sh
ln -s "$PWD" ~/.vscode/extensions/lumen-ui   # Linux/macOS
```

## Settings

| Setting | Purpose |
| --- | --- |
| `lumen.serverPath` | Absolute path to `lumen-lsp`. Empty means auto-discover, then `$PATH`. |
| `lumen.lumencPath` | Absolute path to `lumenc`. Empty means auto-discover, then `$PATH`. |
| `lumen.serverAutoDiscover` | Probe `target/{release,debug}/` (honors `CARGO_TARGET_DIR`) before `$PATH`. |
| `lumen.run.flags` | Extra flags appended to `lumenc run`. |
| `lumen.run.headless` | Append `--headless` to `lumenc run`. |
| `lumen.preview.size` | Logical viewport `WxH` for the preview render. |
| `lumen.preview.dpr` | Device-pixel ratio for the preview render. |
| `lumen.trace.server` | LSP trace verbosity (`off`, `messages`, `verbose`). |

Build the server and CLI with:

```sh
cargo build -p lumen-lsp -p lumenc   # binaries land in target/{debug,release}
```

If `lumen-lsp` cannot be found, syntax highlighting still works and a status-bar
item plus a notification point you at `lumen.serverPath`.

See [`DESIGN.md`](DESIGN.md) for the architecture and the Slint and
rust-analyzer inspirations.
