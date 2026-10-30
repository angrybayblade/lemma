# Lemma

Lemma indexes source code into a shared, versioned store of symbols and asks language servers about them. This first iteration covers TypeScript, Python, and Go: syntax indexing with Tree-sitter, pluggable language servers for semantic answers, and reference lookups for people and agents.

## Quick start

```sh
cargo install --path crates/lemma-cli --locked   # needs Rust 1.85+

lemma index ~/code/project              # writes ~/code/project/.lemma/index.db
cd ~/code/project
lemma symbols Router                    # search by name or qualified name
lemma refs Router.delete                # references via the language's server
lemma refs delete --language python     # narrow an ambiguous name
lemma refs '#415'                       # or pick by id from `lemma symbols`
lemma providers                         # which servers are installed and tried
```

Add `--json` to any command for machine-readable output.

Lemma does not bundle language servers. Install the ones you need: `typescript-language-server` or TypeScript 7's `tsc`, `ty` (fastest) or `pyright`, and `gopls`.

Lemma is built to be called in a loop by agents. Commands run in a background daemon that watches indexed directories and keeps language servers warm. Across 241 randomly drawn symbols in five repositories, 96% of the lines Lemma reported were real references, against 31% for ripgrep; with Lemma running, a lookup took 19-126 ms the first time a symbol was asked and 1-3 ms once cached. Text search remains better for one-off questions, small repositories, strings and other text, and as a cross-check before a rename. See [the comparison](docs/comparison.md) and [Performance](docs/performance.md).

Lemma keeps "no answer" separate from "no references". "No references found" means the server looked and saw nothing; it does not prove a symbol is unused.

## Documentation

- [Getting started](docs/getting-started.md): install, set up language servers, first lookup.
- [Commands](docs/commands.md): every command, option, and exit code.
- [Language servers](docs/providers.md): built-in providers, adding your own, how one is chosen.
- [How Lemma works](docs/concepts.md): snapshots, stale results, caching, current limits.
- [JSON output](docs/json-output.md): the output contract for scripts and agents.
- [Comparison](docs/comparison.md): Lemma against grep, ripgrep, git grep and ast-grep, with caveats.
- [Performance](docs/performance.md): benchmarks, and the approaches that were tried.
- [Troubleshooting](docs/troubleshooting.md): failed lookups and missing results.

## Layout

| Crate | Responsibility |
| --- | --- |
| `lemma-model` | Domain types: snapshots, symbols, references, outcomes |
| `lemma-syntax` | Tree-sitter extraction of definitions |
| `lemma-lsp` | Generic LSP client: framing, lifecycle, readiness, encodings |
| `lemma-providers` | Provider registry and the LSP-to-Lemma adapter |
| `lemma-store` | SQLite storage format |
| `lemma-index` | Walking, hashing, and snapshotting a directory |
| `lemma-query` | Symbol resolution and reference lookups |
| `lemma-cli` | The `lemma` command and its background daemon |

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions.
