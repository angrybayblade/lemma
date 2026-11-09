# Relational grammar and verification

> A record of its time. YAML has since been removed: a description is always `.lm`. See [document 15](15-one-description-format.md).

Captured: 2026-11-01. Status: **established direction** where marked; the rest is **proposal**.

Lemma is the layer between agentic coding and formally verifiable systems. When an agent or engineer proposes a change, Lemma finds what the change touches across code, services and systems, derives which requirements apply, runs the verifiers that can check them, and reports findings with an explanation, stating the coverage and assumptions behind each one. This document defines the layers that hold that knowledge, the grammar users extend it with, the rule engine that evaluates it, and the first verifiers.

It builds on [Graph, relationships, and verification](02-graph-and-verification.md), which separates relationships, requirements and evidence, and on [Pluggable semantic providers](07-semantic-providers-and-indexing.md), which supplies the code layer.

## Motivating incident

IR-70 (2026-10-20): Gateway's Connect link lookup gained an enforced per-IP rate limit (60/min, #13829). The dashboard renders Hosted Connect server-side, so every end user's lookup came from the dashboard's two egress IPs and all customers shared one 120/min budget. About 20% of lookups got a 429 for 12.5 hours, a third at peak. The API contract did not change; its behaviour did. Four separately checkable facts would each have stopped it: the new limiter shipped enforced without shadow mode, it did not reuse the global limiter's exemption list, the dashboard multiplexes end users onto two IPs, and peak traffic (about 188/min) exceeded the shared budget. A later deploy also shipped code before its migration had run.

The pilot below starts simpler (contract breakage between Gateway and its consumers); behaviour such as IR-70 is the second stage.

## Layers

**Established direction (user):** LSP-powered local symbol graphs, a Tree-sitter relational layer, a service graph defined by OpenAPI or a similar grammar, and a Lemma relational grammar, generated once and then extended incrementally by people and LLMs, with verifiers written on top.

**Proposal:** treat the abstraction level of a fact and its source as two independent axes.

| Level | Example facts | Possible sources |
| --- | --- | --- |
| Code | definitions, calls, references, imports | Tree-sitter (complete, cheap), LSP or SCIP (resolved, partial) |
| Service | operations, handlers, request and response shapes, policies (rate limits, auth, retries), schemas, migrations | route extraction from code, OpenAPI, AsyncAPI, protobuf, SQL |
| System | which service calls which operation, from where (server or browser), deployments, egress, queues | call sites in code, infrastructure config, declarations, runtime observations |

```text
  verifiers          rules, contract diffs, capacity and ordering models, tests, runtime evidence
      │
  requirements       declared invariants and policies; obligations derived per change
      │
  system layer       cross-service relationships (derived, declared, observed)
      │
  service layer      operations, contracts, behaviour envelopes
      │
  code layer         Tree-sitter facts, enriched by LSP resolution where needed
      │
  snapshots          manifest of repositories at commits, configuration, specs, extractor versions
```

Tree-sitter is the base of the code layer: it covers every snapshot without a build. LSP resolution enriches selected edges (an unresolved call becomes a resolved symbol). Both write the same relations; provenance tells them apart. OpenAPI is one input to the service layer, not its format: many services have no spec, and a spec says nothing about behaviour such as rate limits.

### Fact sources

Every fact carries a source, a confidence, and the snapshot it holds in.

| Source | Meaning | Example |
| --- | --- | --- |
| `syntax` | extracted from source syntax | a route definition, a `fetch` call with a path literal |
| `semantic` | resolved by a language server or indexer | a call resolved to its target symbol |
| `spec` | read from a contract file | an OpenAPI operation |
| `declared` | written by a person or an LLM in YAML or `.ls` | "the dashboard multiplexes end users onto its egress IPs" |
| `observed` | measured at runtime or read from an environment | egress IPs, peak request rate, applied migrations |
| `inferred` | derived by rules from other facts | "this consumer reads a field the operation no longer returns" |

Derived facts are regenerated for every snapshot and never edited by hand. Declared facts are anchored to identities (symbols, operations, services) and checked against derived facts; a declaration that contradicts the code, or whose anchor changed, is reported. LLM output enters as a proposal until evidence or a person confirms it. Observed facts carry a time and go stale.

## Grammar

**Established direction (user):** users define the grammar themselves, and the grammar defines the relationships that can be declared, in YAML or in a custom Lemma spec file (`.ls`). YAML first.

A grammar declares:

