# Lumen in Neovim

Syntax highlighting from the Lumen tree-sitter grammar, plus diagnostics,
completion, and navigation from `lumen-lsp`.

Needs Neovim 0.11 or newer, `nvim-treesitter`, and a C compiler for building
the grammar.

## Quick start

Copy `lumen.lua` to `~/.config/nvim/lua/lumen.lua` and call it from your
config:

```lua
require('lumen').setup()
```

Then install the parser with `:TSInstall lumen` and open a `.lmn` file.

Working on Lumen itself? Point the grammar at your checkout so `:TSUpdate
lumen` rebuilds from the working tree:

```lua
require('lumen').setup({ grammar_path = '~/src/lumen' })
```

## Options

| Option | Default | Effect |
|--------|---------|--------|
| `grammar_path` | unset | Local clone of the Lumen repository to build the grammar from. Overrides `grammar_url`. |
| `grammar_url` | the Lumen repository | Where to fetch the grammar from. |
| `grammar_revision` | unset | Commit to pin the grammar to. |
| `server_path` | unset | Absolute path to `lumen-lsp`. |
| `treesitter` | `true` | Register the grammar. |
| `lsp` | `true` | Configure and enable the language server. |

With `server_path` unset, the server is looked up the way the VS Code
extension looks it up: `$CARGO_TARGET_DIR`, then the project's `target/`
directory (release before debug), then `lumen-lsp` on `$PATH`.

## Queries

`highlights.scm` and `injections.scm` live in `../../queries`. On the
`nvim-treesitter` main branch they are installed with the parser. On the
master branch, which does not install queries from a grammar repository,
copy them yourself:

```sh
mkdir -p ~/.config/nvim/queries/lumen
cp tools/tree-sitter-lumen/queries/*.scm ~/.config/nvim/queries/lumen/
```

An inline `<script>` body is injected as Rhai, so install the `rhai` parser
to highlight it: `:TSInstall rhai`.

## Without this file

`lsp/lumen_lsp.lua` is the server definition on its own, in the layout
`nvim-lspconfig` uses. Drop it in `~/.config/nvim/lsp/lumen_lsp.lua` and
enable it with `vim.lsp.enable('lumen_lsp')` if you would rather configure
the grammar yourself.
