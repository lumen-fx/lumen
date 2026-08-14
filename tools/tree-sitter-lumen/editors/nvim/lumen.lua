-- Lumen support for Neovim: `.lmn` filetype detection, the tree-sitter
-- grammar, and the `lumen-lsp` language server.
--
-- Copy this file to ~/.config/nvim/lua/lumen.lua and call:
--
--     require('lumen').setup()
--
-- See README.md in this directory for the options and for the query files.

local M = {}

local defaults = {
  -- Where the grammar comes from. Point `grammar_path` at a local clone of
  -- the Lumen repository to build the grammar from your checkout; otherwise
  -- the repository is fetched, and `grammar_revision` pins the commit that
  -- the installed queries were written against.
  grammar_url = 'https://github.com/lumen-fx/lumen',
  grammar_revision = nil,
  grammar_path = nil,

  -- Absolute path to `lumen-lsp`. Unset means: look in the Cargo target
  -- directories of the current project, then fall back to $PATH.
  server_path = nil,

  treesitter = true,
  lsp = true,
}

local GRAMMAR_LOCATION = 'tools/tree-sitter-lumen'
local GRAMMAR_QUERIES = 'tools/tree-sitter-lumen/queries'

local function is_file(path)
  local stat = vim.uv.fs_stat(path)
  return stat ~= nil and stat.type == 'file'
end

--- Resolve the `lumen-lsp` command, mirroring the VS Code extension: an
--- explicit path wins, then a locally built binary under a Cargo target
--- directory (release before debug), then the bare name on $PATH.
---@param opts table
---@return string
function M.server_command(opts)
  opts = vim.tbl_extend('force', defaults, opts or {})
  if opts.server_path and opts.server_path ~= '' then
    return vim.fn.expand(opts.server_path)
  end

  local exe = vim.fn.has('win32') == 1 and 'lumen-lsp.exe' or 'lumen-lsp'
  local roots = {}
  if vim.env.CARGO_TARGET_DIR then
    table.insert(roots, vim.env.CARGO_TARGET_DIR)
  end
  local project = vim.fs.root(0, { 'lumen.toml', 'Cargo.toml', '.git' })
  if project then
    table.insert(roots, vim.fs.joinpath(project, 'target'))
  end
  table.insert(roots, vim.fs.joinpath(vim.uv.cwd(), 'target'))

  for _, root in ipairs(roots) do
    for _, profile in ipairs({ 'release', 'debug' }) do
      local candidate = vim.fs.joinpath(root, profile, exe)
      if is_file(candidate) then
        return candidate
      end
    end
  end

  return 'lumen-lsp'
end

local function register_filetype()
  vim.filetype.add({ extension = { lmn = 'lumen' } })
end

local function install_info(opts)
  local info = { location = GRAMMAR_LOCATION, queries = GRAMMAR_QUERIES }
  if opts.grammar_path then
    info.path = vim.fn.expand(opts.grammar_path)
  else
    info.url = opts.grammar_url
    info.revision = opts.grammar_revision
  end
  return info
end

local function register_parser(opts)
  local ok, parsers = pcall(require, 'nvim-treesitter.parsers')
  if not ok then
    return
  end

  if type(parsers.get_parser_configs) == 'function' then
    -- nvim-treesitter master branch.
    local info = install_info(opts)
    parsers.get_parser_configs().lumen = {
      install_info = {
        url = info.path or info.url,
        revision = info.revision,
        location = GRAMMAR_LOCATION,
        files = { 'src/parser.c' },
      },
      filetype = 'lumen',
    }
  else
    -- nvim-treesitter main branch. Registration has to run while the plugin
    -- refreshes its parser list, which is what the TSUpdate event is for.
    parsers.lumen = { install_info = install_info(opts), tier = 2 }
  end
end

--- Set up filetype detection, the grammar, and the language server.
---@param opts? table
function M.setup(opts)
  opts = vim.tbl_extend('force', defaults, opts or {})

  register_filetype()

  if opts.treesitter then
    register_parser(opts)
    vim.api.nvim_create_autocmd('User', {
      pattern = 'TSUpdate',
      callback = function()
        register_parser(opts)
      end,
    })
  end

  if opts.lsp then
    vim.lsp.config('lumen_lsp', {
      cmd = { M.server_command(opts) },
      filetypes = { 'lumen' },
      root_markers = { 'lumen.toml', 'main.lmn', '.git' },
    })
    vim.lsp.enable('lumen_lsp')
  end
end

return M