- **Entity kinds**, with typed attributes: `Service`, `Operation { method: Method, path: Path }`, `Symbol`, `Field`.
- **Relation kinds**, with typed arguments, attributes, cardinality, and the sources allowed to produce them: `consumes(Symbol, Operation)`, `returns(Operation, field: Field)`.
- **Rules**, in Lemma's Datalog (below), deriving relations from others.
- **Requirements**: named rules whose results are findings (violations, unknowns), with severity and explanation templates.

Built-in kinds (symbols, files, references from the code layer) are part of a standard grammar that user grammars import and extend. A grammar compiles to typed relations: one table per relation kind, columns from the grammar plus source, confidence and snapshot.

YAML form:

```yaml
grammar: http-contracts
imports: [lemma/code]
entities:
  Operation: { attributes: { method: Method, path: Path, service: Service } }
relations:
  returns:  { args: [Operation], attributes: { field: Field }, sources: [syntax, spec] }
  reads:    { args: [Symbol, Operation], attributes: { field: Field }, sources: [syntax, semantic] }
rules:
  - removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).
requirements:
  consumer_reads_removed_field:
    when: reads(c, op, f), removed_field(op, f)
    severity: error
    explain: "{c} reads `{f}` from {op}, which no longer returns it"
```

`.ls` form (same meaning, syntax to be settled once YAML use shows the recurring patterns):

```text
grammar http-contracts imports lemma/code

entity Operation { method: Method, path: Path, service: Service }
relation returns(Operation, field: Field)          from syntax, spec
relation reads(Symbol, Operation, field: Field)    from syntax, semantic

rule removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).

require consumer_reads_removed_field error
  when reads(c, op, f), removed_field(op, f)
  explain "{c} reads `{f}` from {op}, which no longer returns it"
```

`@base` and `@head` select the snapshot of a change; a fact without a qualifier is in the head snapshot. Declarations (facts written by hand) use the same files.

## Rule semantics

**Established direction (user):** Datalog.

Lemma's Datalog is a defined subset, independent of any engine:

- **Core** (every engine): facts, rules, recursion, stratified negation, equality and inequality, snapshot qualifiers.
- **Optional** (engines declare support): aggregates (`count`, `sum`, `min`, `max`), arithmetic comparisons, derivation trees.
- **Excluded:** engine-specific extensions (Soufflé components, ADTs, user functors). A program using an unsupported feature is rejected at compile time with the rule that needs it, never run with a wrong answer.

Negation reads absence as falsehood only over relations whose coverage is complete for the snapshot (for example, every operation of a fully indexed service). Over incomplete relations a rule's result is reported as **unknown**, naming the missing coverage. This keeps "no consumer found" from becoming "no consumer exists".

Results are deterministic: the same snapshots, declarations, grammar, rules and engine version give the same derived facts, sorted before use.

## Engine

**Established direction (user):** Soufflé first, behind a pluggable interface, at zero runtime cost where possible.

```text
 YAML / .ls ──► grammar compiler ──► Lemma IR (engine-neutral)
                                       relations (typed), rules (AST), facts (interned u32 rows)
                                             │
                        ┌────────────────────┼────────────────────┐
                        ▼                    ▼                    ▼
                  Soufflé backend      reference evaluator   other engines
                  (first)              (tests, small runs)   (Nemo, native, later)
                        │
                        └──► derived relations + derivations ──► findings and evidence
```

- The `Engine` trait sits at the granularity of evaluating a program over a fact set, so dispatch cost is negligible either way. Callers are generic over the engine (`Verifier<E: Engine>`) and backends are Cargo features, so only enabled engines are compiled and calls are static.
- The real cost is moving facts. Strings are interned once in Lemma; facts are rows of `u32`. The Soufflé backend starts with tab-separated files and a process call; linking a compiled program in-process through Soufflé's C++ interface is a later optimization, cached by the rules' fingerprint.
- `Engine::capabilities` lists optional features; the compiler checks a program against them before running it.
- Derivations come back in Lemma's own format. Soufflé produces them with its provenance mode; engines without provenance can be explained after the fact by checking which rule instances fired.
- A small reference evaluator (semi-naive, stratified negation) runs in tests and cross-checks Soufflé on small programs, keeping the interface honest from the start.
- Soufflé is an external tool, detected like a language server and reported by `lemma providers`.

## Verifiers

A verifier takes facts and produces evidence for requirements (satisfied, violated, unknown, stale), bound to the snapshots, rules and tool versions it used.

**Established direction (user):** spec-against-code first, behavioural later.

