# Lemma

Lemma indexes source code into a shared, versioned store of symbols and asks language servers about them. It covers TypeScript, Python, and Go: syntax indexing with Tree-sitter, pluggable language servers for semantic answers, and the questions agents ask while changing code: where a symbol is used, where something is defined, what implements an interface, and what a symbol's signature is. Agents can call it as an MCP server.

## Quick start

```sh
cargo install --path crates/lemma-cli --locked   # needs Rust 1.85+

lemma index ~/code/project              # writes ~/code/project/.lemma/index.db
cd ~/code/project
lemma symbols Router                    # search by name or qualified name
lemma refs Router.delete                # references via the language's server
lemma refs delete --language python     # narrow an ambiguous name
lemma refs '#415'                       # or pick by id from `lemma symbols`
lemma outline src/router.py             # symbols in a file
lemma definition src/app.py:12:5        # where the symbol used there is defined
lemma implementations Shape.area        # what implements or overrides it
lemma signature Router.delete           # declaration and the server's description
lemma providers                         # which servers are installed and tried
```

Add `--json` to any command for machine-readable output. For agents, `lemma mcp` serves the same commands as MCP tools; in Claude Code, `claude mcp add lemma -- lemma mcp` adds it to a project.

Lemma does not bundle language servers. Install the ones you need: `typescript-language-server` or TypeScript 7's `tsc`, `ty` (fastest) or `pyright`, and `gopls`.

Lemma is built to be called in a loop by agents. Commands run in a background daemon that watches indexed directories and keeps language servers warm. Across 241 randomly drawn symbols in five repositories, 96% of the lines Lemma reported were real references, against 31% for ripgrep; with Lemma running, a lookup took 19-126 ms the first time a symbol was asked and 1-3 ms once cached. Text search remains better for one-off questions, small repositories, strings and other text, and as a cross-check before a rename. See [the comparison](docs/comparison.md) and [Performance](docs/performance.md).

Lemma keeps "no answer" separate from "no references". "No references found" means the server looked and saw nothing; it does not prove a symbol is unused.

## Documentation

- [Getting started](docs/getting-started.md): install, set up language servers, first lookup.
- [Commands](docs/commands.md): every command, option, and exit code.
- [Language servers](docs/providers.md): built-in providers, adding your own, how one is chosen.
- [How Lemma works](docs/concepts.md): snapshots, stale results, caching, current limits.
- [Architecture](docs/architecture.md): the four layers and where each can grow.
- [JSON output](docs/json-output.md): the output contract for scripts and agents.
- [Comparison](docs/comparison.md): Lemma against grep, ripgrep, git grep and ast-grep, with caveats.
- [Performance](docs/performance.md): benchmarks, and the approaches that were tried.
- [Troubleshooting](docs/troubleshooting.md): failed lookups and missing results.

## Layout

| Crate | Responsibility |
| --- | --- |
| `lemma-model` | Domain types: snapshots, symbols, locations, outcomes |
| `lemma-syntax` | Tree-sitter extraction of definitions and declarations |
| `lemma-lsp` | Generic LSP client: framing, lifecycle, readiness, encodings |
| `lemma-providers` | The `SemanticProvider` trait, provider registry, and the LSP adapter |
| `lemma-store` | The `FactStore` trait and its SQLite storage format |
| `lemma-index` | Walking, hashing, and snapshotting a directory |
| `lemma-query` | The operations: symbols, outline, references, definitions, implementations, signatures |
| `lemma-cli` | The `lemma` command, its background daemon, and the MCP server |

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions.
