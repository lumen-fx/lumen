# Lumen VS Code extension - design note

## Principle: intelligence lives in Rust, not TypeScript

The extension is a **thin client**. Every piece of language intelligence -
diagnostics, completion, hover, formatting, symbols, references, rename,
signature help - is served by the Rust `lumen-lsp` (`tower-lsp`) server, which
calls `lumenc`'s real parser (`parse_html` / `parse_css`) and Rhai engine. The
TypeScript here only:

- discovers + spawns the server and reflects its health,
- transports requests via `vscode-languageclient`,
- adds editor glue (commands, a preview panel, scoped CSS retagging).

This is a deliberate Lumen guardrail: the editor can never diverge from the
compiler on what a valid `.lmn` / Lumen-CSS / `.rhai` file is.

## What we mirror from prior art

### rust-analyzer's extension - server discovery, status, settings

- **Binary bootstrap order** (`src/config.ts` `resolveBinary`): explicit
  setting -> workspace `target/{release,debug}/<bin>` (honoring
  `CARGO_TARGET_DIR`) -> bare name on `$PATH`. This mirrors rust-analyzer's
  `bootstrap`/server-path resolution and means a `cargo build -p lumen-lsp`
  "just works" with no configuration.
- **Status-bar item + restart command** (`src/client.ts`): a single managed
  `LanguageClient`, a status entry that shows starting/running/failed, and a
  `Lumen: Restart Language Server` command - the same affordances
  rust-analyzer exposes.
- **Graceful degradation**: if the server can't launch, syntax highlighting
  still works and the user gets an actionable toast + status entry rather than
  a broken silence.

### Slint's extension - the live-preview panel

- **A dedicated `WebviewPanel` opened beside the editor** that renders the UI
  and offers a Refresh control (`src/preview.ts`).

Where Slint and Lumen **differ, Lumen wins**:

- Slint renders through its own in-process interpreter and streams frames into
  the webview. Lumen instead spawns the **real headless runtime**
  (`lumenc run --headless`, which runs layout + GPU render + an MCP TCP server
  with *zero* windows) and pulls frames through the **MCP `lumen.screenshot`
  path** (`lumenc screenshot --app <dir>`). The preview is therefore
  pixel-identical to production headless output - there is no second renderer
  to keep in sync, honoring Lumen's "one runtime" model.
- The webview chrome is styled purely from VS Code theme tokens
  (`var(--vscode-*)`), so it tracks the editor theme - no hardcoded colors,
  consistent with Lumen's token-driven visual guardrail.

## Scoped CSS language

Lumen stylesheets are plain `.css` files that use a Lumen-specific property
subset. Claiming the `.css` extension globally would break unrelated projects,
so the extension registers a `lumen-css` language **without** a file-extension
association and, on document open, retags a `.css` file to `lumen-css` **only**
when it sits in an app's `src` directory, beside the markup. Global CSS is never
touched. The built-in `css` language id is still routed to `lumen-lsp` by
document selector so stylesheet diagnostics work regardless.

## `lumen-lsp` capability gaps (Rust-side follow-ups)

These are intentionally *not* worked around in TypeScript:

- **CSS completion/hover**: the server provides CSS diagnostics but no
  completion or hover for Lumen CSS properties / `var(--token)` names. A
  property + custom-property completion provider belongs in `lumen-lsp`.
- **Single diagnostic per parse**: `parse_html` short-circuits on the first
  error, so only one markup diagnostic surfaces per save. A best-effort/
  recovering parser would let several show at once.
- **`<script>` body linting inside `.lmn`**: Rhai diagnostics run for standalone
  `.rhai` files but not for inline `<script>` blocks in markup.
- **No `document/rangeFormatting`**: only whole-document formatting is
  advertised; range formatting would let "Format Selection" work.
- **AOT subcommand**: the "Build AOT Artifact" command targets `lumenc build`
  (-> `.lmna`), the ahead-of-time compile that bakes parsed + cascaded IR and
  scripts for a parser-free runtime (`lumenc run <dir> --artifact <out>`).
- **Preview needs `[mcp]` enabled**: `lumenc screenshot` requires the app's
  `lumen.toml` to expose the MCP server. Exposing a first-frame one-shot
  screenshot flag on `lumenc run --headless` (no persistent TCP server) would
  make the preview more robust and cheaper.