1. **Spec against code**: an operation's declared contract (OpenAPI, or a declared contract in the grammar) against what the code implements and what consumers use: operations a consumer calls that no longer exist, request fields that became required, response fields consumers read that are no longer returned, changed status codes.
2. **Behavioural** (later): behaviour envelopes (policies such as rate limits, conditional responses, side effects), capacity and ordering rules with observed facts (the IR-70 family), then formal tools as verifiers where they fit: Alloy for relational models, TLA+ for orderings such as deploy and migration, SMT for numeric properties.

Rule results are explained by derivation; formal tools contribute evidence under the same requirements.

## Pilot

**Established direction (user):** detect a breaking change in Gateway and how it affects the dashboard and the SDKs. The IR-70 replay comes later.

- **Snapshots:** `example/platform` (Gateway) at a base and head commit; `example/workspace` (dashboard) and the SDK repositories at fixed commits, listed in a manifest.
- **Service layer:** Gateway operations (method, path, handler) and response fields, from route code, and from Gateway's OpenAPI document where it exists (spec against code).
- **System layer:** call sites in the dashboard and SDKs: HTTP method and path template, and which response fields they read.
- **Requirements:** a consumer calls an operation that no longer exists; a consumer reads a response field no longer returned; a request field became required.
- **Acceptance:** for a chosen Gateway change, Lemma lists the affected dashboard and SDK code with explanations, reports no findings for an unrelated change, and reports unknown where an extractor could not resolve a call (dynamic URLs).

## Pilot results (2026-11-01, observed)

Implemented on branch `feat/relational-grammar`: `lemma-logic` (Datalog IR, `Engine` trait, reference engine), `lemma-souffle`, `lemma-grammar` (YAML), `lemma-http` (OpenAPI and Tree-sitter extractors), `lemma-verify` and `lemma verify`. The built-in grammar is `grammars/http-contracts.yaml`.

Two real Gateway changes, with the dashboard, the SDKs (`sdk`) and the generated client (`platform-client`) at the commits current when each change merged (taken from `example/workspace` pins and history):

| Change | Expected | Lemma reported |
| --- | --- | --- |
| `fa87ef98a7`: `search` removed from `GET /api/v3.1/session_options` (strict schema, so sending it returns 400) | The TypeScript SDK's `SessionOptions.list` documents and passes `search`; the client's spec still declares it; the dashboard uses a different operation | 2 errors: `client_declares_removed_parameter` (platform-client) and `consumer_sends_removed_parameter` (`SessionOptions.list`, evidence: imports `sessionOptions.types.ts`, which names `search`). Nothing for the dashboard. |
| `048a74372a`: hook handler fields renamed (`prompt` to `condition`, `on_match` to `action`) | Breaking for hook users, but no consumer used hooks yet | No errors or warnings; 20 informational findings listing every caller of the changed session operations (both SDKs, dashboard), each noting it never names the renamed fields. |

About 56,000 facts per run (mostly field names and imports), 9 s with Soufflé, 52 s with the reference engine, identical findings. 228 informational `route_not_in_spec` findings are routes Gateway serves but keeps out of its public spec.

Limits found: field changes compare names, not types or enum values; URLs built from values the code does not show are not matched; "names a field" is evidence, not proof. The `example/workspace` repository already contains a module and route map (`map/graph.json`, 1,268 `calls` edges with evidence) that could be imported as a fact source or used to measure the extractors' coverage.

## Hill climb results (2026-11-01, observed)

**Recall of consumer-to-operation edges** against `example/workspace` `map/graph.json` (independent extractor), all repositories at the map's pins, test-only edges excluded, compared as (repository, module, path):

- Dashboard: 62 of 62 map edges found.
- sdk: 17 of 20 after adding JavaScript (`.mjs` scripts were not read). The 3 left are paths written in comments, which the map counts and Lemma rightly does not.
- The map assigns every method of a path when it sees the path; all 7 method disagreements checked by hand were the map's error.
- Lemma finds 116 sdk operations the map never links: SDK calls through the generated client (`this.client.x.y()`), which a path-only matcher cannot see. 5 of 5 sampled were real calls.
- One Lemma false positive, fixed: a documentation string `PATCH /api/...` matched every method on the path.

**Replays of service changes.** Every commit that touched Gateway's v3.1 or internal spec (94), consumers pinned at merge time; 9 s each, no failures:

