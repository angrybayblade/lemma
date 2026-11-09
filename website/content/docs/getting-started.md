# Getting started

Install Lemma, index a project, and look up your first references. Complete this page to use Lemma for code navigation. To check changes across repositories, continue with the [quickstart](quickstart.md).

## 1. Install

Install the binary. Releases support macOS and Linux on arm64 and x86_64:

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
```

The script installs into `~/.local/bin`. Add that directory to your `PATH` if it is not there. For a pinned version, the agent skills or another directory, see [Onboarding](onboarding.md#1-install).

To build from source, install Rust 1.85 or newer with [rustup](https://rustup.rs), then run:

```sh
git clone https://github.com/angrybayblade/lemma.git
cd lemma
cargo install --path crates/lemma-cli --locked   # installs lemma and lm
```

This installs `lemma` and `lm` into `~/.cargo/bin`. Verify the install:

```sh
lemma --version
```

To build without installing, run `cargo build --release` and use `target/release/lemma`.

## 2. Install a language server

Lemma parses definitions itself and asks a language server where symbols are used. It bundles no servers. Install one for each language you index:

| Language | Install one of | Command Lemma looks for |
| --- | --- | --- |
| TypeScript / TSX, TypeScript 6 or older | `npm install -g typescript typescript-language-server` | `typescript-language-server` |
| TypeScript / TSX, TypeScript 7 | `npm install -g typescript@7` | `tsc` |
| Python | `npm install -g pyright` (or `uv tool install ty`, faster, more memory) | `pyright-langserver` (or `ty`) |
| Python | `npm install -g pyright` or `pip install basedpyright` | `pyright-langserver` or `basedpyright-langserver` |
| Go | `go install golang.org/x/tools/gopls@latest` | `gopls` |
| Rust | `rustup component add rust-analyzer` | `rust-analyzer` |

The command must be on your `PATH`. Run `lemma providers` to see what Lemma found:

```text
$ lemma providers
typescript-language-server   typescript,tsx   /usr/local/bin/typescript-language-server
typescript-native            typescript,tsx   /usr/local/bin/tsc
pyright                      python           /usr/local/bin/pyright-langserver
ty                           python           /Users/me/.local/bin/ty
basedpyright                 python           not found: basedpyright-langserver
gopls                        go               /Users/me/go/bin/gopls

Tried in order for each language:
  typescript   typescript-language-server, then typescript-native
  tsx          typescript-language-server, then typescript-native
  python       pyright, then ty
  go           gopls
```

Install each project's own dependencies first (`npm install`, a Python virtual environment, `go mod download`, `cargo fetch`). Without them, references that go through imports may be missing.

## 3. Index a project

Without a description, Lemma indexes every supported file and warns. Create a `mod.lm` at the project's root, with a service for each part of the project and the directories that hold its code:

```lm
// ~/code/my-project/mod.lm
module my_project

export service api {
    index: {
        include: ["src", "scripts"]
        exclude: ["**/generated/**"]
    }
}
```

Name the module with an identifier: the directory `my-project` takes the module `my_project`.

Extend this file later to describe what each service offers and how it relates to others; see [Check links](check.md). Index the project:

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

Indexing starts no language server and works offline. Run it again when the code changes: Lemma reads only changed files, so on a large repository it takes a fraction of a second, and prints `unchanged` if nothing changed.

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
via pyright
3 references
  src/api/routes.py:22:12  in remove_route
  src/api/routes.py:57:8  in reset
  tests/test_router.py:40:5  in test_delete_unknown_route
```

Commands run in a background daemon that `lemma` starts on first use. The daemon starts each language server after `lemma index` and caches answers until the code changes. The first lookup may wait for the server to load the project; later lookups, even of other symbols, take milliseconds.

If a name matches more than one symbol, Lemma lists the candidates. Choose one with the qualified name, a filter such as `--language python` or `--path api/`, or the id: `lemma refs '#415'`.

## 6. Ask other questions

```sh
lemma definition src/api/routes.py:22:12   # where the call there goes
lemma implementations Shape.area           # what implements an interface method
lemma signature Router.delete              # its declaration and documentation
lemma outline src/router.py                # what a file defines
```

## 7. Connect an agent

Run `lemma mcp` as an MCP server to give agents the same answers. In Claude Code, run this from the project directory:

```sh
claude mcp add lemma -- lemma mcp
```

Then tell agents when to use it; see [Connect agents](onboarding.md#3-connect-agents).

## Next steps

- [Commands](commands.md): every command, option, and exit code.
- [Language servers](providers.md): built-in servers, adding your own, and how one is chosen.
- [Concepts](concepts.md): snapshots, stale results, and caching.
- [Architecture](architecture.md): how the layers fit together.
- [JSON output](json-output.md): using Lemma from scripts and agents.
- [Comparison](comparison.md): when to use Lemma and when to use grep or ripgrep.
- [Performance](performance.md): how fast Lemma is on large repositories, and why.
- [Troubleshooting](troubleshooting.md): what to do when a lookup fails or finds nothing.
