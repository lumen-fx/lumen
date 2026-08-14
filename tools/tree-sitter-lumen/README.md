# tree-sitter-lumen

Tree-sitter grammar for Lumen markup (`.lmn`).

Editors use it for syntax highlighting, structural selection, and bracket
matching, and it is what the Neovim, Helix, and Zed integrations are built on.
It describes the shape of the markup: elements, attributes, text,
`{interpolation}` placeholders, comments, and `<script>` bodies. Whether a tag
or an attribute actually exists is the compiler's business, and `lumen-lsp`
reports that from lumenc's own parser.

The generated parser is committed, so an editor builds it without the
tree-sitter CLI. Queries live in [`queries`](queries): `highlights.scm` and
`injections.scm`. The Zed extension is separate, at
[`../zed-lumen`](../zed-lumen).

## Neovim

Needs Neovim 0.11 or newer, `nvim-treesitter`, and a C compiler. Copy
`editors/nvim/lumen.lua` to `~/.config/nvim/lua/lumen.lua`, call
`require('lumen').setup()` from your config, install the parser with
`:TSInstall lumen`, and open a `.lmn` file.

`setup()` takes `grammar_path` (build the grammar from a local Lumen
checkout instead of fetching it), `grammar_url` and `grammar_revision`
(where and what to fetch), `server_path` (absolute path to `lumen-lsp`),
and `treesitter`/`lsp` toggles to skip either half. With `server_path`
unset the server is found the way the VS Code extension finds it:
`$CARGO_TARGET_DIR`, then the project's `target/` directory, then `$PATH`.

On the `nvim-treesitter` main branch the queries install with the parser;
on master, copy them yourself:

```sh
mkdir -p ~/.config/nvim/queries/lumen
cp tools/tree-sitter-lumen/queries/*.scm ~/.config/nvim/queries/lumen/
```

`editors/nvim/lsp/lumen_lsp.lua` is the server definition on its own, in
the layout `nvim-lspconfig` uses; drop it in `~/.config/nvim/lsp/` and
enable it with `vim.lsp.enable('lumen_lsp')` to configure the grammar some
other way.

## Helix

Append `editors/helix/languages.toml` to `~/.config/helix/languages.toml`,
copy the queries into `~/.config/helix/runtime/queries/lumen/`, then run
`hx --grammar fetch` and `hx --grammar build`. `hx --health lumen` reports
what Helix found. `lumen-lsp` has to be on `$PATH`, or named by an absolute
path in the `[language-server.lumen-lsp]` section.

Both editors highlight an inline `<script>` body as Rhai when a `rhai`
grammar is installed; without one the body shows as plain text.

## What it parses

Elements and self-closing elements, attributes with double- or single-quoted
values, entity references, comments, and processing instructions.
`{interpolation}` placeholders in text and in attribute values, including the
`$signal`, `$self.field`, `$parent.field`, `$index`, and `row.field` reference
forms. A `<script>` body is raw text and is injected as Rhai; see the comment
at the top of `queries/injections.scm` for why that is the default and what to
do for Lua and candela.

Markup that lumenc rejects still parses here when its shape is valid XML, so an
unknown tag looks fine to the grammar and is reported by the language server.
CDATA sections and doctype declarations are not handled.

## Working on the grammar

```sh
npx tree-sitter generate --abi 14
npx tree-sitter test
npx tree-sitter parse ../../apps/widget-garden/main.lmn
```

Commit the regenerated `src/` along with `grammar.js`. Corpus cases in
`test/corpus` are taken from the example apps and fixtures, so a construct that
appears in a real `.lmn` file has a test. Update the expectations with
`npx tree-sitter test --update` and read the diff before committing it.

Every consumer pins a revision of this directory: the `[[grammar]]` section for
Helix, `install_info.revision` for Neovim, and `[grammars.lumen] rev` in the
Zed extension. Bump those after a grammar change.
