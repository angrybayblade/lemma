# Lemma

Lemma checks the contracts between your services before a change merges: it reads every repository of a system at the right commits and reports what a change breaks, where, and what depends on that code. It also answers questions about code (where a symbol is used or defined, what implements it) through language servers, for TypeScript, Python, Go and Rust, and serves both to agents over MCP.

**Start here:** [Introduction](docs/introduction.md) · [Quickstart](docs/quickstart.md) · [Tutorials](docs/tutorials/README.md)

Lemma is in release candidates; see the [pre-release checklist](docs/pre-release.md) for what is ready and what is not.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
# add LEMMA_SKILLS=1 before `sh` to also install the agent skills
```

Or from source: `cargo install --path crates/lemma-cli --locked`, which installs `lemma` and `lm`, the command for `.lm` modules (Rust 1.85+). Details in [Onboarding](docs/onboarding.md#1-install).

## Quick start: navigating code

Describe the project in `mod.lm` at its root: its services and the code each is made of.

```lm
module project

export service api {
    index: { include: ["src"] }
}
```

```sh
lemma index ~/code/project              # writes ~/code/project/.lemma/index.ls
cd ~/code/project
lemma symbols Router                    # search by name or qualified name
lemma refs Router.delete                # references via the language's server
lemma refs delete --language python     # narrow an ambiguous name
lemma refs '#f256e26e'                  # or pick by id from `lemma symbols`
lemma outline src/router.py             # symbols in a file
lemma definition src/app.py:12:5        # where the symbol used there is defined
lemma implementations Shape.area        # what implements or overrides it
lemma signature Router.delete           # declaration and the server's description
lemma providers                         # which servers are installed and tried
lemma check --change self               # what a change breaks in the repositories that depend on it
lemma verify                            # whether the description still matches the code
```

Add `--json` to any command for machine-readable output. For agents, `lemma mcp` serves the same commands as MCP tools; in Claude Code, `claude mcp add lemma -- lemma mcp` adds it to a project.

Lemma does not bundle language servers. Install the ones you need: `typescript-language-server` or TypeScript 7's `tsc`, `pyright` or `ty` (faster, more memory), `gopls`, and `rust-analyzer`.

Lemma is built to be called in a loop by agents. Commands run in a background daemon that watches indexed directories and keeps language servers warm. Across 241 randomly drawn symbols in five repositories, 96% of the lines Lemma reported were real references, against 31% for ripgrep; with Lemma running, a lookup took 19-126 ms the first time a symbol was asked and 1-3 ms once cached, in one repository. From a workspace's root, each query first checks every member for changes, which took about 0.1 s across six repositories. Text search remains better for one-off questions, small repositories, strings and other text, and as a cross-check before a rename. See [the comparison](docs/comparison.md) and [Performance](docs/performance.md).

Lemma keeps "no answer" separate from "no references". "No references found" means the server looked and saw nothing; it does not prove a symbol is unused.

## Documentation

Learn:

- [Introduction](docs/introduction.md): what Lemma checks, how you describe a system, and its two commands.
- [Quickstart](docs/quickstart.md): two repositories, a breaking change, and Lemma catching it, in about ten minutes.
- [Tutorials](docs/tutorials/README.md): links, your own requirements, allowances, behaviors, and editor setup.

Use and reference:

- [Onboarding](docs/onboarding.md): set Lemma up for a team's repositories, from install to checks in CI.
- [Pre-release checklist](docs/pre-release.md): what this release candidate is ready for, and its known limitations.
- [Check links](docs/check.md): `lemma check`, `.lm` descriptions, schema comparison, inferred links and tracing.
- [Verifying descriptions](docs/verify.md): `lemma verify`, whether a description still matches the code.
- [Reading operations from code](docs/adapters.md): adapters that read a service's contract from its code, and the protocol for writing one.
- [Requirements and rules](docs/grammar.md): writing the relations, rules and requirements `lemma check` runs.
- [The `.lm` language](docs/language.md): services, links, rules and requirements as typed modules, with editor support.
- [Agent skills](skills/README.md): skills that teach coding agents to use Lemma.
- [Lemma mode](docs/lemma-mode.md): the workflow agents follow to make and prove a change other code depends on.
- [Getting started](docs/getting-started.md): install, set up language servers, first lookup.
- [Commands](docs/commands.md): every command, option, and exit code.
- [Language servers](docs/providers.md): built-in providers, adding your own, how one is chosen.
- [Concepts](docs/concepts.md): snapshots, stale results, caching, current limits.
- [Architecture](docs/architecture.md): the four layers and where each can grow.
- [JSON output](docs/json-output.md): the output contract for scripts and agents.
- [Comparison](docs/comparison.md): Lemma against grep, ripgrep, git grep and ast-grep, with caveats.
- [Agents with and without Lemma](docs/agent-benchmark.md): what Lemma changes for a coding agent, measured.
- [Performance](docs/performance.md): benchmarks, and the approaches that were tried.
- [Troubleshooting](docs/troubleshooting.md): failed lookups and missing results.

## Brand and marketing

The [landing page](website/README.md) leads with agent speed and system precision: formal requirements, cross-repository impact, and checker-backed behavior simulations. Licensed Suisse files are pending; the documented fallback remains active.

## Layout

| Crate | Responsibility |
| --- | --- |
| `lemma-model` | Domain types: snapshots, symbols, locations, outcomes |
| `lemma-syntax` | Tree-sitter extraction of definitions and declarations |
| `lemma-lsp` | Generic LSP client: framing, lifecycle, readiness, encodings |
| `lemma-providers` | The `SemanticProvider` trait, provider registry, and the LSP adapter |
| `lemma-store` | `index.ls`: snapshots, files, symbols, attributes and cached answers in one append-only file of segments (spec 17) |
| `lemma-mapped` | A read-only memory map: the one crate allowed `unsafe` |
| `lemma-index` | Walking, hashing, and snapshotting a directory |
| `lemma-query` | The operations: symbols, outline, references, definitions, implementations, signatures |
| `lemma-logic` | Lemma's Datalog: programs, facts, the `Engine` trait and a reference engine |
| `lemma-souffle` | Soufflé as an engine |
| `lemma-grammar` | Relations, rules and requirements compiled to Datalog |
| `lemma-http` | HTTP contract facts from OpenAPI documents and code |
| `lemma-verify` | Extracting facts from documents and code, and evaluating requirements over them |
| `lemma-cli` | The `lemma` command, its background daemon, and the MCP server |

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions.
