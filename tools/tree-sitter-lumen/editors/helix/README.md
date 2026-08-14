# Lumen in Helix

Syntax highlighting from the Lumen tree-sitter grammar, plus diagnostics,
completion, and navigation from `lumen-lsp`.

## Setup

Append `languages.toml` in this directory to `~/.config/helix/languages.toml`,
then install the queries and build the grammar:

```sh
mkdir -p ~/.config/helix/runtime/queries/lumen
cp tools/tree-sitter-lumen/queries/*.scm ~/.config/helix/runtime/queries/lumen/
hx --grammar fetch
hx --grammar build
```

Open a `.lmn` file and check the result with `:log-open`; `hx --health lumen`
lists the language server and the highlight, indent, and injection queries
Helix found.

`lumen-lsp` has to be on `$PATH`. Build it with `cargo build --release -p
lumen-lsp` and copy the binary somewhere on the path, or set an absolute path
in the `[language-server.lumen-lsp]` section.

An inline `<script>` body is injected as Rhai, which Helix highlights when a
`rhai` grammar is configured. Without one the body shows as plain text.
