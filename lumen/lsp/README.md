# lumen-lsp

`tower-lsp` 0.20 language server for Lumen markup (`.lumen` files).

## What it does

- **Diagnostics**: re-parses the document on every open / change via
  `lumenc::parse_html` and surfaces the resulting `ParseError` as an LSP
  `Diagnostic`. The LSP can never diverge from `lumenc` on what counts as
  a valid Lumen file because it calls the compiler directly.
- **Completion**:
  - Tag position (after `<`): the seven supported tags
    (`root`, `column`, `row`, `scroll`, `tile`, `label`, `div`).
  - Attribute-name position: the seventeen supported attributes.
  - Attribute-value position for the three constrained values:
    `flex` -> `row` / `column`, `scroll` -> `y` / `x` / `both`,
    `draggable` -> `true` / `false`.
- **Hover**: markdown documentation for tags and attribute names
  (hardcoded in `src/docs.rs`; sync by hand if the markup vocabulary
  grows).

## File extension

Lumen markup uses `.lumen`. **Not `.html`** - opening Lumen markup as
HTML triggers VS Code's built-in HTML language server, which floods the
Problems panel with false positives because it doesn't know the Lumen
vocabulary.

## Install

### Build the server

```sh
cargo build --release -p lumen-lsp
# binary lands at <cargo-target>/release/lumen-lsp
```

You can either put `lumen-lsp` on your `$PATH`, or set
`"lumen.serverPath"` in VS Code settings to the absolute path.

### Sideload the VS Code extension

```sh
cd tools/vscode-lumen
npm install
npm run compile
```

Then symlink (or copy) the directory into your user extensions folder:

- Linux / macOS: `ln -s "$PWD" ~/.vscode/extensions/lumen-vscode`
- Windows: copy the directory into `%USERPROFILE%\.vscode\extensions\lumen-vscode\`

Restart VS Code. Opening any `.lumen` file should now show "Lumen Markup"
in the bottom-right status bar and the Problems panel will populate from
`lumen-lsp` rather than from the built-in HTML server.

The workspace ships `.vscode/settings.json` (force-added past the
ignore rule for `.vscode/`) mapping `*.lumen` to the `lumen-markup`
language id, so the extension's contributions take effect.

## Manual smoke test (no VS Code)

The server speaks LSP over stdio:

```sh
cargo build --release -p lumen-lsp
# then send framed JSON-RPC at $TARGET/release/lumen-lsp
```

A canonical `initialize` -> `didOpen` exchange with a broken file
produces diagnostics like:

```json
{
  "method": "textDocument/publishDiagnostics",
  "params": {
    "diagnostics": [{
      "message": "Unknown tag `<nope>`. Allowed: root, column, row, scroll, tile, label, div, script.",
      "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 7}},
      "severity": 1,
      "source": "lumen-lsp"
    }],
    "uri": "file:///tmp/x.lumen"
  }
}
```

## Known gaps

- `<script>` body contents are not linted. Rhai/JS diagnostics are
  future scope.
- Inline CSS (the future `<style>` block, or external `.css` files)
  is not validated by the LSP; `lumenc::parse_css` is not invoked.
- `parse_html` short-circuits on the first error, so only one diagnostic
  is surfaced per save. A future "best-effort parser" pass could collect
  several at once.
- Completion does not yet offer value snippets for length-typed
  attributes (`width`, `height`, `padding`, `margin`) or color-typed
  attributes (`bg`, `text-color`, `hover-bg`).
- No goto-definition for `id="..."` references yet.
- TextMate grammar is minimal - covers tag/attribute/string coloring,
  not value validation (the LSP does that).
