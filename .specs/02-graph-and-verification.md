# Graph, relationships, and verification

Status: proposed design direction; vocabulary and syntax are illustrative.

## Separate relationships, requirements, and evidence

| Concept | Meaning | Example |
| --- | --- | --- |
| Relationship | A connection between entities | An SDK method consumes an HTTP operation |
| Requirement | A property that must hold | Its request must conform to the operation's contract |
| Evidence | A result supporting a property for specific inputs | A contract check passed for a client/server revision pair |

Declaring a relationship does not verify it. A graph assertion about existing code is also different from an obligation about how code must behave.

## HTTP operations as shared entities

Prefer explicit API operation entities over connecting every consumer directly to every implementation:

```text
TypeScript deletion helper --consumes--> Session deletion API
Python deletion helper    --consumes--> Session deletion API
Backend handler           --implements-> Session deletion API
```

Additional relationships can express parity between the two SDKs. Sharing an operation does not imply complete behavioral equivalence: retry behavior, error translation, cancellation, and runtime semantics may differ intentionally.

The generic platform example motivating this model is:

```text
DELETE /api/v3.1/task_router/session/{id}
```

The SDK repository contains both TypeScript and Python deletion helpers for this operation. The pilot did not inspect or resolve the backend handler.

## Candidate entity types

- Repository and revision/snapshot.
- Package, module, service, and source symbol.
- API operation and contract/schema.
- Infrastructure resource.
- Relationship declaration and extracted observation.
- Requirement, checker, verification run, and evidence record.

These are candidates, not a requirement to implement every entity type in version one.

## Candidate relationship vocabulary

- `depends_on`: a dependency whose precise semantics must be specified by subtype or context.
- `consumes`: a symbol or component uses an operation or contract.
- `implements`: a symbol or component implements an operation or contract.
- `publishes`: a component produces an event or interface.
- `must_match`: an explicitly scoped parity requirement.
- `verified_by`: associates a requirement with an applicable check or evidence record; the exact separation of check definitions and results remains open.

Avoid treating these as interchangeable edges. For example, changing an implementation without changing its contract may require different checks from changing the contract itself.

## Authoring format

**Decided 2026-11-01** (see [Relational grammar and verification](08-relational-grammar-and-verification.md)): users define the grammar themselves; relationships are declared in YAML first, or in custom `.ls` files with the same meaning. Rules are Datalog.

Earlier discussion: the user's proposed approach is a custom grammar for relationships and rules over local symbol graphs. The recommendation was to establish the model and semantics first and initially use YAML/JSON, potentially validated by CUE or JSON Schema. A custom syntax can follow once recurring expressions and usability needs are understood.

Illustrative declaration:

```yaml
relationship:
  from: sdk-ts::deleteTaskRouterSession
  kind: consumes
  to: task-router::deleteSession

requirements:
  - request_conforms_to_contract
  - response_conforms_to_contract

verification:
  - checker: openapi-compatibility
  - checker: session-deletion-contract-tests
```

This is not an implemented grammar. The identifiers are conceptual references and are not yet a defined symbol addressing scheme.

## Identity and resolution

A symbol reference must resolve within a repository/package and a specific source snapshot. Display names alone are insufficient.

The design must address:

- Overloaded or same-named symbols.
- File moves, symbol renames, and deleted symbols.
- Package versions and generated clients.
- Different language indexers and their identity conventions.
- Conditional exports and runtime-specific implementations.
- Missing repositories and unavailable revisions.
- Ambiguous resolution, which should be surfaced rather than silently guessed.

Logical identity across revisions and a concrete occurrence in one revision are related but distinct concepts. Their representation is not yet chosen.

## Deriving obligations

Datalog is the chosen rule language; document 8 defines Lemma's subset and the engine interface. Illustrative Datalog rule bodies, with declarations omitted:

```prolog
Consumes("sdk-ts::deleteSession", "api::deleteSession").
Implements("backend::deleteSession", "api::deleteSession").
Changed("backend::deleteSession").

NeedsCompatibilityCheck(Consumer, Operation) :-
    Changed(Handler),
    Implements(Handler, Operation),
    Consumes(Consumer, Operation).

MissingEvidence(Consumer, Operation) :-
    NeedsCompatibilityCheck(Consumer, Operation),
    !PassingCompatibilityEvidence(Consumer, Operation).
```

The example demonstrates derivation, not a production compatibility policy. A real evidence relation must bind checker identity, rule version, configuration, and all relevant input revisions. Negation must operate over an explicitly bounded and understood fact set.

## Evidence and completeness

Candidate outcomes:

- **Satisfied:** applicable evidence establishes the stated property within the check's scope.
- **Violated:** an applicable check found a counterexample or failure of the requirement.
- **Unknown:** evidence, inputs, extraction coverage, or resolution is insufficient.
- **Stale:** evidence exists but does not apply to the current inputs.

Missing evidence does not establish a behavioral violation, although a merge policy can reject a change until the evidence exists.

Each source snapshot should expose extraction coverage and failures. “No consumer found” is not equivalent to “no consumers exist” when repositories, languages, runtime conditions, or extractors are missing.

## Verification boundaries

- Graph validation checks properties of the supplied graph.
- Contract checks establish properties within their schema/compatibility semantics.
- Tests establish observed behavior for exercised cases.
- Formal analyses establish specified properties under explicit assumptions and bounds.
- Rule provenance explains why an output follows from supplied inputs; it does not prove those inputs faithfully represent the software.

## Shared engine

The same versioned graph, rule semantics, and evidence interpretation should power editor features, agent tools, and CI. Front ends should not independently reimplement the rules.
