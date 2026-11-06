# Pluggable semantic providers and lean indexing

Date: 2026-10-28
Status: accepted starting architecture; interfaces and implementation details below are proposed contracts to validate through a pilot.

## Decision

Start with a Rust core, a small Tree-sitter-based syntax index, pluggable language-specific LSP servers for semantic enrichment, and Lemma's own normalized graph model and relationship language.

SCIP is not required for the initial implementation. Preserve the ability to ingest SCIP or another bulk semantic index later without changing user declarations or rule semantics.

The packaging goal is a lean Lemma core with optional semantic providers. Language servers and their required runtimes/project dependencies remain separate prerequisites. A small core executable does not imply low total runtime memory or installation cost.

## Responsibility boundaries

```text
Source snapshots ──> Tree-sitter syntax extraction ─────────┐
                                                        │
Language servers <── Lemma LSP client + adapters ──────────┤
                                                        ▼
                                      Normalized symbols and facts
                                                        ▲
Relationship declarations ──> reference resolution ────────┘
                                                        │
                                                        ▼
                                       Versioned graph + rule engine
                                                        │
                                              CLI / agents / CI
                                                        │
                                           Future editor-facing LSP
```

LSP is a request/response protocol, not a source-language grammar. Each provider owns its parser, name resolver, and type analysis. Lemma consumes the results rather than attempting to reproduce every language's semantics.

The shared relationship language operates on normalized symbols, operations, services, requirements, and evidence. It does not parse TypeScript, Python, or Go directly. Its concrete syntax remains undecided.

The upstream LSP client used for semantic enrichment is distinct from the future downstream language server that exposes Lemma features to editors. They share the core model but serve different roles.

## Syntax layer

Use Tree-sitter for:

- Definitions, lexical containment, source ranges, and written signatures/annotations.
- Import/export syntax and call-expression locations.
- Argument expressions, literals, decorators, and framework-specific structural patterns.
- Fast parsing of changed or incomplete source files.

Tree-sitter does not independently establish type identity, runtime dispatch, full import resolution, or complete call graphs. Parsed annotations are not inferred types. Pattern recognition is not evidence of a resolved service dependency.

Initially store extracted records and source spans rather than entire syntax trees. Retrieve snippets from source after confirming the content hash. Trees may be retained in memory for active editing without becoming the durable storage contract.

## Semantic provider layer

Use existing language servers as optional, capability-aware providers. Start by exercising two providers against real repositories; TypeScript and Python are natural pilot candidates because the existing generic platform example contains both. Go remains a target language. Exact servers and versions must be selected through evaluation.

| Information | Standard LSP operation | Limits |
| --- | --- | --- |
| File symbols | `textDocument/documentSymbol` | Outline, not guaranteed enumeration of every local binding |
| Symbol search | `workspace/symbol` | Search, not a complete graph-export contract |
| Definitions | `textDocument/definition` | Position-based, potentially multiple targets |
| References | `textDocument/references` | Server/workspace-dependent coverage |
| Types and implementations | Type-definition/implementation requests | Optional capabilities |
| Call hierarchy | Prepare/incoming/outgoing hierarchy requests | Optional; precision and completeness vary |
| Signature information | Signature help, symbol detail, hover | Call-site assistance/display text, not a universal declaration type schema |
| Imports | No standard complete import-graph operation | Requires syntax extraction and resolution or extensions |
| Stable identity | Optional monikers | Must not assume universal support or cross-revision stability |

Do not infer exhaustive graph coverage from a successful LSP request. In particular, an empty search or references response is only an empty result under that provider's scope and state.

## Common client and adapters

The common Rust client should own JSON-RPC transport, request IDs, initialization, capabilities, document synchronization, cancellation, timeouts, process lifecycle, and diagnostics/result routing.

Small provider adapters should own:

- Executable invocation and version reporting.
- Workspace root selection and provider-specific project configuration.
- Necessary initialization options and server-to-client request handling.
- Useful extensions and normalization differences.
- Known coverage limitations and readiness behavior.

Capability adaptation should be explicit. Do not invent methods or infer support from a server's language name. Track capabilities registered dynamically as well as at initialization.

LSP does not guarantee a universal “entire repository analysis complete” signal. Readiness and background-analysis behavior need provider-specific testing.

## Proposed normalized operations

```text
symbols_in_file(file)
search_symbols(query, scope)
resolve_at(file, position)
references_to(symbol)
implementations_of(symbol)
signature_of(symbol)
```

These are Lemma operations, not new standard LSP methods. Adapters may combine requests and syntax facts to answer them. For example, reference lookup needs a concrete source occurrence to address the upstream server.

Results should retain:

- Snapshot identity and source-content/document version.
- Provider identity, version, configuration fingerprint, and workspace scope.
- Original URI/range and normalized source location.
- Origin: syntax, semantic provider, declaration, or derived inference.
- Resolution state and candidates where applicable.
- Supported coverage claims and relevant limitations.

Distinguish successful empty results, unsupported operations, ambiguous resolution, unresolved targets, stale results, and provider failures. These extraction states are separate from requirement outcomes such as satisfied or violated.

Keep signature display text separate from structured signature fields. Populate structured fields only when the producer supports them reliably; do not treat arbitrary hover Markdown as a canonical type representation.

## Identity and snapshots

