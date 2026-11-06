# Editor support for `.lm`

Syntax highlighting, parsing and a language server client for Lemma's `.lm` language. The syntax contract is [spec 13](../.specs/13-lm-grammar.md); everything here follows it.

| Directory | What it is |
| --- | --- |
| `tree-sitter-lemma/` | A Tree-sitter grammar with highlight, locals, fold and indent queries, for Neovim, Helix, Zed and anything else that loads Tree-sitter parsers |
| `vscode/` | A VS Code extension: a TextMate grammar, language configuration and a client for the language server |
| `fixture/` | The module the editor checks open: valid but for one requirement naming a predicate nothing declares, so the language server has one problem to report |
| `check.sh` | Regenerates the parser, runs the grammar's tests, parses every example in spec 13 and every `.lm` file in the repository, and runs each editor's check |

`editors/check.sh` runs every section; name sections to run only those, such as `editors/check.sh grammar vscode`. The editor sections run the language server built from this checkout (`$LEMMA`, or `target/release/lemma`, which they build), and skip an editor that is not installed, saying why.

## The language server

Every editor below runs the same command:

```sh
lemma lm lsp
```

It speaks the Language Server Protocol over stdio. A directory holding `lemma.lm`, a repository's root module, is a good project root.

## Neovim

The parser needs a Tree-sitter runtime of 0.25 or later, which Neovim has from 0.11. If Neovim reports that the parser's ABI version is unsupported, see [older runtimes](#older-runtimes).

With [nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter)'s `main` branch, register the parser from a checkout of this repository, install it, and start it for `.lm` files:

```lua
vim.filetype.add({ extension = { lm = 'lemma' } })

vim.api.nvim_create_autocmd('User', {
  pattern = 'TSUpdate',
  callback = function()
    require('nvim-treesitter.parsers').lemma = {
      install_info = {
        path = '/path/to/lemma/editors/tree-sitter-lemma',
        queries = 'queries',
      },
    }
  end,
})

vim.api.nvim_create_autocmd('FileType', {
  pattern = 'lemma',
  callback = function()
    vim.treesitter.start()
    vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'
    vim.wo.foldmethod = 'expr'
    vim.bo.indentexpr = "v:lua.require'nvim-treesitter'.indentexpr()"
  end,
})

vim.lsp.config('lemma', {
  cmd = { 'lemma', 'lm', 'lsp' },
  filetypes = { 'lemma' },
  root_markers = { 'lemma.lm', '.git' },
})
vim.lsp.enable('lemma')
```

Then run `:TSInstall lemma`.

With the older `master` branch, register it through `get_parser_configs()` instead, and copy the queries yourself, since that branch does not install them. That branch is mostly used with Neovim before 0.11, which needs an [ABI 14 parser](#older-runtimes) generated first:

```lua
require('nvim-treesitter.parsers').get_parser_configs().lemma = {
  install_info = {
    url = '/path/to/lemma/editors/tree-sitter-lemma',
    files = { 'src/parser.c', 'src/scanner.c' },
  },
  filetype = 'lemma',
}
```

```sh
mkdir -p ~/.config/nvim/queries/lemma
cp /path/to/lemma/editors/tree-sitter-lemma/queries/*.scm ~/.config/nvim/queries/lemma/
```

The queries use nvim-treesitter's capture names and its precedence, where a later pattern overrides an earlier one for the same node.

## Helix

Add the language, its server and the grammar to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "lemma"
scope = "source.lemma"
file-types = ["lm"]
roots = ["lemma.lm"]
comment-token = "//"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 4, unit = "    " }
language-servers = ["lemma"]

[language-server.lemma]
command = "lemma"
args = ["lm", "lsp"]

[[grammar]]
name = "lemma"
source = { path = "/path/to/lemma/editors/tree-sitter-lemma" }
```

A git source works too: `source = { git = "<repository URL>", rev = "<commit>", subpath = "editors/tree-sitter-lemma" }`, followed by `hx --grammar fetch`. Then build the grammar:

```sh
hx --grammar build
```

Helix names a few highlight captures differently, so translate them while copying the queries. Helix has its own indent captures and no fold queries, so only highlights and locals are copied:

```sh
queries=/path/to/lemma/editors/tree-sitter-lemma/queries
target=~/.config/helix/runtime/queries/lemma
mkdir -p "$target"
sed -E \
  -e 's/@function\.method\.call/@function.method/g' \
  -e 's/@function\.call/@function/g' \
  -e 's/@keyword\.(import|conditional|repeat|return)/@keyword.control.\1/g' \
  -e 's/@keyword\.modifier/@keyword.storage.modifier/g' \
  -e 's/@keyword\.type/@keyword.storage.type/g' \
  -e 's/@type\.definition/@type/g' \
  -e 's/@module/@namespace/g' \
  -e 's/@boolean/@constant.builtin.boolean/g' \
  -e 's/@number/@constant.numeric.integer/g' \
  -e 's/@property/@variable.other.member/g' \
  -e 's/@string\.escape/@constant.character.escape/g' \
  -e 's/@character\.special/@special/g' \
  -e 's/ @spell//g' \
  "$queries/highlights.scm" >"$target/highlights.scm"
