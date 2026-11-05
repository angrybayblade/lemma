# Segments and shared storage

**Status: proposal, 2026-11-05.** Nothing below is implemented. It refines the storage half of [spec 10](10-native-datastore.md): that document's logical model (three identities, units owned by producers, coverage as data, receipts) and its review follow-up (content-addressed units, snapshots as manifests of units) stand. This document decides the physical format those units take, how they group into shareable segments, and how segments move between a laptop, a devbox and CI. Measurements are from `main` at `db6848e` on the generated systems in `bench/lemmabench/scale.py`.

## Goals

Lemma should run the same way on a developer's machine, on devboxes and in CI, and be as fast as each place is configured to be. Work done once should not be done again: not on the next run, not on the next commit, not on another machine.

1. **Incremental everywhere.** A run pays for what changed since work it can reuse, wherever that work was done.
2. **Speed by configuration, not by a different tool.** The same check is correct with nothing cached; each tier below makes it faster.
3. **One format for everything derived from code.** Symbols, code facts, the functions tracing reads, and the indexes over them live in one format, written by one parse per file.
4. **Follows the development flow.** Local work produces snapshots; snapshots published from CI or a devbox are pulled by other CI runs and devboxes.

**Size references.** A project the size of SQLite (hundreds of files) should feel instant; one the size of Postgres (a few thousand files, decades of history) fast; one the size of Uber's monorepos (millions of files) workable, leaning on shared segments and change-driven evaluation. Lemma does not read C yet, so SQLite and Postgres are size references until it does, and then real benchmarks. Nothing in the format may assume a language.

### Speed tiers

| Tier | What is reused | Configured by |
|---|---|---|
| 0 | Nothing: parse what the run needs | Always available; the correctness oracle |
| 1 | Local unit records and segments | Default |
| 2 | Segments pulled from a shared store | A store URL and credentials |
| 3 | Prebuilt indexes inside segments (terms, names, adjacency) | Profile option; on by default for shared segments |
| 4 | Results (receipts) bound to segment digests | Later; see spec 10 |

Change-driven evaluation ([spec 11](11-change-driven-evaluation.md)) is orthogonal: it decides *which* units a run needs; the tiers decide where they come from.

## Where time and bytes go today

Warm `lemma check --change '#svc0'` at 100k files takes **2.7 s** (main thread):

| Phase | Time | Scales with |
|---|---|---|
| Decode tracing's functions (JSON, parallel) | ~0.6 s | every file of every traced repository |
| Build the trace's name index | ~0.4 s | every mention of every function |
| Inference: listing, selection, Soufflé | ~0.9 s | listing every file; selected facts |
| Rest of tracing, report | ~0.8 s | reached functions |

Cold takes **17 s**, almost all of it Tree-sitter parsing every file, **twice**: once by `lemma_http::extract` for code facts and once by `read_functions` for tracing. `index.db` parses a third time when `lemma index` runs.

The unit cache (`units.db`) holds per file a code-facts JSON value and a functions JSON value: **163 MB and 126 MB** at 100k files, plus 591k term rows. Every warm check decodes the functions of every file of every traced repository. The JSON repeats field names and the file path per function, and every mentioned name becomes its own allocated string: the same few thousand names (`api`, `json`, `return`) are allocated millions of times.

Two facts follow. Warm time is now dominated by decoding and indexing data that did not change. Cold time is parsing, which no local format can remove: only reusing someone else's parse can.

## Requirements

- **Deterministic.** The same inputs produce byte-identical records and segments, on macOS and Linux, in any order of discovery. Digests are taken over canonical encodings.
- **Immutable and content-addressed.** A record or segment is named by the digest of what produced it, never rewritten. Writes go to a temporary name and are renamed; readers never see partial objects.
- **Concurrent.** The daemon, checks and indexers read the same objects at once. Only the small catalog is mutable.
- **Versioned by what produced it, not by Lemma's build.** A release that changes only the Python extractor keeps every Go record valid, locally and in a shared store.
- **Verifiable before use.** Bounds, lengths and section digests are checked before a section is interpreted (spec 10's reader contract).
- **Safe Rust.** The workspace forbids `unsafe`. Readers start with buffered positional reads; memory mapping waits for a reviewed dependency with a sound interface (spec 10, I/O boundary).
- **Private by default.** Records hold names, paths and call structure: source-derived and sensitive. Nothing leaves the machine unless a shared store is configured, and some kinds of data never leave it (below).
- **Same answers.** Findings, traces and query results are identical with and without any tier. The existing gates (verify, mutation, replay, fixtures) are the check.

## Model

Five kinds of object, all but the last immutable:

| Object | Is | Keyed by |
|---|---|---|
| **Unit record** | One file's parse: symbols, code facts, functions and their mentions, index terms | Profile digest + file blob id |
| **Segment** | The unit records of one subtree, plus indexes over them | Profile digest + Git tree id |
| **Manifest** | A repository snapshot: the segments covering its tree, plus an overlay | Digest of its sorted contents |
| **Overlay** | Unit records for files that differ from the tree (uncommitted edits) | Digest of its records; local only |
| **Catalog** | Heads, reader pins, provider answers, retention, what the shared store holds | Mutable; one per machine |

### Unit records

A unit record is one producer's output for one file (spec 10's unit), written by **one parse**. It replaces today's two JSON values per file.

