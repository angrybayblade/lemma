# Proposed first milestone and open decisions

## First end-to-end milestone

An engineer changes an HTTP operation or its implementation. Lemma identifies declared SDK consumers, retrieves their relevant symbols/contracts for an agent, determines the required checks, and reports which obligations are satisfied for the candidate revisions.

Use a small real example before attempting the entire platform graph. Generic platform session deletion is a candidate because its TypeScript/Python consumers are known. The backend repository and handler still need to be identified and put in scope.

## Suggested sequence

1. Define the minimal entity/relationship/evidence model and identity assumptions.
2. Build an explicit snapshot manifest listing repositories, commits, and extractor versions.
3. Build a small syntax index and pilot two pluggable LSP semantic providers, following [the semantic-provider specification](07-semantic-providers-and-indexing.md); declare a shared HTTP operation for the resolved symbols.
4. Resolve declarations deterministically, returning missing/ambiguous references explicitly.
5. Compare Soufflé and SHACL on traversal, constraints, obligation derivation, and explanations.
6. Connect one real compatibility or contract-test checker.
7. Bind results to the exact inputs and demonstrate evidence becoming stale after a relevant change.
8. Expose the same result through a CLI and agent-facing interface.
9. Add an LSP demonstration after the shared API and source mapping are usable.

This sequence is a proposal. The user has not authorized implementation of these components merely by discussing them.

## Evaluation questions

- Does the tool find the relevant symbols and relationships without already knowing exact names?
- Which affected consumers does it miss, and which unrelated consumers does it include?
- Can an engineer follow the evidence behind every inferred relationship and required check?
- Can an agent receive a useful context set within a predictable size budget?
- Does evidence become stale for the right changes and remain reusable for irrelevant changes?
- Does an incomplete graph produce an explicit unknown/coverage warning rather than false confidence?
- How do the results compare with full-text search and existing language tooling?
- Does the framework improve task completion time, correctness, or review burden on representative changes?

## Open decisions

### Model and rules

- ~~Soufflé, SHACL, TypeQL, another engine, or a deliberately small custom layer?~~ Decided: Datalog semantics, Soufflé first behind a pluggable `Engine` interface (document 8).
- ~~YAML/JSON/CUE authoring first, or a custom grammar immediately?~~ Decided: YAML first, `.ls` files with the same meaning.
- ~~How are relationship kinds defined and extended?~~ Decided: by a user-defined grammar that imports and extends a standard one.
- How are declared intent, extracted observations, inferred candidates, and verified evidence represented separately? Proposed in document 8: a source on every fact (syntax, semantic, spec, declared, observed, inferred).
- What are the exact semantics of cross-language parity?
- What completeness assumptions permit a rule to interpret absence as failure or nonexistence?

### Extraction and identity

- Which language servers and versions provide semantic enrichment for TypeScript, Python, and Go, and which two should be piloted first?
- When does bulk indexing justify an optional SCIP or compiler-specific adapter?
- What role, if any, should Seshat play: optional adapter, prototype input, or no dependency?
- How are identities retained or explicitly migrated across renames/moves?
- How are generated clients bound to OpenAPI operations and backend handlers?
- How are dynamic HTTP calls, URL construction, and environment-specific routing handled?
- How are runtime-conditioned implementations represented?

### Evidence and execution

- Which checks are built in versus plugins?
- How are required inputs, tool versions, environment, and result artifacts captured?
- How are tests, schema checks, graph constraints, and formal analyses distinguished in reports?
- What invalidates evidence, and what permits reuse?
- How does a merge policy handle unknown, stale, unsupported, failed, or incomplete checks?

### Product and deployment

- Local store versus shared service, and how do snapshots synchronize?
- ~~Agent interface: CLI, MCP, API, or several adapters?~~ Implemented: CLI with JSON output and an MCP server over the same commands.
- First editor and declaration-authoring experience?
- Open-source license and exact Lemma/Entail boundary?
- Project/package/domain naming availability?

## Decisions already clear enough to preserve

- (2026-11-01) Rules are Datalog in a Lemma-defined subset; Soufflé is the first engine, behind a pluggable interface with static dispatch.
- (2026-11-01) Users define the grammar; relationships are declared in YAML first or in `.ls` files.
- (2026-11-01) Spec-against-code verification comes first; behavioural verification later.
- (2026-11-01) The first pilot detects a breaking change in Gateway and its effect on the dashboard and SDKs; the IR-70 replay comes later.

- Lemma is the intended open-source framework name; Entail is the proposed hosted product name.
- The initial audience is experienced engineers using AI agents.
- Initial languages are TypeScript, Python, and Go; HTTP is the primary service boundary.
- Technical intent verification is the first scope; intangible requirements are deferred.
- Cross-project relationships must be explicit, queryable, and checkable.
- Relationships, requirements, and evidence are separate concepts.
- Existing languages/engines should be evaluated before inventing all semantics and tooling.
- LSP should consume the same graph/rule API as agents and CI.
- The starting extraction architecture is a Rust core, a lightweight Tree-sitter syntax index, and pluggable LSP semantic providers; SCIP is optional future input.
- Lemma owns the normalized symbol model and relationship semantics; upstream LSP is a semantic query interface, distinct from Lemma's future editor-facing LSP.
- The Seshat pilot is encouraging for local navigation but does not validate the full Lemma proposition.
