# Seshat review and generic platform pilot

## Evaluated versions

- Project: [KSDaemon/seshat](https://github.com/KSDaemon/seshat).
- Source review: commit `ea5d77f5380c58bf42c6c761475216fcf1e6521f`, dated 2026-10-20; workspace version 0.8.0.
- Executed pilot: published macOS ARM64 release **v0.8.0**, binary reports `seshat 0.8.0 (6e7eca1)`, released 2026-10-02.
- These are different source revisions. Findings from the later source review must not automatically be attributed to the release binary.

The source archive inspected earlier is at:

```text
/Users/viraj/Documents/Codex/2026-10-28/what/work/seshat-audit/KSDaemon-seshat-ea5d77f
```

## Current feature surface from source review

Seshat is a local project-intelligence tool built in Rust with SQLite storage and an MCP interface. Its surface is closest to a maintained project guide plus code-navigation index.

It parses Rust, TypeScript/TSX, JavaScript, and Python. The reviewed version does not support Go.

Its intermediate representation includes functions, types, imports, exports, parameters, documentation comments, and call expressions. This is not a compiler-resolved semantic index. Some call enrichment matches callee names and can conflate unrelated methods with the same name.

The nine MCP tools are:

| Tool | Purpose |
| --- | --- |
| `query_project_context` | Languages, modules, dependency domains, conventions, representative files |
| `query_convention` | Search conventions and recorded decisions |
| `query_code_pattern` | Symbol-name lookup with snippets, importers, and call-site enrichment |
| `query_dependencies` | File-level dependencies and reverse/transitive import traversal |
| `map_diff_impact` | Approximate changed-symbol and dependent impact from Git changes |
| `validate_approach` | Retrieve records whose keywords overlap a proposed approach |
| `record_decision` | Store a human/agent-authored decision with optional evidence examples |
| `update_decision` | Amend an existing decision |
| `remove_decision` | Remove an existing decision |

The reviewed code explicitly says `validate_approach` is retrieval, not a verdict. It neither sees the eventual implementation nor establishes that a requirement was fulfilled.

Other reviewed capabilities:

- Heuristic import resolution, including relative paths, aliases, and workspace packages.
- Convention detection around naming, imports, errors, libraries, logging, tests, exports, and directory structure.
- Markdown, JSON Schema, and YAML OpenAPI/Swagger ingestion.
- Incremental file hashing, watcher updates, branch/HEAD synchronization, and cached IR.
- Optional local embeddings in the reviewed source; these were not enabled or evaluated in the pilot.
- Separate indexes for root projects and first-level Git submodules.

## Relationship boundary

Seshat can recover some import relationships between modules/packages inside one indexed repository. The missing capability is **user-defined, typed relationships across independent symbol graphs**, including HTTP service relationships, cross-language parity, and verification obligations.

Submodule scoping routes queries to a separate index; it does not provide a federated cross-scope graph join.

A prose decision can mention symbols in two repositories, but that does not make the connection a typed, traversable, automatically validated edge. There is no reviewed workflow that turns such a declaration into revision-specific checks or evidence.

## Generic platform checkout

The assessed path was `/Users/viraj/Workspace/sdk` on branch `next`.

The repository guidance identifies it as the SDK v3 monorepo: TypeScript/Python SDKs, providers, CLI, tests, examples, and docs. This pilot did not cover the complete generic platform's backend or neighboring repositories.

Tracked inventory from the preliminary inspection:

- 1,260 `.ts` files, 154 `.tsx` files, 241 `.py` files.
- No tracked Go or Rust source files in this checkout.
- 24 TypeScript package manifests under `ts/packages` (including fixture manifests counted by that path test).
- 13 Python provider manifests.

Relevant boundaries:

- TypeScript core depends on `@sdk/client`; Python depends on `platform-client`.
- Generated clients do not automatically connect this checkout to backend handler symbols.
- Core uses runtime-conditioned imports such as `#platform`, `#files`, and `#ssrf_guard` for Node/Cloudflare variants.
- Examples, docs, fixtures, generated files, and vendored submodules can distort repository-wide convention inference.

## Pilot setup

No Rust toolchain was available, so the published ARM64 binary was downloaded.

- [Release v0.8.0](https://github.com/KSDaemon/seshat/releases/tag/v0.8.0)
- Archive SHA-256: `d68799bd1a594db7596768e37ec1898189afc9fae8e26132e88c78d52daee746`.
- The archive digest matched the published checksum. Signature verification was not performed; an earlier download prompt incorrectly described the archive as signed.
- Binary retained under `/Users/viraj/Workspace/.seshat-pilot/seshat-aarch64-apple-darwin-v0.8.0/seshat`.

The initial attempt to redirect storage with `XDG_DATA_HOME` did not work on macOS because the implementation uses `dirs::data_dir()`. With user-granted write permission, the scan used:

```text
/Users/viraj/Library/Application Support/seshat/repos/sdk.db
```

Seshat's `init` command was not run, and no agent client registration was performed. No intentional edits were made to generic platform source. A post-pilot Git status comparison was not recorded, so the prior claim that nothing changed was not independently verified afterward.

The scan command was:

```sh
/Users/viraj/Workspace/.seshat-pilot/seshat-aarch64-apple-darwin-v0.8.0/seshat scan /Users/viraj/Workspace/sdk --exclude-submodules
```

This was a broad default scan excluding the two vendor submodules, not the narrower core/provider-only configuration later recommended.

## Scan output

| Metric | Reported value |
| --- | --- |
| Elapsed scan time | 7.7 seconds |
| Files scanned/parsed | 1,690 / 1,690 |
| TypeScript, including TSX | 1,414 |
| Python | 241 |
| JavaScript | 35 |
| Nodes | 4,445 |
| Edges | 501 |
| Manifests analyzed | 2 |
| Documents ingested | 581 |
| Packages reported | 24, all npm |
| Conventions detected | 160 |
| Database size | 23.7 MB |

The node/edge totals are Seshat's scan counters, not a measured complete symbol/call graph. Import relationships may be computed from stored IR at query time. They should not be interpreted as a dependency-recall metric.

The scan did not report Python workspace packages. The exact cause was not isolated; `uv` workspace recognition is a plausible gap, not a separately proven diagnosis.

On the first brief MCP startup, the banner reported zero files/conventions; the next startup reported 1,690 files and “Conventions: 4605.” The mismatch with the scan's 160 conventions was not investigated. Do not use the banner count as a validated convention count.

## Queries exercised

The server was invoked through stdio JSON-RPC with initialization, `tools/list`, and `tools/call`. No persistent editor integration was configured.

### Project context

`query_project_context(focus_area="testing")` returned the language inventory, testing dependencies, conventions, and representative files. It also returned a very large repository-wide module list, including docs and fixtures. Task-context compactness was not achieved merely by supplying a focus area.

### Exact symbol lookup

`query_code_pattern(query="delete_task_router_session", kind="function")` returned:

- `python/sdk/core/models/task_router_session_delete.py`.
- Calls from `task_router.py` and `task_router_session.py`.
- Two dependent files and two call sites.

`query_code_pattern(query="deleteTaskRouterSession", kind="function")` returned:

- `ts/packages/core/src/lib/taskRouterSessionDelete.ts`.
- Calls from `models/TaskRouter.ts` and `models/TaskRouterSession.ts`.
- Two dependent files and two call sites.

These results aligned with the inspected implementations. This was exact-name retrieval using symbols already known to the evaluator, not a blind natural-language discovery test. Both helpers call `DELETE /api/v3.1/task_router/session/{id}`. No explicit cross-language equivalence edge was returned.

### Dependency lookup

`query_dependencies(path="ts/packages/core/src/lib/taskRouterSessionDelete.ts", depth=3)` returned:

- Internal dependencies on request-option types, Tool Router types/schema, and cancellation utilities.
- Direct dependents `TaskRouter.ts` and `TaskRouterSession.ts`.
- 16 total transitive dependents through depth three, including core classes and tests.
- External dependency `@sdk/client`.

The direct relationships were plausible and matched the source inspection. The full transitive set was not scored against an independently constructed oracle. These are file-level dependency paths, not proof that each listed file's behavior changes.

### Approach guidance

The submitted proposal was to change Tool Router session deletion while preserving SDK parity, URL encoding, response validation, and regression coverage.

The response reported zero relevant rules, zero decisions, ten conventions, five observations, and four possible duplicates. The duplicate results were unrelated TypeScript-generation errors. Examples included docs code and harness fixtures.

It surfaced both “Uses default exports exclusively (TypeScript)” and “Uses named exports exclusively (TypeScript)” with 100% confidence. These may describe different detected subsets, but the presentation did not make them reliable project-wide instructions. This is evidence of confusing scope and relevance, not proof of a logical contradiction inside the detector.

### Diff impact

`map_diff_impact(base="HEAD~1")` returned six changed paths, zero affected symbols, zero hunks, and a risk value of `none`. Paths included docs JSON, a skills symlink, a CLAUDE guidance file, and vendor submodules.

The actual Git diff was not independently reconciled with that output, and no source-code mutation or representative historical source change was tested. Therefore this run does not establish diff-impact accuracy or failure. It should not be used as evidence that a change is safe to merge.

## Assessment

Observed strengths: quick indexing on this machine, useful exact-symbol lookup, local call-site context, and import traversal for the sampled helpers.

Observed limitations: broad/noisy context, irrelevant approach guidance, no reported Python packages, and no cross-language/API relationship in the sampled responses.

Source-reviewed gaps relative to Lemma: Go support, federated typed relationships, HTTP consumer/provider linking, infrastructure relationships, obligation derivation, revision-bound verification evidence, and formal verification orchestration.

This was a functional smoke test. It did not measure retrieval precision/recall, latency distributions, agent task success, token savings, edit freshness, or merge outcomes.

## Proposed next evaluation

Use historical tasks with independently identified relevant files and obligations. Compare graph-assisted work with ordinary search. Consider narrower indexing around SDK cores/providers and tests, while retaining useful docs through separate retrieval or scoped conventions. No such tuned second pilot was run.

## Source references

- [Reviewed source snapshot](https://github.com/KSDaemon/seshat/tree/ea5d77f5380c58bf42c6c761475216fcf1e6521f)
- [IR](https://github.com/KSDaemon/seshat/blob/ea5d77f5380c58bf42c6c761475216fcf1e6521f/crates/seshat-core/src/ir.rs)
- [Dependency queries](https://github.com/KSDaemon/seshat/blob/ea5d77f5380c58bf42c6c761475216fcf1e6521f/crates/seshat-graph/src/dependencies.rs)
- [Approach retrieval handler](https://github.com/KSDaemon/seshat/blob/ea5d77f5380c58bf42c6c761475216fcf1e6521f/crates/seshat-mcp/src/tools/validate_approach.rs)
- [Scope routing](https://github.com/KSDaemon/seshat/blob/ea5d77f5380c58bf42c6c761475216fcf1e6521f/crates/seshat-mcp/src/scope.rs)
