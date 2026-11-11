# Change-driven evaluation

**Status: implemented, 2026-11-05** (on `main`; the classifier was later rebuilt to judge the rules' structure, see `crates/lemma-verify/src/demand.rs`). Written as a proposal on 2026-11-04; the measurements come from the `perf/fact-cache` branch (commit 9196eb6) on the generated 100k-file system (`bench/lemmabench/scale.py`: 20 services, 20 consumers, `lemma check --change '#svc0'`).

## Problem

After the fact cache work, a warm check at 100k files takes 7.5 s. Almost all of it is spent on files the change cannot affect:

| Phase (main thread, warm) | Time | Scales with |
|---|---|---|
| Decode kept code facts | 0.43 s | every file |
| Write Datalog facts, resolve imports | ~0.7 s | every file |
| Soufflé evaluation | 1.06 s | every fact |
| Decode tracing's functions | 0.5 s | every file of every traced repository |
| Tracing, rest | ~1.0 s | every function |
| Listing, keys, setup | ~0.5 s | every file (cheap per file) |

A change touches a handful of operations. Every finding in the built-in grammar either:
1. names a consumer that calls one of those operations;
2. comes from a file the change edited; or
3. is read from the small set of files that are read whole anyway (routes, schemas, generated clients).

Today every file's facts are loaded, written and evaluated, and then the requirements throw away everything not tied to the change.

**Goal.** Make the cost of a warm check scale with the change and its callers, not with the system. The result must be byte-for-byte the same report as full evaluation. When Lemma cannot prove a requirement change-local, it falls back to full evaluation for that grammar.

**Target.** At 100k files, warm under 2 s, and the same order at 1M files. What remains O(files) is listing and key computation, which are cheap per file.

## Shape

```text
 spec facts (all) ──► phase 1: evaluate the spec-only rules ──► Δ = operations the requirements anchor on
                                                                   │
 call index (SQLite, per unit) ◄── look up Δ's paths and call names ┘
        │
        ▼
 selected files = callers of Δ + their context + files the change edited + always-read files
        │
        ▼
 phase 2: load and write only the selected files' facts ──► evaluate the full program ──► findings (identical)
        │
        ▼
 tracing: asks for the callers of further operations round by round, answered the same way
```

## 1. Which requirements are change-driven (grammar analysis)

The grammar already tags every relation with its sources (`spec`, `syntax`, `declared`). The compiler classifies relations and requirements as follows. The analysis is generic, so custom grammars get the optimisation when they qualify and full evaluation when they don't.

- **Phase-1 relations.** Derived relations whose dependency closure contains only `spec` and `declared` facts, for example `removed_operation`, `new_required_parameter`, `kept_operation` and `rejects_unknown`. Exception: `rejects_unknown` also reads `strict_route`, which comes from syntax facts, so it is not phase-1. Phase 1 evaluates these with the in-process reference evaluator. They are small, and an in-process run avoids a Soufflé process.
- **Always-read relations.** Code relations whose facts come from files Lemma reads whole regardless of the change: service route and schema directories, and client repositories. These are `route`, `route_schema`, `inline_strict_route`, `strict_schema`, `union_member`, `client_operation` and `client_parameter`. The extractor declares this per fact-writing site, not the grammar, because only the extractor knows which file kinds write which relations.
- **Selected relations.** Code relations written from consumer files: `calls_path`, `calls_client`, `mentions`, `in_file`, `named`, `imports`, `addressed_to` and `part_of_service`.
- **Anchored requirement.** A requirement whose body has a positive literal over a phase-1 relation with an operation variable `op`, where every selected-relation literal (positive or negated, directly or through derived relations) is connected to that `op` through a positive `calls`, `references_operation` or `client_operation` literal. **Δ is computed, not declared:** each call in a requirement's body has a group of anchoring literals, the phase-1 literals sharing its `op`, and an operation the call can be of is in every relation of its group. Δ is the union over all groups of the intersection of each group's `op` values. A group holding a relation that does not depend on the change (`rejects_unknown` beside `removed_request_field`) is therefore bounded by its change-dependent relations, rather than adding every operation that rejects unknown fields. So for every operation an anchored requirement can fire on, the callers are guaranteed to be loaded.
- **Change-local requirement.** Its selected literals are guarded by a negated literal over the same code relation at `@base` with the same consumer variable. The built-in example is `unserved_call` with `!called_before(c, m, p)`. Such a requirement can only fire on facts that differ between base and head, which means files the change edited.
- **Always-read requirement.** It uses only phase-1, spec, declared and always-read relations, for example `route_not_in_spec`, `client_declares_removed_parameter` and `client_calls_removed_operation` for hand-written clients.
- **Anchored absence.** It says an operation is called by nothing: a negated literal over a relation each rule of which copies the operation column of one call (`called_at_head(op) :- calls_at_head(_, op, _, _)`), whose `op` a positive phase-1 literal holds, and no other selected literal. Its anchor joins Δ, so every caller of those operations is loaded and the absence is complete. The built-in example is `operation_without_consumer`, anchored by `added_operation`.
- **Anything else** makes the whole grammar run in full mode. The reason is reported under `--explain`.

**Context reach.** Negated selected literals such as `!sends(c, leaf)` are complete only if every fact the rules for `sends` can reach for `c` is loaded. The compiler computes that reach from the rules:
- facts of `c`'s own file, through `in_file`;
- files that file imports, through `imports`: one level in the built-in grammar, and the transitive closure if a rule recurses over `imports`;
- other functions of the same file, through `local_callee`, which come with the file anyway.

The selection loads exactly that reach.

**The built-in grammar today:**

| Requirement | Class |
|---|---|
| `consumer_calls_removed_operation` | anchored: `removed_operation` |
| `consumer_misses_new_required_parameter` | anchored: `new_required_parameter` |
| `consumer_misses_new_required_field` | anchored: `new_required_request_field`, plus `!sends` within context |
| `consumer_sends_removed_parameter` / `consumer_sends_ignored_parameter` | anchored: `removed_parameter`; `rejects_unknown_parameter` reads route facts, which are always read |
| `consumer_sends_rejected_field` / `consumer_uses_removed_request_field` | anchored: `removed_request_field` |
| `consumer_uses_removed_response_field` | anchored: `removed_response_field` |
| `consumer_calls_changed_operation` | anchored: `removed_request_field` |
| `client_declares_removed_parameter` | always-read: client parameters |
| `client_code_sends_removed_parameter` | always-read: client parameters and the names client methods use |
| `client_calls_removed_operation` | anchored: `removed_operation`; generated clients' `client_operation` rows come from the consumer files that call them, which are selected through the name index |
| `consumer_calls_unserved_operation` | change-local |
| `route_not_in_spec` | always-read |
| `operation_without_route` / `route_without_operation` | always-read |
| `operation_without_consumer` | anchored absence: `added_operation` |
| `consumer_calls_unrouted_operation` | anchored: `unrouted_operation`, which reads routes, always read |

So the built-in grammar runs change-driven with no annotations.

## 2. The call index (persistent)

The index lives in the same `units.db` as the facts it describes, and is written in the same background transaction as each code-facts unit.

```sql
CREATE TABLE unit_terms (term TEXT, unit TEXT, PRIMARY KEY (term, unit)) WITHOUT ROWID;
CREATE TABLE indexed_units (unit TEXT PRIMARY KEY) WITHOUT ROWID;  -- every unit whose terms are written
```

The terms are a superset of everything fact writing can turn into a call. They are taken from `CodeFacts` before any cross-file resolution:

| Term | From | Why it is a sound key |
|---|---|---|
| `path:<p>` | `path_references[].path`, `method_paths[].path` | `calls_path` is written with exactly these paths, and the rules match them to an operation's path by equality. A `*` method matches by path alone, which the term also covers. |
| `chain:<c>` | `client_calls[].chain` | `calls_client` is written with these chains, and `client_operation` maps a chain to a method and path. |
| `pair:<r>.<n>` | normalized resource and last part of each `member_calls[].parts` with three parts or more | Client method resolution (`Clients::resolve`) requires the call's last name to equal the method's name and the resource before it (the last part between the root and the method longer than one letter) to be one of the method's resource names. An operation is looked up by `pair:` for each resource name of each client method making it. |
| `name:<n>` | normalized last part of each `member_calls[].parts` with at most two parts rooted at an import, and each imported name | Generated-function resolution (`Clients::named_operation`) requires the call's last name, or the imported name, to equal the function's name. Re-export and alias resolution only change the module, never that name. |
| `reexports` | `reexports` is non-empty | `generated_reexports` needs every re-exporting file of a consumer, whatever the change. Those files are always loaded. |

**Looking up Δ.** For each operation in Δ:
- its canonical path;
- the chains that `client_operation` maps to it (client facts are always read);
- the method names of hand-written clients for its method and path;
- its generated names under the repository's `call_names` templates (`{operationId}`, `{camel}`, …).

The lookup returns unit keys. These are intersected with the current snapshot's unit keys, which come from the listing and blobs (`unit_key`), at each snapshot read.

**Coverage.** Selection is sound only if every current unit is indexed. Before selecting, Lemma checks the current keys against `indexed_units` (batched `IN` queries over a small table). Missing units (new or edited files, a cold cache, or a different build) are parsed and indexed first, exactly as now. A run never selects from an index it cannot prove complete.

**Size.** At 100k files, roughly 5 to 20 terms per file gives 0.5 to 2M short rows, tens of MB beside 347 MB of facts. To be measured in rollout step 1.

## 3. Phase 2: selected fact writing

`extract_all` takes a `Selection` per repository snapshot. It is either *all files*, or a set of paths:
- index hits for Δ;
- their context, following the reach rules from section 1 through the selected files' `imports` facts, resolved as `write_imports` resolves them;
- every file whose blob differs between base and head in the changed repository, including added and deleted files;
- the `reexports` files;
- the always-read files (route, schema and client directories).

Spec facts are always written whole. Fact writing for a selected file is unchanged, so its rows are identical to full mode.

**Relations a caller asks for**, such as `--relation calls` or check's traced relations, are restricted to Δ in this mode. `verify --relation …` without a change stays in full mode. Check, the only caller of restricted relations, gets them through the tracing interface below.

## 4. Tracing on demand

Tracing currently takes every `calls` and `references_operation` row (`affects::walkable`), because a trace that reaches a route handler in service S must continue to S's callers, for operations that are not in Δ.

Replace the bulk relations with a lookup: `callers(ops) -> Vec<Call>`. Each round runs phase 2 for just those operations (selection by index, then evaluation of the `calls` rules), so the matching rules stay in the grammar and are never re-implemented in Rust. The number of rounds is the cross-service depth of the trace, usually 1 to 3. Each round costs a small evaluation.

Tracing's own per-repository function load (0.5 s decode at 100k) gets the same treatment from a second index: `mention:<name>` terms on functions units. `NameIndex` then loads only the functions that mention a name the walk is following, instead of a whole repository's functions. That is a later step, measured separately.

## 5. Drift mode

`lemma check` without `--change` compares committed documents to what an adapter reads from code. Its Δ is the operations where the overlay differs from the committed document, and it goes through the same phase 1. Nothing else changes.

## Correctness argument

Write F for findings and R for requested relations. For an anchored requirement:
1. Any derivation of it binds `op` through a phase-1 literal, so `op ∈ Δ` by construction.
2. Its consumer facts are reached through `calls` or `client_operation` for that `op`. Every file whose facts can produce such a row carries a term for `op` (section 2), so it is selected.
3. Every negated selected literal is keyed by that consumer, and its reach is loaded.
4. Facts from unselected files can only add rows whose consumer is not linked to any `op ∈ Δ`, and no anchored requirement can use those rows.

Change-local requirements fire only on rows that differ between snapshots, and those come from edited files, which are always selected. Always-read requirements see the same facts in both modes.

The parts most likely to break this argument:
- an extractor change that writes a call without the matching term;
- a resolution path that changes a call's last name.

Both are guarded by the rollout below, not by trust.

## Rollout

1. **Index only.** Write terms with units, with no behaviour change. Measure write cost and index size, and add a unit test per term kind.
2. **Classification.** Compiler analysis plus phase 1 behind `--evaluation demand`, which falls back to full and says why. Add unit tests on the built-in grammar and on a grammar that must fall back: a requirement over `calls` with no anchor.
3. **Selection.** Phase 2 for verify and check findings. **Differential gate:** `--evaluation both` runs both modes and fails on any difference. Run it over:
   - the 54 generic platform replay cases;
   - the 86 mutation cases;
   - the fixtures;
   - the 10k and 100k synthetic systems;
   - a new synthetic variant with multi-hop service chains, generated clients, re-exports and import-only field use, so every selection path is exercised.
4. **Tracing rounds**, then the functions mention index. Same gate.
5. **Default to demand** once the gate has been clean on all of the above. Keep `--evaluation full` permanently, as the fallback and as a CI differential job on the fixtures.

## Open questions

- **Interning:** whether phase 1 should reuse the facts Soufflé already receives or keep its own small interner. Leaning towards its own, since the spec facts are small.
- **Restricted relations:** whether `verify --relation calls` with a change should offer a restricted mode. Leaning no: whoever asks for a whole relation expects all of it.
- **Unvouched files:** untracked and edited files are read and hashed on every run, as now. With many of them, the index helps only after they are parsed. That is acceptable, because they are what changed.
- **Page size:** `unit_terms` rows are small. They may want their own database file if 64 KiB pages hurt point lookups. To be measured in rollout step 1.
