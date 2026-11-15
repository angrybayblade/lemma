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

**What a caller is defined inside (2026-11-15).** A use inside a nested function belongs to that function, the innermost definition around it. A caller graph adds the function or value that definition is written inside as a user of it, without a step of depth: what a function is written around is part of it. It is read from the file's symbols, with nothing asked. A type is not added for its members, which are not uses of one another. Without this a walk to depth 2 that met a nested function stopped one caller short. Of 109 sampled graphs in a workspace of six repositories, 8 changed: together they gained 581 nodes and lost 39, the 39 in one graph that gained 411.

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

**A server that cannot start (2026-11-14).** Providers are tried in order, and one that cannot start gives way to the next. Where that is the settled state of a machine, as `typescript-language-server` beside TypeScript 7 with `typescript-native` serving, every lookup used to start the first again before falling back, 0.1 s each. A graph asks dozens of questions: of 7.7 s for one after an edit on a public repository, 7 s went to 58 failed starts and 0.1 s to the server that answered. The session pool now remembers the failure with its message (`SessionPool::failed_starts`) and reports it as `ProviderError::CouldNotStart` without starting the server: for 60 s in a pool that stays, so a repaired installation is picked up within a minute, and for the life of a pool made for one lookup. A server started since, as `lemma daemon lsp start` does, serves at once. On that repository the first graph after an edit went from 7.5 s to 1.0 to 1.5 s for two symbols and from 38 s to 3.6 s for a third, with the same answers; where the first server starts, nothing changes.

**Waiting that can be given up (2026-11-14).** A client can be handed a function that says when to stop waiting for answers (`LspClient::interrupt_when`), for requests made ahead of anyone asking ([spec 14](14-sentry.md), "More than one graph, and giving way"). The request is not cancelled, and the same request made again before the server is told of a change waits for the answer already on its way.

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

**Callers outside the snapshot (2026-11-15).** A language server reads the files its project names, not the files Lemma indexes, so a reference can be in a file the snapshot leaves out: a test, or a script outside `include`. A graph names the definition such a use sits in by parsing that one file when the walk meets it, gives it the id it would have if the file were indexed, and marks it `(outside the snapshot)`. Nothing is added to the index. What was found around the use is kept with the snapshot's answers under the file's contents and the extractor's version, so a graph asked again reads the file and parses nothing: without that, a graph with 199 such callers took 3.7 s each time it was asked again, and with it 0.23 s.

The walk does not ask about such a definition unless `--outside <n>` allows it. A lookup in a file the snapshot does not hold is answered only for a walk that has read the file, and its answer is kept under a hash of the file's contents, because the snapshot's own fingerprint does not cover the file.

Naming without following is the default because of what was measured in a workspace of six repositories. Of 109 sampled graphs, 29 ended at files outside the snapshot, 692 such files in all, 634 of them not tests by their names. Naming left 9 graphs ending at 34 files, 31 of them the top-level code of tests, and named 775 definitions, with a repeated graph as fast as before (a median of 25 ms). Following every named definition, on a sample of 120, added 26 more outside the snapshot and 3 inside it, since 795 of 834 were tests and entry points nothing calls, and the first ask of one graph went from 0.15 s to over seven minutes: about two seconds for each of its 199 callers. So the graph says what is at the edge of what a description includes, and going past the edge is asked for by number.

**A server too slow for a question (2026-11-15).** A provider that starts and then fails is reported, and the next is not asked, so that a real problem is not hidden. One failure is different in kind on a very large repository: the server is well and cannot answer one question in the time a lookup waits. In a Python repository of tens of thousands of files, pyright did not answer `textDocument/references` within 60 s for 11 of 120 sampled symbols, on every ask. Each failed after about 72 s, the session was thrown away as a failed one is, nothing was kept, and a graph had no callers under that symbol.

What a lookup does then is a setting, `[servers] when_slow`, because the right answer depends on the repository and most are nowhere near that size. `"report"`, the default, is the behaviour above. `"next"` asks the next installed provider for the language when one times out, and names both in the answer (`slow_providers`, a line under `via`, one note on a graph). Only a timeout is handed on: an error or a crash is still reported. Only when a next provider is installed: the last one's timeout is reported as before.

