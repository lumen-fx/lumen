# Lumen for Zed

Zed extension for Lumen apps: `.lmn` files get syntax highlighting from the
Lumen tree-sitter grammar, and `lumen-lsp` supplies diagnostics, completion,
hover, navigation, and formatting.

## Install

The extension is not in Zed's registry yet, so install it from this
directory: open the Extensions view, choose "Install Dev Extension", and pick
`tools/zed-lumen`. Zed clones the grammar, compiles it, and builds the
extension itself, so a Rust toolchain and a C compiler have to be available.

`lumen-lsp` has to be on `$PATH`:

```sh
cargo build --release -p lumen-lsp
cp target/release/lumen-lsp ~/.local/bin/
```

To use a server elsewhere, point Zed at it in settings:

```json
{
  "lsp": {
    "lumen-lsp": {
      "binary": {
        "path": "/absolute/path/to/lumen-lsp"
      }
    }
  }
}
```

## What you get

Highlighting covers tags, attributes, `{interpolation}` placeholders and
their `$signal` and `row.field` reference forms, comments, and entity
references. Control-flow tags (`<for>`, `<if>`) and composition tags
(`<template>`, `<use>`, `<slot>`, `<include>`) read as keywords, and reactive
attributes (`bind-*`, `on-*`) are distinguished from plain ones.

An inline `<script>` body is injected as Rhai. Install a Rhai extension to
highlight it; without one the body shows as plain text. Keep Lua and candela
scripts in `<script src="...">` files, which Zed opens as their own language.

## Grammar

The grammar lives at `tools/tree-sitter-lumen` in this repository, and
`extension.toml` pins the commit it is built from. Bump `rev` there after
changing the grammar, otherwise Zed keeps building the old parser.