```text
record      magic[4]  major:u16  minor:u16  profile:blake3[32]  blob:bytes
            section_count:u32  sections[...]
section     kind:u16  rows:u32  offset:u32  length:u32  blake3[32]
strings     count:u32  offsets:u32[count+1]  bytes   -- sorted, deduplicated
symbols     (name:sid, qualified:sid, kind:u8, span:(u32,u32,u32,u32))[rows]
functions   (name:sid, qualified:sid, kind:u8, lines:(u32,u32),
             mentions:(offset:u32, len:u32), binds:(offset:u32, len:u32))[rows]
mentions    sid[...]                     -- each function's run sorted
facts       per relation: typed columns of sids and integers
terms       sid[...]                     -- the record's index terms, sorted
```

- **Strings are interned per record** (`sid` is an index into the record's sorted string table). The file path is stored once, not once per function. A function's mentions are a sorted run of `sid`s: decoding allocates nothing per mention.
- **Canonical:** sections in kind order, rows in source order, string table sorted, integers little-endian fixed width. The record's digest is over these bytes.
- **Facts are typed columns** per relation of the code-facts schema (`path_references`, `client_calls`, `member_calls`, `import_bindings`, …), not a serialized struct, so readers decode only the relations they need. Selection reads `terms` alone.

### Segments

A segment is the unit records of one subtree, merged, with indexes built once:

```text
segment     header (as spec 10's pack header)
            strings      -- one sorted table for the subtree; records' sids remapped
            files        (path:sid, blob:bytes, record digest, language:u8)[rows]
            symbols, functions, mentions, facts   -- concatenated in file order
            terms        postings: term sid -> file rows
            names        definitions: name sid -> count
                         mentioners:  name sid -> function rows
            children     (path:sid, tree id, segment digest)[...]   -- see boundaries
```

The `names` section is the trace's name index, prebuilt: a warm trace reads it instead of decoding every function and building the index (today ~1 s at 100k files). `terms` is spec 11's call index without SQLite.

**Keyed by Git tree.** Git already names every directory by the hash of its contents. A segment's key is `(profile digest, tree id)`. A commit that changes one package changes the tree ids on the path from that package to the root and nothing else: every other segment is reused as is, locally and across machines.

**Boundaries.** One segment per directory would be too many small objects; one per repository would rewrite everything on every commit. A directory roots its own segment when its subtree holds at least `segment_min_files` files (proposal: 2,000) or is a package root (`go.mod`, `package.json`, `pyproject.toml`, `Cargo.toml`); smaller directories fold into their parent's segment. The policy is part of the profile digest, so it is deterministic. A parent segment lists its child segments in `children` rather than containing them, so a commit inside a child rewrites the child and the small chain of parents above it.

**Reading a snapshot** opens the manifest's segments (tens to low hundreds even at monorepo scale) and merges their postings. Queries that bind a term or name probe each segment's sorted section; repository-wide scans iterate segments in path order.

### Overlays

Uncommitted edits are files whose content differs from the tree's blob. Each gets a unit record; the overlay is the set of them, and it masks those paths in the segments below. Overlays stay in the catalog's machine and are never pushed: they hold unpublished code.

### Catalog

The catalog keeps what changes: named heads and reader pins, provider answers (a language server's answer depends on installed dependencies and server version, so it is not portable), retention bookkeeping, and which segments the shared store holds. It is small and transactional, which SQLite does well; it stays SQLite and is off the hot path.

`index.db`'s code-derived tables (`files`, `symbols`, `exports`, the workspace tables derived from manifests) move into segments in stage C2; what remains of `index.db` is the catalog. One storage format for everything derived from code; one small transactional store for bookkeeping.

## Identity and versioning

A **profile** names everything that determines a record's bytes: format major version, the extractor version of each language, extraction options (path markers, client receivers), and the segment boundary policy. Its digest replaces today's whole-binary build id in cache keys.

Extractor versions are declared constants, bumped when an extractor's output changes. Forgetting to bump is the risk, so a golden test extracts a fixture corpus per language and fails when the output changes but the version did not. A shared store keeps segments for several profiles side by side; a client asks only for its own.

## Determinism and portability

A segment built on a laptop must equal the one built in CI:

- **Read committed content from Git blobs**, never from the working tree, when building a segment: this removes line-ending and filter conversion (Git's `eol`, `filter`, `ident` attributes; listing already distrusts such blobs). Working-tree content goes only into overlays.
- **Paths** are repository-relative with `/`, compared bytewise.
- **Grammars** are pinned by the Tree-sitter crate versions in the profile.
- **No ambient input:** no clock, no environment, no map iteration order in anything written.
- A CI job builds the same fixture segments on macOS and Linux and compares digests.

## Sharing segments

A hosted Lemma (Entail, spec README) stores segments by address for an organization. The flow follows development:

1. **Local:** runs build records and segments for what they read; nothing leaves the machine.
2. **Publish:** CI on the main branch, or a devbox, pushes the segments of the commits it checked, with a signed provenance record.
3. **Pull:** CI on a pull request, or a devbox, asks for the manifest of the base commit and pulls the segments it lacks for the subtrees it needs; it parses only the change.

Protocol sketch, content-addressed and cache-friendly:

```text
GET  /segments/{profile}/{tree}           -> segment bytes, or 404
PUT  /segments/{profile}/{tree}           <- segment bytes + provenance
GET  /manifests/{repository}/{commit}     -> root tree id and profile list
```

The client never needs the manifest from the server to be correct: it can compute tree ids from Git itself. The manifest endpoint only saves listing a commit it has not fetched.

**Trust.** A client cannot check a segment without re-parsing it, so it trusts provenance: who built it (a CI identity of the organization), with which profile, from which commit, signed. Clients accept segments signed by builders their configuration trusts. A client can re-parse a small random sample of each pulled segment's files and compare records, which detects a poisoned or buggy builder cheaply and is on by default.

**Privacy.** Segments are source-derived. The store is per organization, encrypted at rest, with access tied to repository access. Overlays and provider answers are never pushed.

**Third-party code.** Vendored and installed dependencies are the same across organizations. Public segments per package version could make them free; this is a hosted-product feature for later and needs no format change.

## Retention

Locally: keep segments reachable from named heads, pinned readers, and the last *n* snapshots per repository (today's `--keep` and `--ttl` apply to segments too); evict least recently used beyond a disk budget. In a shared store: keep segments reachable from recent commits on active branches. Content addressing makes eviction safe: a missing segment is rebuilt or pulled, never wrong.

## Rollout

Each stage stands alone, is gated, and keeps tier 0 as the oracle.

| Stage | Deliverable | Gate |
|---|---|---|
| **A** | Unit records with interned strings in `units.db`, replacing the functions JSON first, then code facts. Profile digest replaces the build id in keys. | All gates unchanged; warm 100k ≤ 2.2 s; `units.db` ≤ half its size |
| **B** | One parse per file: one record holds symbols, code facts and functions. Verify, tracing and later `lemma index` read it. | Cold 100k ≤ 12 s (one parse instead of two); all gates unchanged |
| **C** | Segments by Git tree with `terms` and `names` sections, buffered reads, overlays, catalog. Tracing and selection read sections instead of decoding records. | Warm 100k ≤ 1.2 s with nothing decoded per unchanged file; editing one file writes one record and one segment chain; all gates unchanged |
| **C2** | `index.db`'s code-derived tables served from segments; `index.db` becomes the catalog. | `symbols`, `refs`, `graph` answer identically; ten retained snapshots cost about one plus their changes |
| **D** | Shared store: protocol, signed provenance, sampling verification, pull and push in the CLI and CI. | Cold 100k check with the base's segments pulled ≤ 3 s plus transfer; a tampered segment is rejected by sampling in the test suite |

Spec 10's promotion criteria still govern making any of this the default, with today's SQLite unit cache as the optimized control.

## Benchmarks

- **Synthetic corpora** from `bench/lemmabench/scale.py` at 1k, 100k and 1M files (the last as the Uber-size reference), cold and warm, plus a one-file edit, a one-package edit and a shared-contract change.
- **Real:** the generic platform workspace, replayed history (`results-review`), mutation suites.
- **When C lands in Lemma:** SQLite and Postgres repositories as the small and medium references, with their history as edit streams.
- Report wall time, peak memory, bytes read and written, objects written per edit, and, for D, bytes transferred.

## Vocabulary

To add to CONTRIBUTING's table when the code lands: **unit record** (one file's parse in Lemma's format), **segment** (the unit records of one subtree with indexes, keyed by tree), **overlay** (records for uncommitted files), **catalog** (the mutable local store), **profile** (what determines a record's bytes).

## Open questions

- **Segment boundary threshold.** 2,000 files is a guess; measure object count against rewrite size on the generic platform's history and on a 1M-file corpus.
- **Grammar facts.** Should segments also hold the Datalog input facts per file (post-resolution), so Soufflé's input for unchanged files is copied, not rebuilt? Resolution reads other files (imports, clients), so these facts depend on more than the file; they belong to a manifest-level unit, not a file record.
- **Cross-repository dedup.** Forks and vendored copies share trees across repositories; keying by tree id already dedups them within one store. Is that safe across organizations' stores (it reveals that two hold the same tree)? Default: no cross-organization dedup.
- **Mapped reads.** Which reviewed crate, if any, offers a sound safe interface over immutable published files, and does it beat buffered reads on the 1M corpus?
- **Receipts in the shared store.** Results bound to segment digests (tier 4) could make an unchanged pull request's CI a lookup. Design with spec 10's receipt model once C exists.