Lemma owns its identifiers. Preserve upstream identifiers when available, but do not rely solely on names, paths, monikers, or URI/ranges for identity across revisions.

Distinguish a symbol occurrence in one snapshot from a logical symbol identity reconciled across snapshots. Renames and moves require explicit reconciliation; ambiguous cases should remain visible.

A snapshot must account for source contents, repository revisions, dependency/project configuration, extraction configuration, and provider versions. A Git commit alone is insufficient for a dirty worktree or an editor buffer.

Join syntax and semantic results only when they describe compatible source versions. Normalize coordinate encodings explicitly, including Tree-sitter byte offsets and the negotiated LSP position encoding.

## Query-first enrichment

LSP is optimized for interactive requests rather than bulk export. The initial indexing strategy is:

1. Build a lightweight syntax inventory and searchable local index.
2. Locate candidate symbols and source occurrences relevant to a query or declaration.
3. Ask semantic providers to resolve or enrich those candidates.
4. Cache results with their input fingerprints and coverage metadata.
5. Derive relationships and obligations from the resulting facts.

Bound concurrency and query expansion. Measure provider startup, query latency, cache behavior, and memory before attempting whole-repository reference enumeration.

If bulk semantic indexing becomes necessary, evaluate SCIP or direct compiler/indexer adapters as additional producers. The normalized model must not depend on LSP response shapes.

## Storage and discovery proposal

SQLite is the initial storage candidate, not a finalized dependency. The model should support:

- Files and content hashes.
- Symbols and containment.
- Occurrences and their resolution results.
- Typed relationships and provenance.
- Snapshots and extraction coverage.
- Requirements, checker definitions, and revision-bound evidence.

Provide exact lookup, text search, and indexes for traversal in both directions. A custom domain schema is sufficient initially; designing a custom binary storage format is unnecessary.

## Example: HTTP relationship resolution

For a call such as `client.delete(path)`:

1. Syntax extraction identifies the call expression, arguments, and enclosing function.
2. An available semantic provider resolves the relevant identifier occurrence where possible.
3. A framework adapter or explicit declaration connects the consumer symbol to an HTTP operation.
4. Another declaration/extractor links the backend implementation to that operation.
5. Rules derive impact and verification obligations.

Neither LSP nor Tree-sitter automatically proves that the consumer and handler share a contract. Dynamic paths, routing configuration, wrappers, and unresolved dispatch must remain explicit uncertainties.

## Editing and invalidation

Maintain a durable graph snapshot and temporary overlays for unsaved documents. Reject or label late responses that refer to superseded document versions.

Syntax can refresh before semantic enrichment. During that interval, affected semantic facts must be stale or unknown rather than silently reused.

Cache invalidation must account for dependency/configuration changes as well as edits to the directly queried file. Evidence invalidation follows the verification model; fresh syntax or a successful definition lookup does not establish fresh behavioral evidence.

## Pilot acceptance criteria

- Exercise two selected language servers through one shared client and small adapters.
- Resolve known definitions/references and navigate to the correct source ranges.
- Capture useful signature information without claiming unsupported structure.
- Distinguish same-named symbols and return ambiguous/unresolved cases explicitly.
- Handle missing dependencies and unsupported capabilities without silently reporting complete coverage.
- Demonstrate unsaved-edit synchronization and rejection/invalidation of stale results.
- Persist and reload normalized results with provenance and snapshot identity.
- Resolve explicit consumers of one shared HTTP operation and derive one required check.
- Measure startup/query latency, request counts, cache behavior, and memory on the chosen workloads.
- Demonstrate useful syntax search when semantic providers are unavailable.

Performance targets and server versions remain to be established. This acceptance list is not a claim that the implementation exists.

## Deferred: choosing a provider by name

**Status: deferred, 2026-11-06.** Today a project chooses its servers by writing provider definitions in `.lemma/providers.toml` (tried before the built-in order), or per command with `--providers`. That is per repository, cannot name a choice ("pyright for Python"), and pyrefly has no built-in definition. Python's built-in order is pyright, then ty, then basedpyright: in a comparison on a large repository, pyright found one reference ty missed (a classmethod called through an instance of a generic subclass of a metaclass-built class) and used less memory, while ty answered far faster.

Proposed when this is picked up:

- A preference by name, per language: `[languages] python = "pyright"`, or an ordered list for fallback, in the user's `~/.config/lemma/config.toml` and in a project's `.lemma/providers.toml`. Precedence: `--providers`, then the project, then the user, then the built-in order. The daemon reads its config again when it changes, so a switch applies to the next lookup.
- pyrefly, and basedpyright, as built-in definitions that are opt-in: available by name, not tried unless chosen. pyrefly once returned references from the open file only (see Performance), which must be checked again before it is offered.
- `lemma providers` and `lemma daemon lsp` say where each choice came from (`user config`, `project`, `built-in order`).
- A chosen server that is not installed is reported with its install hint.

Open: whether a chosen server that cannot start falls back to the built-in order with a warning (proposed default) or fails (opt-in, `strict = true`).

## References

- [LSP 3.17 specification source](https://github.com/Microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/specification.md)
- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/)
- [SCIP](https://github.com/scip-code/scip)

See [Graph and verification](02-graph-and-verification.md) for relationship/evidence semantics and [Language server and integrations](05-lsp-and-integrations.md) for the future editor-facing server.
