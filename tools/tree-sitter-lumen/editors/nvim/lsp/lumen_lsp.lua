---@brief
---
--- https://github.com/lumen-fx/lumen
---
--- Language server for Lumen apps: markup (`.lmn`), Lumen CSS, and Rhai
--- scripts. It calls lumenc's own parser, so the editor and the compiler
--- agree on what counts as valid.
---
--- It ships with the Lumen toolchain beside `lumenc`, so installing Lumen
--- puts it on $PATH; or build it from a checkout with `cargo build --release
--- -p lumen-lsp`.

---@type vim.lsp.Config
return {
  cmd = { 'lumen-lsp' },
  filetypes = { 'lumen' },
  root_markers = { 'lumen.toml', 'main.lmn', '.git' },
}
