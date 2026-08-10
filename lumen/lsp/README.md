# lumen-lsp

`tower-lsp` 0.20 language server for a Lumen app directory: `.lmn` markup,
Lumen CSS in `.css`, and `.rhai` scripts. It calls `lumenc`'s own parser and a
fully-registered Rhai engine, so the editor and the compiler always agree on
what counts as valid.

## What it does

- **Diagnostics**: re-parses the document on every open and change.
  - `.lmn`: parse errors from `lumenc::parse_html`, plus every parse-time lint
    finding as its own diagnostic at its severity.
  - `.css`: parse errors from `lumenc::parse_css`, plus apply-time warnings for
    unknown properties and unparseable values. The sibling markup is used as
    the scratch tree so selector matching is realistic.
  - `.rhai`: compile errors from a Rhai engine with every Lumen builtin
    registered, so `signal`, `derive`, and `on` never read as unknown
    functions. The optimizer is off during analysis, so no builtin can run as a
    side effect of opening a file.
- **Completion**: tag names after `<`, attribute names inside an open tag, and
  values for the attributes with a fixed vocabulary (`flex`, `align`,
  `justify`, `position`, `overflow`, `wrap`, `fit`, and the boolean
  attributes). In `.rhai`, the Lumen builtins, plus the ids declared in the
  sibling markup when completing an id argument.
- **Hover**: markdown documentation for tags and attribute names, and builtin
  signatures in `.rhai`.
- **Signature help**: parameter hints for the Rhai builtins.
- **Goto-definition**: from a template use site to its `<template name="...">`,
  and from an id string in `.rhai` to the element that declares it.
- **References and rename**: every `id="X"` in markup, `"X"` string literal in
  Rhai, and `#X` selector in CSS that names the same id.
- **Document symbols**: the markup element tree, and the function list of a
  `.rhai` file.
- **Formatting**: whole-document formatting of `.lmn` files.

## File extensions

Lumen markup uses `.lmn`. Opening it as HTML hands it to the editor's built-in
HTML language server, which floods the Problems panel with false positives
because it does not know the Lumen vocabulary.

Stylesheets are ordinary `.css` files that use the Lumen property subset, and
scripts are `.rhai`.

## Install

### Build the server

```sh
cargo build --release -p lumen-lsp
# binary lands at <cargo-target>/release/lumen-lsp
```

Put `lumen-lsp` on your `$PATH`, or set `"lumen.serverPath"` in VS Code
settings to the absolute path.

### Sideload the VS Code extension

```sh
cd tools/vscode-lumen
npm install
npm run compile
```

Then symlink (or copy) the directory into your user extensions folder:

- Linux / macOS: `ln -s "$PWD" ~/.vscode/extensions/lumen-vscode`
- Windows: copy the directory into `%USERPROFILE%\.vscode\extensions\lumen-vscode\`

Restart VS Code. The extension registers `.lmn` as the `lumen-markup` language
and starts the server, so opening any `.lmn` file shows "Lumen Markup" in the
status bar and populates the Problems panel from `lumen-lsp`. See
[`tools/vscode-lumen/README.md`](../../tools/vscode-lumen/README.md) for the
rest of the extension.

## Manual smoke test (no VS Code)

The server speaks LSP over stdio:

```sh
cargo build --release -p lumen-lsp
# then send framed JSON-RPC at <cargo-target>/release/lumen-lsp
```

An `initialize` then `didOpen` exchange with a broken file produces
diagnostics like:

```json
{
  "method": "textDocument/publishDiagnostics",
  "params": {
    "diagnostics": [{
      "message": "Unknown tag `<nope>`. See LSP completion list for the full set.",
      "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 7}},
      "severity": 1,
      "source": "lumen-lsp"
    }],
    "uri": "file:///tmp/x.lmn"
  }
}
```

## Known gaps

- CSS gets diagnostics but no completion or hover, so property names and
  `var(--token)` names are not suggested.
- candela (`.cdl`) and Lua (`.lua`) scripts get no intelligence; only `.rhai`
  is analysed.
- `<script>` bodies inside `.lmn` are not linted. Rhai diagnostics run for
  standalone `.rhai` files only.
- `parse_html` stops at the first error, so one markup parse error surfaces per
  save. Lint findings are not affected and all surface at once.
- Only whole-document formatting is offered; "Format Selection" does nothing.
- The tag and attribute vocabulary in `src/docs.rs` is written by hand and is
  behind the parser. `a`, `textarea`, `switch`, `tooltip`, `tabs`, `tab`,
  `dropdown`, `option`, `menu`, `menuitem`, `separator`, `date-picker`, and
  `time-picker` parse fine but are not offered in completion or hover.
  Extending the markup vocabulary means updating `docs.rs` in the same change.
