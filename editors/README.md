# Editor support for `.lm`

Syntax highlighting, parsing and a language server client for Lemma's `.lm` language. The syntax contract is [spec 13](../.specs/13-lm-grammar.md); everything here follows it.

| Directory | What it is |
| --- | --- |
| `tree-sitter-lemma/` | A Tree-sitter grammar with highlight, locals, fold and indent queries, for Neovim, Helix, Zed and anything else that loads Tree-sitter parsers |
| `vscode/` | A VS Code extension: a TextMate grammar, language configuration and a client for the language server |
| `zed/` | A Zed extension: the language's settings, its queries for Zed, and the code that starts the language server |
| `neovim/`, `helix/` | The Neovim and Helix checks, which run each editor with the setup this README documents |
| `fixture/` | The module the editor checks open: valid but for one requirement naming a predicate nothing declares, so the language server has one problem to report |
| `check.sh` | Regenerates the parser, runs the grammar's tests, parses every example in spec 13 and every `.lm` file in the repository, and runs each editor's check |

`editors/check.sh` runs every section; name sections to run only those, such as `editors/check.sh grammar vscode`. The editor sections run the language server built from this checkout (`$LM`, or `target/release/lm`, which they build with `cargo build --release -p lemma-cli --bin lm`), and skip an editor that is not installed, saying why.

## The language server

Every editor below runs the same command, from Lemma's `lm` binary, which works with `.lm` files (`lemma` itself checks and verifies):

```sh
lm lsp
```

It speaks the Language Server Protocol over stdio. A directory holding `mod.lm`, a repository's root module, is a good project root.

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
  cmd = { 'lm', 'lsp' },
  filetypes = { 'lemma' },
  root_markers = { 'mod.lm', '.git' },
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

**Verified** with Neovim 0.12.6. `neovim/check.lua` runs in a headless Neovim (`nvim --clean --headless -l`) with the parser built by `tree-sitter build` and the queries on its runtime path, and runs the first Lua block above as written. It opens `editors/fixture/mod.lm` and asserts the highlight capture that wins at eleven positions, among them a keyword, a declared name, a rule variable and a string. Then it asserts that the `lemma` server attaches, rooted at the fixture's directory, and publishes the fixture's one error at the right place. Re-run it with `editors/check.sh neovim`. Set `NVIM_EXECUTABLE` to use a Neovim that is not on `PATH`; the check is skipped when Neovim is missing or older than 0.11.

The nvim-treesitter `main` setup was also followed once in a throwaway configuration, with nvim-treesitter's `main` branch of 2026-11-06: `:TSInstall lemma` compiled the parser and installed the queries, and highlighting, folding, nvim-treesitter's indentation and the diagnostics all worked. The `master` branch setup has not been tried.

## Helix

Add the language and its server to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "lemma"
scope = "source.lemma"
file-types = ["lm"]
roots = ["mod.lm"]
comment-token = "//"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 4, unit = "    " }
language-servers = ["lemma"]

[language-server.lemma]
command = "lm"
args = ["lsp"]
```

Build the grammar into Helix's runtime directory, under the name Helix loads (`lemma.dylib` on macOS):

```sh
grammar=/path/to/lemma/editors/tree-sitter-lemma
mkdir -p ~/.config/helix/runtime/grammars
cc -shared -fPIC -O2 -I "$grammar/src" "$grammar/src/parser.c" "$grammar/src/scanner.c" \
  -o ~/.config/helix/runtime/grammars/lemma.so
```

Helix can build it instead, from a `[[grammar]]` entry in `languages.toml`:

```toml
[[grammar]]
name = "lemma"
source = { path = "/path/to/lemma/editors/tree-sitter-lemma" }
```

A git source works too: `source = { git = "<repository URL>", rev = "<commit>", subpath = "editors/tree-sitter-lemma" }`. Then run `hx --grammar build`. It builds every grammar Helix is configured with, not only this one, and fails for those `hx --grammar fetch` has not fetched. It also writes into `~/.config/helix/runtime/grammars` without creating it, so make that directory first. To build only this grammar, put `use-grammars = { only = ["lemma"] }` at the top of `languages.toml`; it limits what `hx --grammar` fetches and builds, not which grammars Helix loads.

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

The queries need Helix 25.07 or later. Its Tree-sitter loads the ABI 15 parser, and like Neovim it lets a later pattern override an earlier one for the same node, which the queries rely on; Helix before 25.07 gave the earlier pattern precedence. If Helix reports that the grammar's ABI version is unsupported, see [older runtimes](#older-runtimes).

Helix highlights a reference to a local definition with the scope its `@local.definition.<scope>` capture names. In `locals.scm` only `type`, `function` and `namespace` are Helix scopes, and they agree with the highlights; the others are nvim-treesitter's kinds, so those references keep their highlights from `highlights.scm`.

**Verified** with Helix 25.07.1. `helix/check.sh` follows this section in a throwaway home: it writes the first `languages.toml` block with this checkout's path, runs the shell blocks as written, and asserts that `hx --health lemma` finds the language server, the parser and the highlight queries. It asserts that every capture in the translated `highlights.scm` is a scope Helix's theme documentation lists, and builds the grammar a second time through the `[[grammar]]` entry and `hx --grammar build`. With tmux installed it then opens `editors/fixture/mod.lm` in Helix in a detached tmux session, with a theme giving each checked scope its own colour, and asserts that a keyword, the declared relation and rule, a rule variable and a string have their colours and that the server's diagnostic marks the gutter. Re-run it with `editors/check.sh helix`; set `HELIX_EXECUTABLE` to use a Helix that is not on `PATH`.

## Zed

`zed/` is a Zed extension. `extension.toml` names the grammar and the language server, and `languages/lemma/` holds the language's settings and the queries Zed reads: highlights, brackets, indents, the outline and the scopes that stop a quote auto-closing in strings and comments. Zed starts a language server only through extension code, so `src/lib.rs` runs `lm lsp` with the `lm` that `worktree.which` finds on the worktree's `PATH`.

`languages/lemma/highlights.scm` is generated from the grammar's highlights by `zed/update-highlights.sh`, which renames the two captures Zed's themes name differently; run it after changing the grammar's highlights. Zed clones the grammar at the commit `rev` in `extension.toml` names, the last one that changed it. To try a grammar that is not committed and pushed yet, set `repository` to your checkout, `file:///path/to/lemma`, and `rev` to a local commit.

