# tree-sitter-lumen

Tree-sitter grammar for Lumen markup (`.lmn`).

Editors use it for syntax highlighting, structural selection, and bracket
matching, and it is what the Neovim, Helix, and Zed integrations are built on.
It describes the shape of the markup: elements, attributes, text,
`{interpolation}` placeholders, comments, and `<script>` bodies. Whether a tag
or an attribute actually exists is the compiler's business, and `lumen-lsp`
reports that from lumenc's own parser.

## Using it

- Neovim: [`editors/nvim`](editors/nvim)
- Helix: [`editors/helix`](editors/helix)
- Zed: [`../zed-lumen`](../zed-lumen)

The generated parser is committed, so an editor builds it without the
tree-sitter CLI. Queries live in [`queries`](queries): `highlights.scm` and
`injections.scm`.

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