Asking the next one every time would wait the whole timeout for every such symbol, and start the slow server again each time. So once another provider has answered, the slow one is passed over on that root, in the pool: lookups there go straight to the next, and a warm-up starts the one lookups will ask. It is passed over only after another answered, so a machine too busy for any server keeps its order, and never when every installed provider would be. `try_again_after` says for how long, from `"0s"` (ask it first every time) through the default of 30 minutes to `"never"` (not again while the pool lives).

How long a provider gets before the next is asked is `give_way_after`, the lookup's own wait unless set. It is given to the server for that one question, in place of the timeout the server was started with, because that timeout is whatever the command or warm-up that started it chose: five minutes for a server the daemon warmed up, which is no time to wait with another server ready. Only a provider with another after it is given it, so the last one keeps the lookup's whole wait and a pool that reports slow servers behaves as it did. It takes a time: the two ends of the range are already settings, `"report"` for never and the provider order for at once. A pool for one command keeps the same record for that command. Changing the setting clears it.

With `"next"` on that repository all 11 graphs were answered: the first in 109 s (pyright's 59 s, then ty starting; 66 s with `give_way_after = "20s"`), the other ten in 0.7 to 7.8 s with pyright passed over, a repeated one in 1 s or less. ty held 12.3 GB, and pyright was not running. Which provider answered is then a matter of timing, which is why it is off unless asked for and said in every answer it touches. Stored answers are used whichever installed provider gave them, as before, so a graph can rest on answers from two providers that count differently (ty leaves import statements out).

Not covered: `find_uses`, the lookup under a root that is not indexed, tries providers in its own loop. It leaves out a provider passed over on that root, and still reports a timeout of its own. A repository where the first provider is too slow for most questions is better served by listing another in `.lemma/providers.toml`, which is tried first and waits for nothing; choosing by name stays deferred (below).

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

## Deferred: functions given to locals

**Status: tried and dropped for now, 2026-11-15.** A function declared inside another is a symbol. The same function written as `const save = () => ...` (TypeScript) or `save := func() { ... }` (Go) inside a function is not: it cannot be asked about by name, and a use inside it counts as a use by the function around it.

What was tried: two definition queries, one for a `variable_declarator` whose value is an arrow function, a function expression or a generator (`queries/typescript.scm`), one for a `short_var_declaration` of a single name given a `func_literal` (`queries/go.scm`), with `short_var_declaration` classified as a function and the extractor's version moved, since every index is read again from its files. Other locals stayed out.

What was measured, in a workspace of six repositories: 1,576 more symbols on 114,252 (1.4%), and of the caller graphs that ended at the top level of an indexed file, 165 where there had been 168. Against that, a graph traced across repositories lost 17 operations. The trace names a call by the innermost definition around it (`enclosing_function` in the HTTP code facts, and the handlers and impact walks that read them) and then looks for that function's callers by name. A call inside `const save = () => ...` became a call by `save`, which nothing calls by that name from outside, where before it was a call by the handler around it, which the route names.

Why it was dropped: three graphs out of 168 gained, for a change to how every chain across repositories is named.

To take it up again, settle first what a call belongs to where both the symbol walk and the trace read it: the innermost definition (the symbol walk's answer, which wants these locals indexed) or the outermost function a route or an export can name (the trace's). Two ways were considered and not built: the trace walks outwards from a nested definition until it reaches one that is called by name, as a caller graph now adds what a caller is defined inside; or locals are indexed for the symbol walk and left out of the code facts the trace reads. Either needs the differential checks of the trace (recall against the mutation ground truth, the replay of past changes) before and after, since the loss showed only there. The measurements need a sample that counts graphs ending inside indexed files, not only at files outside the snapshot.

## References

- [LSP 3.17 specification source](https://github.com/Microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/specification.md)
- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/)
- [SCIP](https://github.com/scip-code/scip)

See [Graph and verification](02-graph-and-verification.md) for relationship/evidence semantics and [Language server and integrations](05-lsp-and-integrations.md) for the future editor-facing server.
