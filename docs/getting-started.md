# Getting started

This guide takes you from a fresh checkout to your first reference lookup.

## 1. Build and install

Lemma is a single binary. You need a Rust toolchain, 1.85 or newer ([rustup](https://rustup.rs) is the easiest way to get one).

```sh
git clone https://github.com/angrybayblade/lemma.git
cd lemma
cargo install --path crates/lemma-cli --locked
```

This puts `lemma` in `~/.cargo/bin`. Check it works:

```sh
lemma --version
```

To build without installing, run `cargo build --release` and use `target/release/lemma`.

## 2. Install a language server

Lemma reads definitions itself, but asks a language server where symbols are used. It does not bundle any servers, so install one for each language you care about:

| Language | Install one of | Command Lemma looks for |
| --- | --- | --- |
| TypeScript / TSX, TypeScript 6 or older | `npm install -g typescript typescript-language-server` | `typescript-language-server` |
| TypeScript / TSX, TypeScript 7 | `npm install -g typescript@7` | `tsc` |
| Python (fastest) | `uv tool install ty` or `pip install ty` | `ty` |
| Python | `npm install -g pyright` or `pip install basedpyright` | `pyright-langserver` or `basedpyright-langserver` |
| Go | `go install golang.org/x/tools/gopls@latest` | `gopls` |

The command must be on your `PATH`. Run `lemma providers` to see what Lemma found:

```text
$ lemma providers
typescript-language-server   typescript,tsx   /usr/local/bin/typescript-language-server
typescript-native            typescript,tsx   /usr/local/bin/tsc
ty                           python           /Users/me/.local/bin/ty
pyright                      python           /usr/local/bin/pyright-langserver
basedpyright                 python           not found: basedpyright-langserver
gopls                        go               /Users/me/go/bin/gopls

Tried in order for each language:
  typescript   typescript-language-server, then typescript-native
  tsx          typescript-language-server, then typescript-native
  python       ty, then pyright
  go           gopls
```

Language servers work best when the project's own dependencies are installed (`npm install`, a Python virtual environment, `go mod download`). Without them, references that go through imports may be missing.

## 3. Index a project

```sh
lemma index ~/code/my-project
```

```text
indexed /Users/me/code/my-project (212 files, 1840 symbols)
  python       180 files
  typescript   32 files
revision 4f2c1e9a..., uncommitted changes
index /Users/me/code/my-project/.lemma/index.db
```

Lemma reads every `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, `.pyi` and `.go` file, respecting `.gitignore`, and stores what it finds in `.lemma/index.db` inside the project. Add `.lemma/` to the project's `.gitignore`.

Indexing does not start any language server, so it is fast and works offline. Run it again whenever the code changes: only changed files are read again, so on a large repository it takes a fraction of a second. If nothing changed, Lemma says `unchanged`.

## 4. Find a symbol

Run the remaining commands from anywhere inside the project. Lemma finds the nearest `.lemma/index.db` above the current directory.

```sh
cd ~/code/my-project
lemma symbols Router
```

```text
#412     class      Router  src/router.py:8:7
#415     method     Router.delete  src/router.py:31:9
#418     method     Router.register  src/router.py:44:9
```

Each line shows the symbol's id, kind, qualified name, and where it is defined. Positions are `line:column`, counted from 1 as in an editor.

## 5. Find its references

```sh
lemma refs Router.delete
```

```text
#415     method     Router.delete  src/router.py:31:9
via ty 0.0.94
3 references
  src/api/routes.py:22:12  in remove_route
  src/api/routes.py:57:8  in reset
  tests/test_router.py:40:5  in test_delete_unknown_route
```

The first lookup for a language starts its language server, so it takes a few seconds. Lemma keeps the server running in a background daemon, so later lookups, even of other symbols, take milliseconds. Answers are also cached until the code changes.

If a name matches more than one symbol, Lemma lists the candidates and asks you to choose. Use the qualified name, a filter such as `--language python` or `--path api/`, or the id: `lemma refs '#415'`.

## Next steps

- [Commands](commands.md): every command, option, and exit code.
- [Language servers](providers.md): built-in servers, adding your own, and how one is chosen.
- [How Lemma works](concepts.md): snapshots, stale results, and caching.
- [JSON output](json-output.md): using Lemma from scripts and agents.
- [Performance](performance.md): how fast Lemma is on large repositories, and why.
- [Troubleshooting](troubleshooting.md): what to do when a lookup fails or finds nothing.
