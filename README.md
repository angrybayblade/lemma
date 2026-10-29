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

Lemma is built to be called in a loop by agents. Re-indexing only reads what changed (about 0.15 s on a 73,000-file repository), and a background daemon keeps language servers warm, so reference lookups after the first take milliseconds. See [Performance](docs/performance.md).

Lemma keeps "no answer" separate from "no references". "No references found" means the server looked and saw nothing; it does not prove a symbol is unused.

## Documentation

- [Getting started](docs/getting-started.md): install, set up language servers, first lookup.
- [Commands](docs/commands.md): every command, option, and exit code.
- [Language servers](docs/providers.md): built-in providers, adding your own, how one is chosen.
- [How Lemma works](docs/concepts.md): snapshots, stale results, caching, current limits.
- [JSON output](docs/json-output.md): the output contract for scripts and agents.
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