cp "$queries/locals.scm" "$target/"
```

The parser needs a Helix built against Tree-sitter 0.25 or later. If Helix reports that the grammar's ABI version is unsupported, see [older runtimes](#older-runtimes).

## Zed

Zed loads languages from extensions. Make a development extension with an `extension.toml` that points at the grammar:

```toml
id = "lemma"
name = "Lemma"
version = "0.1.0"
schema_version = 1

[grammars.lemma]
repository = "file:///path/to/lemma"
rev = "<commit>"
path = "editors/tree-sitter-lemma"
```

Add `languages/lemma/config.toml` with `name = "Lemma"`, `grammar = "lemma"`, `path_suffixes = ["lm"]`, `line_comments = ["// "]` and `block_comment = ["/* ", " */"]`, copy `highlights.scm` into `languages/lemma/`, and install the folder with **zed: install dev extension**. Zed starts a language server only through extension code, so running `lemma lm lsp` takes a small `language_server_command` implementation in the extension's `src/lib.rs`.

## VS Code

Build the extension, then either run it from source or install a package:

```sh
cd editors/vscode
npm ci
npm run compile
```

- **From the folder:** open `editors/vscode` in VS Code and press F5 to start an Extension Development Host with the extension loaded.
- **As a package:** run `npm run package`, then `code --install-extension lemma-lm-0.1.0.vsix`.

Highlighting comes from `syntaxes/lemma.tmLanguage.json`. TextMate grammars match lines with regular expressions, so contextual keywords such as `service` or `check` are recognised where a declaration starts a line, and rule variables inside rule clauses and `when:` bodies.

The extension starts `lemma lm lsp` when it opens a `.lm` file. Set `lemma.path` if `lemma` is not on your `PATH`, and run **Lemma: Restart Language Server** after rebuilding the server. `lemma.trace.server` logs the protocol messages to the output panel.

**Verified** with VS Code 1.145.2 on Linux. `npm run test:integration` packages the extension, installs the `.vsix` into a fresh VS Code profile, points `lemma.path` at the server built from this checkout and opens `editors/fixture`. Its Mocha suite in `test/suite` runs inside VS Code and asserts that:

- the extension runs from the installed package and gives `.lm` files the `lemma` language;
- the server's diagnostic for the fixture's error arrives, with its message and position;
- hover on a relation shows its columns, and go-to-definition jumps to its declaration;
- the TextMate grammar scopes keywords, strings and rule variables.

Re-run it with `editors/check.sh vscode`, which also runs the static checks. The test downloads VS Code into `.vscode-test/` on first use; set `VSCODE_EXECUTABLE` to use an installed one or `VSCODE_VERSION` to pick another release. Without a display it runs itself under `xvfb-run`, and it is skipped when neither is available or VS Code cannot be downloaded.

## Older runtimes

The committed parser uses Tree-sitter's ABI 15, which lets reserved words such as `let` stay keywords even where only a name could appear, so a half-typed line does not swallow the next one. Editors built against an older Tree-sitter load only ABI 14. Generate an ABI 14 parser for them:

```sh
cd editors/tree-sitter-lemma
npm ci
npx tree-sitter generate --abi 14
```

Reserved words then behave like the contextual ones. Two error-recovery tests in `test/corpus/errors.txt` expect the ABI 15 behaviour, so do not commit the regenerated files.

## Changing the grammar

Edit `tree-sitter-lemma/grammar.js` (and `src/scanner.c` for newlines and string interpolation), then run:

```sh
editors/check.sh
```

It regenerates `src/`, which is committed so editors can build the parser without the Tree-sitter CLI, and fails if any example in spec 13 or any `.lm` file in the repository (the standard library in `crates/lemma-lang/src/stdlib`, the parser's corpus) stops parsing. Commit the regenerated files with the grammar change. When the spec changes, update `test/corpus/spec.txt` to match its examples.

The grammar follows the parser in `crates/lemma-lang/src/syntax`. It accepts a little more, so an editor can still highlight a file the compiler rejects:

- the `module` line is optional, since most examples in the spec are fragments;
- a newline is a separator only where the grammar can end an item, so `fn f(): Int` with its `{` on the next line parses;
- a graph row `(a) -> b` parses, though a row starting with `(` is a tuple;
- whether a term is a variable is left to the queries, so a variable with two fields, `x.a.b`, parses.

It is stricter in two places, both following spec 13: reserved words are never names, so `let in = 1` is an error, and a `\u` escape needs four hex digits.
