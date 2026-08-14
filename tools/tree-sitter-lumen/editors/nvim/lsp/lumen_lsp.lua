---@brief
---
--- https://github.com/lumen-fx/lumen
---
--- Language server for Lumen apps: markup (`.lmn`), Lumen CSS, and Rhai
--- scripts. It calls lumenc's own parser, so the editor and the compiler
--- agree on what counts as valid.
---
--- Install it with `cargo install --git https://github.com/lumen-fx/lumen
--- lumen-lsp`, or build it from a checkout with `cargo build --release -p
--- lumen-lsp`.

---@type vim.lsp.Config
return {
  cmd = { 'lumen-lsp' },
  filetypes = { 'lumen' },
  root_markers = { 'lumen.toml', 'main.lmn', '.git' },
}
