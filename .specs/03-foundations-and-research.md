# Reusable languages and projects

Research consulted during the discussion on 2026-10-28. These are candidate foundations, not adopted dependencies.

## Shortlist

| Foundation | Reusable capability | What Lemma still supplies |
| --- | --- | --- |
| Datalog / Soufflé | Typed relations, recursive analyses, derived facts, violation queries, provenance | Software model, symbol resolution, snapshots, persistence integration, check execution |
| RDF + SHACL + SPARQL | Graph identities, relationships, queries, constraints, structured validation reports | Code binding, coverage semantics, revision model, executable verification integration |
| TypeDB / TypeQL | Typed entities, relations, role players, attributes, database queries | Extractors, domain semantics, checks, evidence lifecycle |
| OPA / Rego | Policies over structured snapshots, reusable modules, graph reachability | Graph construction, recursive analysis strategy, evidence production |
| Alloy | Relational specifications, invariant analysis, counterexample search | Operational graph store, source extraction, editor/CI workflows |
| CUE | Declarative data/schema constraints and composition | Graph traversal and software verification engine |

## Datalog and Soufflé

The leading recommendation for the analysis/rules layer was Datalog, with Soufflé as a concrete engine to evaluate. Dependency propagation and obligation derivation naturally map to relations and rules.

Soufflé is designed for declarative analyses, including static program analysis. Its provenance facilities can help explain why a particular consumer or verification obligation was derived.

It does not provide the entire product: Lemma still needs repository adapters, identity resolution, versioned inputs, check runners, and useful explanations for engineers. Incremental updates across editor changes must be evaluated rather than assumed from the engine choice.

Sources:

- [Soufflé tutorial](https://souffle-lang.github.io/tutorial)
- [Soufflé provenance](https://souffle-lang.github.io/provenance)
- [Source repository](https://github.com/souffle-lang/souffle)
- [Provenance for Large-scale Datalog](https://arxiv.org/abs/1907.05045)

## RDF, SHACL, and SPARQL

This is the closest standards-oriented stack to the proposed relationship language: RDF represents entities/relationships, SPARQL queries them, and SHACL validates graph constraints and emits structured reports.

Examples include requiring an API contract for every operation, valid targets for every relationship, and input/checker metadata for every evidence record. pySHACL offers a Python implementation for experimentation.

Adopting this stack commits the model to RDF conventions. Recursive shapes require care: the cited SHACL Recommendation leaves recursive-shape validation semantics to implementations. Evidence orchestration and revision semantics remain Lemma's work.

Sources:

- [W3C SHACL Recommendation](https://www.w3.org/TR/shacl/)
- [pySHACL](https://github.com/rdflib/pyshacl)
- [RDFLib](https://github.com/rdflib/rdflib)
- [A Review of SHACL: From Data Validation to Schema Reasoning for RDF Graphs](https://arxiv.org/abs/2112.01441)

## TypeDB and TypeQL

TypeQL supports typed entities, relations, roles, and attributes. A consumption relation could explicitly assign consumer and operation roles, with related context or evidence.

This is attractive when the domain relationship model should be native to the database. It implies a stronger database commitment than a portable fact format plus an independent analysis engine.

- [TypeQL schema definitions](https://typedb.com/docs/typeql-reference/schema/define/)
- [TypeQL reference](https://typedb.com/docs/typeql-reference/)

No TypeDB prototype or deployment evaluation was run.

## OPA and Rego

Rego is a candidate for merge policies such as rejecting a candidate whose affected public operations lack current compatibility evidence. It evaluates structured data and includes graph reachability built-ins.

It is best considered as a policy consumer of a prepared graph snapshot. Do not introduce both Rego and Datalog initially unless they solve distinct demonstrated needs.

- [Rego language](https://www.openpolicyagent.org/docs/policy-language)
- [Graph built-ins](https://www.openpolicyagent.org/docs/policy-reference/builtins/graph)

## Alloy

Alloy is relevant for exploring whether the relationship/rule model admits unwanted states and for finding counterexamples to proposed invariants. It is not the default choice for the operational code-graph store.

Bounded searches must be interpreted within their bounds. Model correctness and implementation correctness remain separate obligations.

- [Alloy](https://alloytools.org/)
- [Alloy specification](https://alloytools.org/spec.html)

## CUE

CUE could validate and compose author declarations before they enter the graph/rule engine. It offers an existing language and tooling, reducing the need for an early custom parser.

- [CUE introduction](https://cuelang.org/docs/introduction/)
- [Validation tour](https://cuelang.org/docs/tour/basics/validation/)

## Structurizr

Structurizr DSL is relevant as an architecture-authoring and visualization reference. It models systems, components, and relationships with properties and perspectives. It does not by itself supply the proposed source-symbol binding and verification-evidence workflow.

- [Structurizr DSL](https://docs.structurizr.com/dsl)
- [Perspectives](https://docs.structurizr.com/dsl/cookbook/perspectives-static/)

## Proposed comparison

Evaluate Soufflé and SHACL using the same small HTTP consumer/provider example:

- Express entities and typed relationships.
- Resolve declared source references.
- Identify affected consumers after a change.
- Derive required checks.
- Report missing/stale evidence and graph violations.
- Explain each result.

Prefer Soufflé if recursive program/dependency analysis dominates. Prefer SHACL if portable graph contracts and interoperability dominate. This is a recommendation, not a completed evaluation.

License and distribution compatibility should be checked against the exact selected versions before adoption; this record is not a licensing assessment.
