# Native index and evidence store

**Superseded in its physical design by [spec 17](17-lemma-store.md), 2026-11-10.** The logical rules here still stand.

**Status — proposal, 2026-11-04.** The intended product interface is a reusable `.lm` module defining configuration, domain relations, behaviours and verification entry points. This document proposes a **native primary datastore** for that product. The on-disk layout, rollout and performance gates below are design choices to evaluate. No native implementation or native performance measurements exist in this review.

## Decision and first workload

Build the store around **fork a pinned workspace → apply an edit → identify the affected obligations → return evidence for the candidate**. A workspace can contain TypeScript, Python and Go repositories, their HTTP contracts, and declared infrastructure relationships. Two agents may fork the same workspace and arrive at different correct answers. Each must be able to explain its result against its own inputs.

Use immutable, range-partitioned packs of typed facts, small branch overlays, and published workspace roots. Store occurrences, relationship indexes and verification dependencies in the same snapshot model. A standalone native store must be able to index, close, reopen, fork and verify without a SQLite database. The current backend remains available during development as a migration source, correctness comparator and performance control.

**First slice.** A TypeScript client adds a call to a Python HTTP operation previously certified as unused. On the child branch, Lemma identifies the new call site, withdraws the old certificate, produces the new verdict and links it to source. On the parent, the original answer remains valid. Repeat with a Go caller, a second independent call site, and a dynamic target the resolver cannot identify. This slice exercises storage, binding and invalidation before advanced compression or a new language front end.

## Precedents and implementation choices

These are source and documentation reviews. Comparisons below describe which design to borrow and where Lemma would still have work to do.

