# Lumen for Zed

[![tools](https://github.com/lumen-fx/lumen/actions/workflows/tools.yml/badge.svg)](https://github.com/lumen-fx/lumen/actions/workflows/tools.yml)
[![docs](https://img.shields.io/badge/docs-reference%2Ftooling-blue)](https://docs.lumenfx.dev/reference/tooling/)
[![license](https://img.shields.io/badge/license-MPL--2.0-blue)](https://github.com/lumen-fx/lumen/blob/main/LICENSE)

Zed extension for Lumen apps: `.lmn` files get syntax highlighting from the
Lumen tree-sitter grammar, and `lumen-lsp` supplies diagnostics, completion,
hover, navigation, and formatting. Control-flow and composition tags read as
keywords, and `bind-*`/`on-*` attributes are distinguished from plain ones.
An inline `<script>` body is injected as Rhai; install a Rhai extension to
highlight it, and keep Lua and candela scripts in `<script src="...">`
files, which Zed opens as their own language.

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

## Grammar

The grammar lives at `tools/tree-sitter-lumen` in this repository, and
`extension.toml` pins the commit it is built from. Bump `rev` there after
changing the grammar, otherwise Zed keeps building the old parser.
