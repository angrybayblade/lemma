# Vision and scope

## Established direction

Build an open-source framework for **intent-verifiable software dependency graphs**. The end consumer is an experienced software engineer using AI agents to write, review, scope, and merge code faster, with technical verification of the intended change.

The initial environment is TypeScript, Python, and Go, with service interactions primarily over HTTP. Relationships also need to include infrastructure eventually.

The original context problem is that an agent often sees a localized portion of a codebase without the dependencies, parallel implementations, contracts, or operational context necessary to make a complete change. Lemma should expose a relevant, bounded context set backed by identifiable source evidence.

## Intended outcomes

- Find implementations through full-text search, pattern matching, and AST or semantic symbols.
- Traverse dependencies within repositories and across service boundaries.
- Locate related SDK implementations, API contracts, backend handlers, and relevant infrastructure.
- Scope the consequences of a proposed change.
- Turn declared technical requirements into explicit verification obligations.
- Explain why a file, service, contract, or check is relevant.
- Report which obligations are satisfied, violated, unknown, or stale for the proposed revisions.
- Let engineers inspect and correct the declarations and evidence driving those results.

Speed is an outcome to measure, not an assumption. A graph that produces excessive context or false obligations can make agents slower.

## Technical intent first

The conversation initially explored verification of intangible or business requirements. That work is explicitly deferred.

Initial requirements should have concrete technical interpretations, such as:

- An SDK consumer conforms to an HTTP operation's request and response contract.
- TypeScript and Python implementations preserve a defined subset of behavior.
- A service respects an architectural dependency boundary.
- A changed contract has compatibility evidence for its declared consumers.
- Evidence applies to the actual candidate revisions, rather than an earlier build.

“Verified” must state what property was checked and with what evidence. Passing tests, graph consistency, and formal proofs establish different things.

## Local graphs and composition

The proposed progression is:

1. Extract local symbols and relationships from each codebase.
2. Compose them into service-level knowledge.
3. Declare cross-service and infrastructure relationships using a formalized relationship model.
4. Build traversal, context selection, validation, and evidence tools on top.

Although the initial discussion used “dependency trees,” the general representation must support graphs: shared dependencies, cycles, multiple relationship types, and multiple consumers.

## Determinism

Determinism should mean that the same source snapshots, declarations, extractor versions, rules, and configuration produce the same normalized facts and results.

This does not make every fact correct. A deterministic heuristic can consistently infer a wrong edge. Extracted facts, inferred candidates, human declarations, and verification results must remain distinguishable.

AI may help propose relationships or author declarations. Unreviewed model output should not silently become authoritative verification evidence.

## Lemma and Entail

Lemma is the reusable open-source core. The proposed product boundary keeps local graph querying, verification, and CI usable without a hosted service.

Entail could add managed indexing, repository connections, shared infrastructure, coordinated checks, team workflows, and hosted change-impact/evidence views. Pricing, licensing, tenancy, and the exact product split remain undecided.

## Non-goals for the first milestone

- Proving arbitrary program correctness.
- Verifying broad natural-language business intent.
- Replacing TypeScript, Python, or Go language servers.
- Inferring all HTTP and infrastructure dependencies automatically.
- Treating absence from an incomplete index as evidence of no dependency.
- Automatically performing distributed refactors across repositories.