| Project | Relevant mechanism | Decision for Lemma |
|---|---|---|
| Meta Glean | Typed facts, lookup by fact key, ownership units, stacked databases, and visibility of derived facts under removal of an owner. [Database representation](https://glean.software/docs/implementation/db/) · [Incrementality](https://glean.software/docs/implementation/incrementality/) | Adopt the ownership semantics. The same tuple can have several independent supports. Replacing one file retracts that file's support. Lemma also needs to record coverage, observations and dependencies on selections that returned zero rows. |
| TerminusDB's Rust triple store | A base layer plus immutable additions and removals. Rollup reduces read amplification while retaining history. [Layer model](https://docs.rs/terminus-store/latest/terminus_store/layer/index.html) · [Rollup API](https://docs.rs/terminus-store/latest/terminus_store/store/struct.StoreLayer.html#method.rollup) | Closest physical precedent. Borrow base/overlay/rollup. Keep native typed tuples of arbitrary arity. Encoding a field read, call site and supporting observation as triples would make reconstructing these records part of the hot path. |
| Software Heritage `swh-graph` | Forward and transposed compressed graphs, mapped graph files, and independently loadable properties. Compression is a separate build with a real memory cost. [Rust graph API](https://docs.rs/swh-graph/latest/swh_graph/) · [Compression pipeline](https://github.com/SoftwareHeritage/swh-graph/blob/master/docs/compression.rst) | Borrow dense IDs, directional adjacency and lazy metadata. Use inexpensive local packing during edits. Evaluate heavier compression for sealed, frequently reused bases. |
| Git commit-graph | Durable object names map to dense integer positions. Readers can walk split graph files while a writer publishes a new chain. [Commit-graph design](https://git-scm.com/docs/commit-graph) | Keep physical ordinals private, publish roots atomically and retain packs reachable by existing readers. Lemma overlays must also handle replacement and deletion. |
| Rust compiler and Differential Dataflow | The compiler records query reads and compares stable fingerprints. Differential Dataflow retains indexed updates to evaluate changes across versions, including iterative computations. [Rust compiler guide](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html) · [Differential Dataflow paper](https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf) | Adopt read tracking and result fingerprints. Start with conservative reevaluation through Lemma's existing engines. Consider a differential executor only if recursive reevaluation is a measured bottleneck. |
| redb | Copy-on-write B+ trees, pinned read transactions and deferred page reclamation. [redb design](https://github.com/cberner/redb/blob/master/docs/design.md) | Credible alternative if small random writes dominate. Packed segments are the proposed default because neighbouring values can be read together across shared snapshots. Revisit page trees if overlay maintenance dominates the measured edit loop. |

Reuse the Tree-sitter extractors, LSP adapters, grammar compiler, BLAKE3 dependency and Datalog engines already in the workspace. Lemma owns the native file format, binding rules, ownership model, coverage contract and evidence lifecycle. The proposed block format works with positional reads and with mappings, so those I/O strategies can be compared on identical bytes.

## Snapshot and fact model

Separate three identities. All logical digests use versioned, domain-separated canonical encodings of typed values. Normalize ordering explicitly and encode string lengths, signed integers and absent values unambiguously.

| Identity | Meaning |
|---|---|
| Input world | Digest of repository identities, mounts, revisions and dirty file contents, dependency/configuration inputs, provider and extractor profiles, compiled module versions, and explicit observation bundles. Capture the environment inputs a producer reads, or declare its scope opaque. |
| Fact snapshot | Input world plus logical roots of visible tuples, supports, bindings and coverage. Provider enrichment can publish a new fact snapshot for the same source world. Canonical key partitions make the logical digest independent of pack boundaries, insertion order and compaction. |
| Physical generation | A manifest locating the packs, indexes, evidence and retained artifacts for a named head. A repack may change this manifest while preserving the logical fact snapshot. Verification records bind **both** base and candidate fact snapshots when a rule compares them. |

**Entities and occurrences.** A repository-local producer owns a unit such as a source file, contract document or infrastructure inventory. A syntax occurrence identifies its unit, source blob, UTF-8 byte range, role and discriminator. Keep line-start tables for protocol conversion. Two call sites to the same target remain two occurrences. A language reference becomes a call only when the producer establishes that role.

Capture dirty files and editor buffers as immutable content when pinning the candidate. Committed bytes may be fetched from Git by blob identity if the store guarantees retention, otherwise retain a copy. Verify the digest before using source bytes for queries or explanations. A missing blob is an unavailable input. Falling through to the current worktree could make a past receipt point to different code.

Within a segment, use dense `u32` slots for files, symbols, strings and bindings. Split the segment or return a capacity error before an ordinal overflows. A slot has meaning only alongside its segment. Cross-repository references use an import/export binding resolved against the workspace pins, including service deployment pins where supplied. External identity comes from a qualified contract key or a source anchor with a disambiguator. Preserve ambiguous resolutions explicitly. A rename need not imply identity continuity.

**Relations and supports.** A relation has a versioned schema, typed columns, permitted sources and a canonical tuple key. A support records the producer unit and version, source kind, anchor or observation, and any premises used for derivation. Alternate supports may establish the same tuple. A unit replacement retracts its old supports and installs its complete new batch atomically. The tuple stays visible while a valid support remains. A rule that needs observed traffic cannot consume a declared estimate as observed evidence.

**Coverage is stored data.** A producer records which relation, repository set, environment and key range it examined, its resolution assumptions, and whether that scope is complete or partial. Successful execution or zero matches does not itself establish completeness. A newly discovered repository, changed HTTP binding, or unresolved dynamic target can change coverage even when no known tuple changes.

## Physical layout

The local store contains immutable packs, immutable manifests, named head references and reader pins. A small persistent directory maps repository/file units and relation key partitions to packs. Partition updates copy directory paths and changed blocks. Forking creates a reference to an existing root.

| Pack family | Primary records | Read indexes |
|---|---|---|
| Source | File paths and blob hashes, symbol spans and kinds, source occurrences, resolution status and binding slots | Exact name dictionary and postings, file→symbols in source order, nested span lookup for the enclosing symbol |
| Connections | Occurrences bound to local symbols, operations or external slots | Forward and reverse adjacency. Each neighbour carries a site ID or support reference. Keep heuristic and resolved edges distinguishable. |
| Domain relations | Typed tuples declared by the grammar, with support IDs in separate columns | Canonical tuple order plus selected permutations. For example, index `reads(consumer, operation, field, site)` by `(operation, field)` to find consumers of a changed response field. |
| Verification | Obligation instances, applicability queries, read sets, coverage records, verifier artifacts and receipts | Obligation→reads for explanation and selection→dependent queries for invalidation. Include prospective obligation discovery. |

Store hot columns as independently addressable aligned buffers. Use little-endian fixed-width integers and relative byte offsets. A versioned header and section directory identify section kind, required features, codec, row count, offset, length and integrity digest. Use checked `u64` byte offsets. Validate bounds, lengths and required features before interpreting a section. Verify block integrity on first use and return a storage error on failure. Start with plain arrays and dictionary-encoded strings. Benchmark block sizes, for example 16, 64 and 256 KiB, independently of the OS page size. Compressed cold blocks must have independent restart points.

Proposed v1 reader contract, shown as field order rather than a Rust ABI:

```text
pack header       magic[8]  major:u16  minor:u16  required_features:u32
                  section_directory_offset:u64  section_count:u32  reserved:u32
section entry     kind:u16  codec:u16  flags:u32  rows:u64
                  offset:u64  stored_bytes:u64  decoded_bytes:u64  blake3[32]
call index        offsets:u64[n+1]  entries:(neighbour:u32, site:u32)[e]
```

The manifest records the expected header and directory digest. Check offsets for overflow, ordering and alignment, cap decoded lengths before allocation, and require the final adjacency offset to equal the entry count. Unknown optional sections may be skipped. When feeding `lemma-logic`, translate segment dictionary slots into that evaluation's interner. Its first-seen IDs must never enter a persistent logical fingerprint. [Current interner](../crates/lemma-logic/src/facts.rs)

For frequent binary relations, partition by source key and use an offset array into contiguous `(neighbour_slot, site_slot)` pairs. Store the transpose by destination. Sparse relations use a dictionary of populated keys rather than an offset for every workspace symbol. Cross-shard slots go through the binding dictionary. The two arrays for one million local nodes and five million occurrences would occupy about **92 MiB** with 64-bit offsets and two 32-bit fields per entry in each direction. This is layout arithmetic for the adjacency alone, excluding symbols, supports, dictionaries and overlays.

Register secondary orders from measured queries and bound columns in compiled rules. An unsupported binding pattern falls back to an exact scan with a broader read dependency. Existing substring search can use a packed name buffer. Optional source-text trigram postings can filter candidate blobs for regex or AST matching, with the actual matcher run against the pinned bytes. Build this index only if that workload pays for it. [Code Search's trigram design](https://swtch.com/~rsc/regexp/regexp4.html)

**I/O boundary.** The first reader uses checked slices and bounded positional reads. The mapped reader opens immutable published packs and shares the same decoder. The workspace forbids unsafe Rust and `memmap2`'s file-backed map constructor is unsafe. A mapped backend therefore needs a reviewed dependency with a sound safe interface and a guaranteed immutable file lifetime. Keep the buffered backend available for the comparison and on platforms without that guarantee. [Workspace lints](../Cargo.toml) · [Mapping API](https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html#method.map)

## Verification and invalidation

A tracked read returns **rows, coverage and a read stamp**. Its key names the relation, bound arguments or range, observed projection, scope and query semantics. Its stamp fingerprints the result values, coverage and relevant producer versions. An explanation that exposes source anchors tracks their support data too. Logical read keys survive physical repacking and can be compared across worlds. Record a read even when its result is empty.

Identify an obligation instance by its module, versioned requirement, bound entities and parameters. Mark whether the obligation preserves an organisational invariant or establishes a requested task postcondition. The agent can then tell which part of the requested change a receipt actually certifies.

1. Apply changed owners, bindings and coverage to a private child. Emit logical changes for additions, removals and changes to relevant supports. Consult both old and new keys when a value moves.
2. Look up affected **applicability queries as well as existing results**. Otherwise a patch can create the very first obligation of a kind without invalidating any existing receipt. Recompute applicability and retain the inventory, including incomplete applicability scopes.
3. Compare read stamps. Reuse an old result only if every tracked input and its coverage still match, the rule/checker/configuration versions match, and observations are valid at the requested evaluation time. Create a candidate receipt that identifies the original run and the revalidated inputs. Never relabel a previous snapshot's artifact as a new execution.
4. Rerun dirty verifiers. Emit the obligation ID, verdict, base/candidate identities, rule and checker versions, assumptions, source anchors, evidence and missing inputs. Distinguish `satisfied`, `violated`, `unknown` and `stale`. Skipped work is explicitly unexecuted. Blocking policy is separate from the verdict.

For the first slice, track equality/prefix selections, relation partitions, scope membership and coverage. A receipt for “no callers of X” depends on the callers selection even at cardinality zero. A partial search cannot satisfy that absence claim. A positive witness can establish a violation if it suffices under the rule. Observations carry a measurement window, source and validity deadline. Expiry invalidates reuse without requiring a source edit.

The minimum record for that absence check would look like this:

```text
read key    calls(target = operation X), project caller and site, scope {ts, go}
read stamp  digest(sorted matching caller/site pairs), digest(coverage and scope membership)
receipt     rule digest, validator digest, input snapshots, read key and stamp,
            verdict, evidence anchors, origin run, valid-until if applicable
```

An empty complete selection and an empty partial selection have different stamps. A receipt whose observation has expired is stale. If a fresh run still lacks sufficient evidence, its verdict is unknown.

Treat joins, negation, aggregation and recursion conservatively. When a precise read set cannot be established, depend on all input relations and coverage in the affected rule stratum, or rerun the whole program. For recursive deletes, reevaluate the affected component from surviving base supports. Counting references alone can leave a self-supporting cycle visible. Keep the existing full evaluator as the comparison oracle. An opaque external verifier must declare its input boundary or rerun for the candidate. This proposal does not assume that Soufflé already exposes incremental reads.

**Worked transition.** At root A, the deprecated Python operation has zero callers and complete coverage for the pinned TypeScript and Go clients. On child B, a TypeScript edit produces an HTTP call site whose workspace binding resolves to that operation. Publishing the new occurrence changes the operation's reverse posting and empty-selection stamp. The “unused” result becomes violated with the new site as witness. A requirement applying to *every caller* gains its first instance. If resolution instead becomes ambiguous and might target this operation, widen the affected coverage and return unknown for the absence claim. Readers pinned to A retain A's result. Removing one of two independent callers on B must preserve the remaining witness.

## Write protocol, recovery and retention

Use one local publisher per store initially. Agents can prepare batches independently. Every commit supplies its expected parent generation. A conflicting publish returns an explicit conflict with the current head. Late provider responses can be attached only to the world and input dependencies for which they were obtained.

Write new packs and their manifest to temporary files, verify and flush them, install them under immutable names, then flush the containing directories. Under the writer lock, compare the head, atomically replace the named reference and flush its directory. Acknowledged commits survive restart on a supported local filesystem. Recovery sees the old complete head or the new complete head. Unreachable staging files are reclaimable. A referenced corrupt or missing pack causes an explicit error. Establish crash semantics on Linux and macOS before enabling either as a durable backend.

Readers acquire a root pin under the same coordination protocol used by garbage collection before mappings are opened. GC traces named heads, retained receipts and active reader pins. Include pack references from external bindings, origin snapshots and evidence artifacts. Use process-held locks for active pins so a crashed process cannot retain data indefinitely. Retention of receipts must be explicit because a receipt can keep an old snapshot alive.

An overlay holds additions, support retractions and coverage changes for touched partitions. Reads merge newest replacements with live base supports. Publish a compacted physical generation when measured overlay read amplification warrants it. Preserve logical snapshot IDs, answers and source anchors through compaction. Retire old packs only after their last pin disappears. An on-disk major version change requires a shadow migration or rebuild and atomic cutover. Preserve irreplaceable declarations, observation bundles and evidence during migration.

## Integration and delivery order

At review time, the read/query boundary already exists in `FactStore`. Index writers and substring search also use concrete SQLite `Store` methods. The verification path extracts facts into the grammar and evaluates through `Engine`. Simply implementing the current read trait leaves both the write path and evidence reuse unfinished. [Storage boundary](../crates/lemma-store/src/lib.rs) · [Indexer](../crates/lemma-index/src/lib.rs) · [Symbol search](../crates/lemma-query/src/search.rs) · [Verifier](../crates/lemma-verify/src/lib.rs)

| Stage | Deliverable | Exit condition |
|---|---|---|
| 1. Logical model and oracle | Define ownership, bindings, tracked reads, coverage and four-state reports. Use the current manifests and compiled grammar as inputs. A future `.lm` compiler targets the same IR. | Full recomputation on the first-slice fixture yields reviewed expected results, including missing coverage and two supports. |
| 2. Native reader and writer | Implement native packs, roots, writer transactions, provider-answer storage and bulk readers. Route `index`, navigation and graph through the store boundary. | Close/reopen reproduces sorted results, source spans, provider status and snapshot scoping. Named branches and crash tests pass with no second database. |
| 3. Evidence loop | Connect `check` and `verify` to the pinned fact reader, obligation inventory and receipts. Start with conservative relation-level invalidation, then specialize the first caller selection. | Native incremental and fresh evaluation agree after every edit, including creation of an obligation, partial coverage and rollover after expiry. |
| 4. Optimize and evaluate | Add directional postings, overlay rollup and, if its lifetime contract is met, mapped reads. Add optional codecs/indexes one at a time. | Meet the correctness, performance and product gates below before making native storage the default. |

Suggested internal operations are `pin(head)`, `fork(parent, name)`, `replace_owner(unit, facts, coverage)`, `select(relation, bounds, scope, projection)`, `affected(delta)`, `record_receipt(run, reads)` and `publish(expected_parent)`. Return owning snapshot handles with cursors. Preserve legacy CLI numeric symbol IDs through persisted, non-recycled aliases when importing an existing index. Never expose dense physical slots as durable public IDs.

Make backend choice explicit during the pilot. Reindex pinned source or import version-compatible syntax rows and ID aliases into a sibling native store. Import provider answers only with matching input and provider fingerprints. Legacy empty answers do not imply complete coverage. Compare normal CLI output before switching a workspace reference. Keep the old store independently readable for rollback. Earlier storage and grammar decisions remain recorded in [spec 07](07-semantic-providers-and-indexing.md), [spec 08](08-relational-grammar-and-verification.md) and [spec 09](09-composable-manifests.md).

## Acceptance and decision gates

**Correctness is mandatory.** Compare sorted logical facts, witnesses, obligation inventory, verdicts and unknown reasons against a fresh build after each edit. Include addition/deletion of the first/last caller, two owners for the same fact, a moved symbol, independent agent forks, changed service pins, new scope members, policy edits, observation expiry and recursive deletions. Check interrupted publication at each durability boundary, competing writers, a live reader during GC and compaction, corrupted sections and unknown format versions. Empty, incomplete, stale and failed must stay distinguishable at the CLI boundary.

**Benchmark four configurations on the same facts and rules.** Use the current implementation, an optimized SQLite control with batched reads and the same materialized indexes and reuse rules, native buffered packs, and native mapped packs when available. The first comparison measures product improvement. The latter comparisons isolate the native layout and mapping. Use identical answers and coverage as a prerequisite for timing. On obligations the current implementation cannot express, use the full-rebuild oracle and optimized control, and exclude that case from current-versus-native speedup claims. A fast answer with missing consumers fails the case.

Use real TypeScript/Python/Go HTTP service tasks plus a reproducible synthetic corpus with tunable size, fan-out and edit locality. Measure cold first index, cold first query, warm two-hop impact, localized edit→verified evidence, and a shared-contract change that invalidates most consumers. Run one and ten concurrent branches. Record p50/p95, provider and verifier invocations, bytes written per edit, disk footprint including overlays, private and proportionally shared resident memory where available, page faults, compaction peaks, and build/compaction cost amortized across queries. Report LSP memory separately. For an agent run, also record independently correct completion, elapsed time and tokens.

**Proposed promotion criteria, not forecasts.** Require parity with fresh evaluation on all fixtures. Target warm edit→verdict p95 at or below half the time of the current implementation. To justify owning the physical format, also require either at least 25% lower p95 than the equivalent optimized control, or at least 30% lower store resident memory at comparable p95 (within 10%). Target at most 15% cold-index regression on the same corpus. Publish warm/cold methodology, repetitions and variance. If only the incremental model wins, keep that model and defer defaulting to the native format. The recorded navigation benchmarks and verification pilots are baselines, not measurements of this proposal. [Performance](../docs/performance.md) · [Agent benchmark](../docs/agent-benchmark.md) · [Pilot results](08-relational-grammar-and-verification.md)

## Review follow-up (2026-11-04): content-addressed units

**Recommendation — keep the logical model above; change the physical model and the order.** Partition facts by the unit that produced them, store each unit's batch once under a content address, and describe a snapshot as a manifest of unit addresses. Relation-ordered indexes become derived views. Build it on files and SQLite first; a native pack format is an optimisation of unit storage, to be justified by the gates above.

### What the code and measurements say

- **There are two fact pipelines.** `lemma index` writes symbols to SQLite (`files` and `symbols` rows per snapshot, reused within a directory's incremental runs). `lemma verify` and `lemma check` run a second extractor (`lemma_http::extract`), assemble its facts into the grammar, evaluate, and discard them. Nothing a check extracts is kept. [Store schema](../crates/lemma-store/src/schema.sql) · [Verifier extraction](../crates/lemma-verify/src/extract.rs)
- **Re-extraction dominates a check.** On the six-repository generic platform workspace, `lemma check` takes 15–20 s: about 8 s re-parsing every consumer repository with Tree-sitter, about 2 s in Soufflé, 1.5 s in an adapter, the rest reading Git. The SQLite store is not on that path at all. A within-run cache of facts by file content (shared by a change's base and head) already cut a change check from 21 to 17 s.
- **Snapshots duplicate unchanged files.** Each retained snapshot stores its own file and symbol rows, so retention multiplies storage by the number of snapshots kept rather than by the amount of change.
- **Every invalidation we have needed is unit-shaped.** A file edited, a document regenerated, an adapter's reading of a service at a tree, a resolver's schema output at a commit, a provider's answer for an input world. None has needed a relation key range.
- **Coverage already exists ad hoc.** Adapters report `x-lemma-complete`, unread routes, unread prefixes and routes-only readings, and the overlay refuses to remove operations when too few matched. That is this spec's coverage record without the name.
- **Worktrees per commit were the costliest storage mistake so far.** Adapters and command resolvers each created one per commit (26 GB and 28 GB in this session's caches). Keeping their outputs by Git tree removed the need. The same principle should govern every producer.

### Design

**Units.** A unit is one producer's output for one input: `(producer, producer version, options digest, input address)`. Producers are the Tree-sitter extractors (per file blob), the OpenAPI reader (per document blob), adapters (per service and code state), schema resolvers (per tree), and providers (per question and input world). The unit address is a BLAKE3 digest of that tuple in a canonical encoding. Its value is an immutable batch: typed rows per relation, the unit's coverage record (what it examined, complete or partial, assumptions, what it could not read), and the producer's diagnostics.

**Manifests.** A snapshot is a sorted list of `(owner, unit address)`, where the owner is the path or service the unit stands for. Its digest is the fact snapshot identity. A working directory's snapshot is its Git tree's manifest with the changed owners replaced, which is the key adapters already use. Forking copies a manifest or stacks a small delta on a parent; two agents' branches share every unit they did not change.

**Supports and ownership.** A tuple's supports are the units whose batches hold it. Replacing an owner's unit retracts exactly that unit's tuples; a tuple another unit also states stays. No separate support bookkeeping is needed for base facts. Derived facts keep the conservative rule this spec already states: re-evaluate the affected stratum.

**Storage.** Unit batches go in a content-addressed object directory (or one SQLite table of address to bytes), written to a temporary name and renamed, so writes are idempotent and need no lock. Manifests are objects too; named heads are small files replaced atomically, with compare-and-swap on the expected parent. Garbage collection marks from heads, kept receipts and active reader pins, then sweeps unreferenced objects: Git's object model, which this spec already cites for commit-graph.

**Views.** Navigation keeps SQLite, rebuilt from units rather than from parsing: symbol and name indexes per snapshot become views over the manifest's units, refreshed per changed owner. The grammar's input for a check is the union of the manifest's batches, decoded straight into `lemma-logic`'s interner, so a check re-parses only changed files. Relation-ordered or adjacency indexes (this spec's `reads(consumer, operation, field, site)` by `(operation, field)`) are materialised views keyed by manifest digest, built when a measured query needs them.

**Receipts.** A receipt records the requirement, the rule and grammar digests, the base and candidate manifest digests, the relations it read, and the coverage it relied on. It is reusable for a new manifest when no unit producing a relation it read changed (a manifest diff plus the producer-to-relation map) and the coverage is the same. This is the spec's relation-level invalidation, computed from units instead of read stamps; finer read sets come later, if measured.

### Against the proposal

| Choice | Proposal | Follow-up | Why |
|---|---|---|---|
| Primary partition | Relation key ranges in packs | Producer units | Invalidation, retention and dedup are all unit-shaped; no hot query is a range scan over all facts |
| Overlay and rollup | Additions and retractions per touched partition, compacted later | Manifest deltas over immutable units | Nothing to compact: an edit writes one new unit |
| Supports | Explicit support records | Implicit: the units holding a tuple | Same semantics for base facts, no bookkeeping |
| Physical format | Native packs first | Files or SQLite first; packs as a later optimisation of unit storage | The store is not on the measured hot path |
| Read tracking | Read stamps per selection | Relation-level from manifest diffs first | Soufflé takes 2 s; finer invalidation pays only once extraction is cached |
| Coverage | Stored per producer scope | Same, written by every producer into its unit | Adapters already produce it; give it one schema |

Kept unchanged: the three identities, capturing dirty content immutably, four-state verdicts, coverage as data, the first slice, the correctness gates, and the promotion criteria for owning a format.

### Order

1. **Unit cache for verify's extraction**, keyed by blob, extractor version and options, in the object directory. Exit: a repeated `lemma check` on unchanged code spends under a second extracting, with identical findings.
2. **Index facts as units.** The index's symbols become units of the same store; SQLite tables become views refreshed per changed owner. Exit: close and reopen give identical query results, and retaining ten snapshots costs about one snapshot plus their changes.
3. **One producer model.** Specs, adapters, resolvers and provider answers become units with coverage records; resolvers stop creating a worktree per commit. Exit: coverage records replace the adapter-specific completeness rules without changing any finding on the replay and mutation suites.
4. **Receipts and the first slice**, with relation-level invalidation and full recomputation as the oracle. Exit: the slice's verdicts match fresh evaluation after every edit, including a first caller, a removed caller and partial coverage.
5. **Physical format**, only if the gates above are met against this design as the optimised control.

### Open questions

- Unit encoding: a canonical binary encoding (for digests) per relation schema, versioned with the grammar's relation declarations.
- Provider answers are the one producer whose input world is not just blobs (a language server's view depends on the whole project). Key them by the manifest digest of what the provider reads, or declare their scope opaque and invalidate them on any change in their repository.
- Whether `.lm` modules should define relations' schemas, and therefore unit formats, or only rules on top of them.