| Change | Lemma | Ground truth |
| --- | --- | --- |
| `fa87ef98a7` `search` removed | 2 errors | confirmed (earlier pilot) |
| `e63c9fbdbf` premium renamed to Instant, no aliases | 6 warnings: dashboard still sends `enable_premium_usage`, reads `hosted_account_available`, `premium_usage_charge` | PR says the dashboard must adopt the names and that work "has no PR yet" |
| `2cd3e05c2c` tool pricing reshaped | 8 warnings in `getConsumerTools.ts`, `listTools.ts` | companion dashboard #1750, merged an hour later, changed exactly those two files |
| `1491525f78` playground key operation removed | nothing | dashboard had stopped calling it a week earlier (#1784); against the dashboard before that, 1 error at the exact line |
| 5 other field removals | nothing | no consumer named the fields, or no consumer called the operation yet; checked by hand |

No missed breakage found among the 94. New-required parameters and fields never occurred in this history, so those rules are still untested on real data.

**Replays of consumer changes** (new: a changed consumer is read at both commits). 68 dashboard commits touching Gateway paths, against Gateway as deployed at each: 1 error. The dashboard called `GET /api/internal/catalog/products` on 2026-09-01; Gateway served it from 2026-09-08 (#12449). True positive, zero noise.

**Fixes from the climb:** JavaScript consumers; method written before a path; spec present at only one commit; a removed operation reported once, not per field; consumer changes; no route scan without `routes`; `--relation` to dump derived relations.

**Still open:** removing a request field a consumer sends is a warning, though "no aliases" changes like `e63c9fbdbf` break writes, arguably an error; field types and enum values; service-to-service (Go); new-required rules unexercised.

## Languages and service-to-service results (2026-11-01, observed)

Go and Rust added (Rust throughout: model, syntax, rust-analyzer, verify). Platform testing:

- Gateway (TS) → Worker (Go) via a client generated from Worker's spec (`generated_from`, functions named by camel-cased operationId; `client_modules` for imported functions). Coverage vs `map/graph.json`: 35/35 operations.
- 156 Worker spec changes with Gateway at the same commits: 0 errors, 8 warnings, all false positives on common names (`id`, `slug`, `product(s)`) where Gateway, updated in the same commit, uses the name for its own objects. Response-field warnings need read-versus-write evidence to fix (an object key is a write; a member access or destructuring is a read; schemas are ambiguous).
- Final re-sweeps with all fixes: Gateway 94 changes → the same three confirmed breaks (2 errors, 8 warnings); dashboard 68 changes → the same one confirmed unserved call.
- Qdrant (Rust, actix): 69/69 spec operations found among routes; 33 undocumented debug routes reported.

Open items resolved:

- **Severity of a removed request field**: graded by evidence. Error when the operation rejects unknown fields (spec `additionalProperties: false`, or `rejects_unknown_fields` declared on the service); otherwise a warning that the field is now ignored. Zod `.strict()` is now read from route code (route schema uses, strict constants, unions, through relative and `@/`/`~/` alias imports, plus `schemas` directories and `route_aliases` for Gateway's `/api/v3/` → `/api/v3.1/`). On Gateway at fa87ef98a7 the code shows the same 17 strict bodies as the spec among published operations, plus 6 strict query schemas the spec cannot express; removed parameters are now graded the same way (error if the query is strict, else a warning).

False positives on common names (Worker sweep, 8 warnings): judged acceptable for the POC because they only affect warnings (no false error in 318 real changes; errors gate CI, warnings do not) and cluster in commits that change service and caller together.
- **New-required rules on real data**: none fired because requiredness was mis-read (zod-openapi omits `requestBody.required`; union branches were never required). Fixed; the history then has one newly required field (Worker `f64314e152`, `auth` on tool sync). On the real commit Gateway was updated together (no finding); with Gateway pinned before it (entries may pin their own commit), `consumer_misses_new_required_field` fires at the caller.

Other fixes: spec added or deleted by a change read as unchanged (deleting a spec had reported 73-83 removals); use evidence scoped to the calling function plus declarations outside functions; specs and routes accept paths without `/api/`.

## Open questions

- Identity across layers and repositories: how a symbol connects to an operation, service and deployment, and survives renames.
- How Lemma's IR maps generated SDK clients to operations (cheap if generated from OpenAPI).
- `.ls` syntax details, and how grammars are versioned and shared.
- Which observations are worth importing first (deploy topology, telemetry, migration state), and how their staleness is set.
- Incremental evaluation as snapshots change: re-run per change at first; differential evaluation later if needed.
