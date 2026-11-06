-- Checks the `.lm` support in a real Neovim, headless:
--
--   nvim --clean --headless -l check.lua <runtime> <fixture> <readme>
--
-- <runtime> holds parser/lemma.so and queries/lemma/*.scm, built from
-- editors/tree-sitter-lemma. <readme> is editors/README.md: its first Lua
-- block under "## Neovim" is run as written, so the check covers the
-- filetype, Tree-sitter and language server setup it documents. `lm`
-- must be on PATH, as that setup runs `lm lsp`.
--
-- The check opens <fixture>, asserts the highlight capture that wins at a
-- few positions, then waits for the language server to attach and publish
-- the one problem the fixture has. editors/check.sh runs it.

local runtime, fixture, readme = arg[1], arg[2], arg[3]
if not (runtime and fixture and readme) then
  io.stderr:write('usage: nvim --clean --headless -l check.lua <runtime> <fixture> <readme>\n')
  os.exit(2)
end

local attach_timeout_ms = 20000
local diagnostics_timeout_ms = 30000
local poll_interval_ms = 50

local failures = {}

local function fail(message)
  table.insert(failures, message)
end

local function finish()
  for _, client in ipairs(vim.lsp.get_clients()) do
    client:stop(true)
  end
  if #failures > 0 then
    io.stderr:write('Neovim check failed:\n  ' .. table.concat(failures, '\n  ') .. '\n')
    os.exit(1)
  end
  io.stdout:write('Neovim ' .. tostring(vim.version()) .. ': highlights and language server work\n')
  os.exit(0)
end

--- The first ```lua block in the README's "## Neovim" section.
local function readme_setup(path)
  local in_section, in_block, block = false, false, {}
  for _, line in ipairs(vim.fn.readfile(path)) do
    if in_block then
      if line:match('^```') then
        return table.concat(block, '\n')
      end
      table.insert(block, line)
    elseif line:match('^## ') then
      in_section = line == '## Neovim'
    elseif in_section and line:match('^```lua') then
      in_block = true
    end
  end
  error('no ```lua block under "## Neovim" in ' .. path)
end

--- The zero-based row and column of the `|` in `marked`, a piece of the
--- buffer's text with `|` marking a position, such as `relation |Owns`.
local function position_of(buffer, marked)
  local column_in_text = marked:find('|', 1, true) - 1
  local text = marked:gsub('|', '', 1)
  for row, line in ipairs(vim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
    local start = line:find(text, 1, true)
    if start then
      return row - 1, start - 1 + column_in_text
    end
  end
  error('the fixture has no `' .. text .. '`')
end

--- The highlight captures at a position, in the order Neovim applies them,
--- so the last one is what the position shows.
local function captures_at(buffer, row, column)
  local names = {}
  for _, found in ipairs(vim.treesitter.get_captures_at_pos(buffer, row, column)) do
    if found.capture ~= 'spell' and found.capture ~= 'nospell' then
      table.insert(names, found.capture)
    end
  end
  return names
end

local function check_highlights(buffer)
  local cases = {
    { at = '|module fixture', capture = 'keyword.import' },
    { at = '|relation Owns', capture = 'keyword.type' },
    { at = '|rule owned', capture = 'keyword' },
    { at = 'relation |Owns', capture = 'type.definition' },
    { at = 'rule |owned', capture = 'function' },
    { at = 'require |owned_twice', capture = 'function' },
    { at = '(|service) :-', capture = 'variable.parameter' },
    { at = ':- Owns(|_, service)', capture = 'variable.builtin' },
    { at = '(service: |Text)', capture = 'type.builtin' },
    { at = '"{service}| is owned by', capture = 'string' },
    { at = '"{|service} is owned by', capture = 'variable.parameter' },
  }
  for _, case in ipairs(cases) do
    local row, column = position_of(buffer, case.at)
    local names = captures_at(buffer, row, column)
    if names[#names] ~= case.capture then
      fail(
        string.format(
          '`%s` (%d:%d) shows @%s, not @%s; captures: %s',
          case.at,
          row + 1,
          column + 1,
          tostring(names[#names]),
          case.capture,
          table.concat(names, ', ')
        )
      )
    end
  end
end

local function check_language_server(buffer)
  local attached = vim.wait(attach_timeout_ms, function()
    return #vim.lsp.get_clients({ bufnr = buffer, name = 'lemma' }) > 0
  end, poll_interval_ms)
  if not attached then
    fail('the `lemma` language server did not attach within ' .. attach_timeout_ms .. ' ms')
    return
  end
  local client = vim.lsp.get_clients({ bufnr = buffer, name = 'lemma' })[1]
  local expected_root = vim.fs.dirname(vim.fs.normalize(vim.fn.fnamemodify(fixture, ':p')))
  if client.root_dir ~= expected_root then
    fail(string.format('the server runs in %s, not in %s', tostring(client.root_dir), expected_root))
  end

  local published = vim.wait(diagnostics_timeout_ms, function()
    return #vim.diagnostic.get(buffer) > 0
  end, poll_interval_ms)
  if not published then
    fail('the server published no diagnostics within ' .. diagnostics_timeout_ms .. ' ms')
    return
  end
  local diagnostics = vim.diagnostic.get(buffer)
  if #diagnostics ~= 1 then
    fail('expected one diagnostic, got ' .. vim.inspect(diagnostics))
    return
  end
  local problem = diagnostics[1]
  local row, column = position_of(buffer, 'when: |missing(service)')
  if not problem.message:find('no relation, rule or graph `missing`', 1, true) then
    fail('unexpected diagnostic message: ' .. problem.message)
  end
  if problem.lnum ~= row or problem.col ~= column then
    fail(string.format('the diagnostic is at %d:%d, not %d:%d', problem.lnum, problem.col, row, column))
  end
  if problem.severity ~= vim.diagnostic.severity.ERROR or problem.source ~= 'lemma' then
    fail('the diagnostic is not an error from lemma: ' .. vim.inspect(problem))
  end
end

vim.opt.runtimepath:prepend(runtime)
vim.cmd('filetype plugin indent on')
assert(load(readme_setup(readme), '@' .. readme))()

vim.cmd.edit(vim.fn.fnameescape(fixture))
local buffer = vim.api.nvim_get_current_buf()

if vim.bo[buffer].filetype ~= 'lemma' then
  fail('the fixture opened with filetype `' .. vim.bo[buffer].filetype .. '`, not `lemma`')
  finish()
end
if not vim.treesitter.highlighter.active[buffer] then
  fail('Tree-sitter highlighting did not start for the fixture')
  finish()
end
local tree = vim.treesitter.get_parser(buffer):parse()[1]
if tree:root():has_error() then
  fail('the parser found a syntax error in the fixture')
end

check_highlights(buffer)
check_language_server(buffer)
finish()