To install it, run **zed: install dev extension** and choose `editors/zed`. Zed builds the crate for `wasm32-wasip2` and compiles the grammar with its own wasi-sdk.

**Verified** without Zed: no Zed ran on this machine, so the extension has not been seen working in Zed itself. The crate builds for `wasm32-wasip2`, which Zed uses, and for `wasm32-wasip1`, and passes clippy. Every query compiles and runs with the Tree-sitter CLI over spec 13's examples and every `.lm` file in the repository. Once, by hand, the grammar was also compiled as Zed's extension builder compiles it, with wasi-sdk 34's clang and the same flags. The result loaded through Tree-sitter's WebAssembly store, the loader Zed uses (here from the tree-sitter 0.25 crate; Zed pins a later revision). It parsed the fixture without errors, the five queries compiled, and the keyword, declared names, rule variables and string got the captures they should. Re-run the automated part with `editors/check.sh zed`. It needs `rustup target add wasm32-wasip2` (or `wasm32-wasip1`) and skips the build without one.

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

The extension starts `lm lsp` when it opens a `.lm` file. Set `lemma.lmPath` if `lm` is not on your `PATH`, and run **Lemma: Restart Language Server** after rebuilding the server. `lemma.trace.server` logs the protocol messages to the output panel.

**Verified** with VS Code 1.145.2 on Linux. `npm run test:integration` packages the extension, installs the `.vsix` into a fresh VS Code profile, points `lemma.lmPath` at the `lm` built from this checkout and opens `editors/fixture`. Its Mocha suite in `test/suite` runs inside VS Code and asserts that:

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

It regenerates `src/`, which is committed so editors can build the parser without the Tree-sitter CLI, and fails if any example in spec 13 or any `.lm` file in the repository (the standard library in `crates/lemma-lang/src/stdlib`, the parser's corpus) stops parsing. Commit the regenerated files with the grammar change. When the spec changes, update `test/corpus/spec.txt` to match its examples. After changing `highlights.scm`, run `zed/update-highlights.sh`; `check.sh` fails until the Zed copy matches. The Zed extension can only pin a commit that already holds the grammar, so once a grammar change is committed, set `rev` in `zed/extension.toml` to that commit in a commit of its own; `check.sh` notes when the two differ.

The grammar follows the parser in `crates/lemma-lang/src/syntax`. Where that parser reads a newline as a separator that cannot stand, as in `fn f(): Int` with its `{` on the next line, `let x` with `= 1` on the next, or an `if` condition continuing on a line that starts with `&&`, the grammar reports an error too. Error recovery keeps a declaration whole when only its `{` is on the next line, and ends a half-written line such as `let x` at its newline, so the next line still parses. A graph row starting with `(` is a tuple in both, so `(a) -> b` is an error.

The grammar still accepts a little more, so an editor can highlight what the compiler rejects:

- The `module` line is optional. Most examples in spec 13 are fragments without one, and so is a file being started.
- A newline right after a unary operator (`!`, `-`, `need`) or a member's `.` is whitespace. Those rules also occur inside `( )` and `[ ]`, where a newline is whitespace, and the scanner cannot tell which context it is in; marking the places outside brackets would take a second copy of the expression rules.
- Whether a term is a variable is left to the queries, so a variable with two fields, `x.a.b`, parses. A variable is a name starting with a lowercase letter or `_`. Telling it apart in the grammar would take separate tokens for lowercase and uppercase names, which clash with each other where a predicate name and a term can both start a rule literal.

Both parsers read a newline after the `:` of a type annotation or return type as continuing the line, as section 1 says, so `let x:` with `Int = 1` on the next line parses. Both reject reserved words used as names, such as `let in = 1`, and a `\u` escape without four hex digits.
