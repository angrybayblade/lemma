# Lemma

Lemma indexes source code into a shared, versioned store of symbols and asks language servers about them. It covers TypeScript, Python, Go, and Rust: syntax indexing with Tree-sitter, pluggable language servers for semantic answers, and the questions agents ask while changing code: where a symbol is used, where something is defined, what implements an interface, and what a symbol's signature is. Agents can call it as an MCP server.

Lemma is in release candidates; see the [pre-release checklist](docs/pre-release.md) for what is ready and what is not.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
# add LEMMA_SKILLS=1 before `sh` to also install the agent skills
```

Or from source: `cargo install --path crates/lemma-cli --locked` (Rust 1.85+). Details in [Onboarding](docs/onboarding.md#1-install).

## Quick start

Describe the project in `lemma.yaml` at its root: its services and the code each is made of.

```yaml
services:
  api:
    index:
      include: [src]
```

```sh
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
lemma verify change.yaml                # what a change breaks in the repositories that depend on it
```

Add `--json` to any command for machine-readable output. For agents, `lemma mcp` serves the same commands as MCP tools; in Claude Code, `claude mcp add lemma -- lemma mcp` adds it to a project.

Lemma does not bundle language servers. Install the ones you need: `typescript-language-server` or TypeScript 7's `tsc`, `ty` (fastest) or `pyright`, `gopls`, and `rust-analyzer`.

Lemma is built to be called in a loop by agents. Commands run in a background daemon that watches indexed directories and keeps language servers warm. Across 241 randomly drawn symbols in five repositories, 96% of the lines Lemma reported were real references, against 31% for ripgrep; with Lemma running, a lookup took 19-126 ms the first time a symbol was asked and 1-3 ms once cached. Text search remains better for one-off questions, small repositories, strings and other text, and as a cross-check before a rename. See [the comparison](docs/comparison.md) and [Performance](docs/performance.md).

Lemma keeps "no answer" separate from "no references". "No references found" means the server looked and saw nothing; it does not prove a symbol is unused.

## Documentation

- [Onboarding](docs/onboarding.md): set Lemma up for a team's repositories, from install to checks in CI.
- [Pre-release checklist](docs/pre-release.md): what this release candidate is ready for, and its known limitations.
- [Verifying changes](docs/verify.md): `lemma verify`, the manifest, findings, and how well it works.
- [Checking links](docs/check.md): `lemma check`, composable `lemma.yaml` files, schema comparison, inferred links and tracing.
- [Reading operations from code](docs/adapters.md): adapters that read a service's contract from its code, and the protocol for writing one.
- [Grammars](docs/grammar.md): writing the relations, rules and requirements `lemma verify` checks.
- [Agent skills](skills/README.md): skills that teach coding agents to use Lemma.
- [Getting started](docs/getting-started.md): install, set up language servers, first lookup.
- [Commands](docs/commands.md): every command, option, and exit code.
- [Language servers](docs/providers.md): built-in providers, adding your own, how one is chosen.
- [How Lemma works](docs/concepts.md): snapshots, stale results, caching, current limits.
- [Architecture](docs/architecture.md): the four layers and where each can grow.
- [JSON output](docs/json-output.md): the output contract for scripts and agents.
- [Comparison](docs/comparison.md): Lemma against grep, ripgrep, git grep and ast-grep, with caveats.
- [Agents with and without Lemma](docs/agent-benchmark.md): what Lemma changes for a coding agent, measured.
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
| `lemma-logic` | Lemma's Datalog: programs, facts, the `Engine` trait and a reference engine |
| `lemma-souffle` | Soufflé as an engine |
| `lemma-grammar` | User-defined grammars (YAML) compiled to Datalog |
| `lemma-http` | HTTP contract facts from OpenAPI documents and code |
| `lemma-verify` | Verifying a change across the repositories in a manifest |
| `lemma-cli` | The `lemma` command, its background daemon, and the MCP server |

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions.
