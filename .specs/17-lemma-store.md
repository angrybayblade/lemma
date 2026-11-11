# The Lemma store

**Status: in progress, 2026-11-10. Stages 1–3 are on the `lemma-store` branch; the layout below replaces stage 1's tree of nodes.** This spec replaces the two pipelines that read code today with one store that every command reads. It takes over from the physical designs in [spec 10](10-native-datastore.md) and [spec 12](12-segments-and-shared-storage.md), and keeps their logical rules: coverage as data, four outcomes, determinism, safe Rust and full evaluation as the oracle. [Spec 14](14-sentry.md)'s prepared inputs shrink to keeping this store current.

## Why

- **Code is read twice.** `lemma index` writes symbols to `index.db`. `lemma check` and `lemma verify` read the same files again into unit records and segments, and neither reads the other's output.
- **Units of invalidation disagree.** Code facts are per file. Schemas are per repository: one edit changes the adapter's key, and the whole app is imported again.
- **Evaluation is all or nothing.** Every ask checks every declared link and evaluates the rules, even when one file changed.
- **The change-to-symbol step does not exist.** Nothing records which symbols an edit changed, so nothing can say which operations it can reach.

## Model

```text
source → file → symbol → attribute
           ↑
service | module: a mapping over files, not a level of containment
```

| Level | Is | Identity |
|---|---|---|
| **Source** | A repository or package at one revision: a commit, or a working directory | Repository name and revision |
| **Service, module** | What a description names. A file can belong to several services or to none | Declared name; files come from its `include` |
| **File** | One parse | Blob digest |
| **Symbol** | A function, type, schema or route | Path, qualified name, discriminator |
| **Attribute** | One thing known about a symbol: a signature, a request schema, a resolved reference | Symbol, kind, input key |

**Symbol digests** cover the symbol's tokens, comments excluded and spans excluded, so adding a line above a function changes nothing about it. String tokens stay, so a docstring or a `.describe()` that reaches a schema counts as a change. Anonymous handlers take their identity from what registers them, such as `POST /x` in `app.post('/x', ...)`, with a discriminator when a file repeats it.

Facts about one symbol and connections between symbols are separate. "Calls `foo`" belongs to the caller and depends on its file. "This `foo` is that definition" depends on other files, so it is an attribute with those files in its input key, and an edit elsewhere does not touch the caller's record.

## Eager and lazy

**Eager, on every change:** files and symbols, with name-level references and index terms. Tree-sitter produces all of it from one file, in one parse.

**Lazy, only when required:** attributes. An attribute is looked up by its input key. On a miss its provider runs and the answer is stored.

| Attribute kind | Provider | Input key covers |
|---|---|---|
| Written type (Go, Rust, TypeScript) | Tree-sitter | The files it follows |
| Pydantic or Zod schema | Interpreter runner | The files actually loaded; lockfile and interpreter |
| Operation shape from a whole service | Whole-service adapter | The service's code state, as today |
| Route path across mounts | Tree-sitter, else whole-service adapter | The files of the mount chain |
| Resolved reference | Language server | Project files, lockfile, server name and version |

An attribute that cannot be produced has the value `unknown` with a reason. It is reported, and it never reads as "no change".

### What makes an attribute required

1. Compare symbol digests between the two sources.
2. Follow name-level references backwards from changed symbols to the operations they can reach, across whatever services the description links.
3. Those operations' attributes are required, at base and head. Others keep their stored value or are never computed.

Name-level references over-approximate on purpose. If code cannot be followed to a symbol, as with a schema built at run time, the operation is required. A wrong "not required" is a missed finding, so the gates compare this rule with full evaluation on every run.

- `resolve` demands one symbol's attributes.
- `check --change` demands attributes for what the change can reach, and evaluates only the links and rules those touch.
- `verify` demands everything, and adds the description checks.

## Providers and runners

All providers answer one request, so a command does not know which one replied.

```text
request   { file, symbols: [{ name, kinds }] }
answer    per symbol: { attributes } or { unknown: reason }
          loaded_files: every file read to produce the answers
```

`loaded_files` comes from the interpreter's own module table, so the input key records what actually happened. Imports inside functions that never ran and environment variables do not appear; the err-towards-required rule covers them.

**Interpreters are discarded.** A runner is a new process, per source and per batch: it loads the required files in sorted order, collects every requested attribute, exits. There is no reloading and nothing resident. Language servers stay warm, because they are built to take incremental changes.

- **When sentry is attached**, it starts a runner as soon as a batch changes a required attribute, after a short debounce. An edit that reaches none starts nothing.
- **A new batch** kills a running runner only if it changes one of the files that runner depends on.
- **Without sentry**, the ask starts the same runner and waits. Sentry can make an answer faster, never different.
- **An answer is stored only if** the digests of its `loaded_files` still match after loading.
- **A file that kills the process** is marked `unknown`, and the rest are retried in a new one.
- **Concurrent runners are capped**, so a bulk state in several repositories does not start every import at once.

Today's adapters remain as the fallback when file-level extraction cannot answer.

## Format: `index.ls`

Each repository, or module with its own index, has one `.lemma/index.ls`. The tree in the model is how Lemma names things; it is not how they are stored. Storage follows the reads and writes Lemma makes:

| Access | Served by |
|---|---|
| A batch of edited files | A delta segment holding only those files |
| A fresh index, or a merge | A full segment built in one sorted pass |
| One file, by path | The files table of the newest segment holding the path |
| Symbols by name | Each segment's string table, then its name lists |
| What mentions a name | Each segment's reverse mention lists |
| One symbol, by id | Each segment's sorted id table |
| A value under a key | Sorted attribute runs, each with a filter |

```text
file      magic "LMLS"  major:u16  minor:u16  commits...
commit    nodes and values, then a footer
node      kind:u8  length:u32  payload               -- roots, segment, run, run list
value     bytes                                      -- found through its run entry
footer    "LSFT"  roots:u64  runs:u64  previous:u64  start:u64  synced:u64  checked:u64
          commit:blake3[32]  footer:blake3[32]
```

**Roots** are the sources a store keeps: `head` and `work` for a repository, and one per indexed directory for navigation. Each lists its segments, oldest first, with its source record and a digest of its files. History beyond the roots a store keeps is out of scope here.

**Segments** are immutable and position-independent: every offset inside one is relative to its start, so a segment can be copied into another file unchanged.

```text
segment     counts and a section table, then:
  paths       sorted; a file's ordinal is its position
  files       blob[32]  language  flags  stamp  symbols:(start, count)
  strings     sorted and deduplicated: names, qualified names, mentions
  symbols     name  qualified  kind  span  name_span  discriminator  digest[32]  id[16]  file  mentions
  mentions    symbol → string ids
  ids         sorted (id, symbol)
  by name, by qualified name, mentioned by     -- string id → symbols
```

A delta segment has the same layout and holds only the files a commit changed; a removed file is a row flagged as removed. **A newer segment replaces whole files in older ones.** Every row of a segment belongs to one file, so a delta never deletes a single symbol, and a read takes the newest segment that holds a path.

**Merging.** A root stacks at most eight delta segments on its full segment, which holds every file. Past that, its deltas merge into one, and when that one holds more than a quarter of the full segment's files, everything merges into a new full segment. `work` stacks on `head`'s segments, so the two share a full segment. ("Full" rather than "base", which already names the old side of a change.)

**Symbol ids** are the first 16 bytes of a BLAKE3 digest of the symbol's path, qualified name, kind and discriminator. They are the same in any store and any build and are never handed out from a counter, so nothing is stored to keep a symbol's id across edits. In a workspace, a symbol's address adds its member's name.

**Root digests** add up a BLAKE3 digest of each file's path and blob in four 64-bit lanes, wrapping. Order does not matter, so a commit updates a root's digest from only the files it changed, and the same files give the same digest whichever segments hold them.

**Comparing roots.** When one root's segments extend the other's, as `work` extends `head`, only paths in the extra segments can differ, so the cost follows the change. Otherwise the two sorted file tables are walked side by side. Changed files then compare symbol digests.

**Attributes** are values filed under 32-byte keys. A write appends its values and then a run: its keys, sorted, each with its value's offset and length, then a Bloom filter. The footer names the list of runs, newest first, and a lookup skips a run whose filter rules the key out. A write merges the newest runs into its own while each is at most twice what it has gathered, and more of the newest while the list would pass eight, so runs grow in size tiers, as in size-tiered LSM stores: a small write rewrites only small runs, and a large run is rewritten rarely. Values are not copied.

**A cache, not a record.** Everything in `index.ls` can be derived again from source, so commits are not flushed one by one: a commit is written whole and the operating system writes it back when it likes. A commit larger than 1 MiB, an explicit checkpoint, and the commit that would leave more than 4 MiB or 64 commits unflushed flush the file and then append a footer whose `synced` says so. On opening, every commit after the last `synced` offset is checked against its footer's commit digest, on every core when they add up to 1 MiB or more, and the first that fails ends the store there. A flush costs about a millisecond, and checking a small commit about a microsecond, so the bounds keep both small for stores that take many small commits. A process that wrote or checked the store does not check again what it already has: while the file still ends with the footer it last saw, its next writer and its reads start from there. Bytes before `synced` are trusted. Decoding checks every length and offset, so damage there is an error and never a crash, but damage that keeps a valid shape is not detected. A crash loses at most the commits since the last flush, which the next run computes again.

**Compaction** writes the live segments and the newest value under each key into a new file, flushes it and renames it over the old one. It runs when the file has doubled since the last check and less than half of it is live. A reader that mapped the old file keeps reading it until it reopens.

**One writer** at a time, under a lock on `index.ls.lock`. Readers take no lock: bytes before a footer never change, and compaction replaces the file instead of changing it.

**Reads** are memory-mapped, with positional reads as the fallback for filesystems where maps are unreliable. The workspace forbids `unsafe`; the `lemma-mapped` crate is the one exception, holding a single call that hands everyone else a `&[u8]`. Decoding stays safe and bounds-checked, and both paths read the same bytes. The conventions of `records.rs` carry over: little-endian fixed-width integers, sorted string tables, every length checked before use.

**Portability.** Language-server and interpreter attributes depend on the machine's installed dependencies. They are flagged local and dropped by a pack for sharing. Uncommitted work is a root that never leaves the machine.

### Precedents

| System | What this layout borrows |
|---|---|
| Git's index and cache-tree | A flat sorted file table, with tree digests computed instead of stored |
| Lucene, Tantivy, Zoekt | Immutable segments merged in the background, sorted term dictionaries, memory-mapped reads |
| RocksDB, LevelDB | Small sorted runs over a large sorted file, a filter per run, a small record of what is current |
| Glean, IntelliJ's file indexes | Every fact owned by one file, so a delta replaces whole files |
| clangd | Per-file symbols and relations merged into one index for reads, with hashed symbol ids |
| Salsa, ccache, Bazel | Values keyed by what they were computed from, so stale ones stop matching instead of being deleted |
| GitHub's stack graphs | Name-level facts per file, resolved across files when read |

## Workspaces subsume members

A workspace's `index.ls` **mounts** its members. It holds no copy of their segments.

```text
mount     (member name, path to its index.ls, root name, root digest)[]
```

Joining reads each member's last footer and compares the root digest. **The cost follows the number of members, not the number of symbols**, and it is only possible because nothing in a member's store refers to anything outside it:

- String ids and symbol ordinals are private to a segment, and are never compared across segments.
- References across members go through names, and are resolved in the workspace's own layer.
- Symbol ids depend only on a symbol's identity in its repository, so a mounted symbol has the same id wherever it is read.

**What a workspace stores for itself** is only what no member can know: the description, links, cross-member resolved references and term postings, each kept per member and keyed by that member's root digest. A change in one member rebuilds that member's contribution and leaves the others.

**Lookups** across members probe each member's segments, newest first. A merged index is built only if a measured query needs one.

**Packing** copies a workspace and its members into one self-contained file for CI or a shared store. Mounting is for local work; packing is for moving.

**A member whose profile differs**, or whose file is missing or corrupt, is rebuilt. It is not refused and never read as empty. An external service pinned to a commit is mounted from its cached checkout's index at that commit.

## Gates and tuning

The private gates must not lose a finding at any stage. New findings are reviewed rather than failing automatically, since language-server answers can legitimately find more.

Fixed before tuning: record addresses, symbol digests, input keys, `unknown`, the provider contract, one path with or without sentry.

Open to hill climbing, against replays of real edit sequences: the precision of the "required" rule, debounce and superseding, grouping per batch or per file, which provider answers which kind, concurrency limits, and how much to fill in the background. Score ask time at median and p95, runner starts per change, runs nobody used, cold time and peak memory. Some commits are held back and never tuned on.

## Build order

Each stage passes the gates before the next starts, and the existing evaluation stays as the oracle throughout.

| Stage | Deliverable | Exit |
|---|---|---|
| **1** | `index.ls` reader, writer and crash recovery, with Tree-sitter producing the eager layer in one parse | Reopening gives identical symbols; an interrupted write recovers at every boundary; byte-identical files on macOS and Linux |
| **2** | `refs`, `symbols` and `graph`, then `check`'s code facts and tracing, read the store. `index.db`, unit records and segments go | Same answers, one parse per file |
| **3** | Mounts: a workspace joins its members | Joining costs the same at 100 and 100k files per member |
| **4** | Attribute contract, with today's adapters and resolvers wrapped as whole-service providers | No finding changes |
| **5** | The "required" rule, limiting link checks and evaluation | Same findings as full evaluation on every gate |
| **6** | File-level interpreter runners, Python first, then TypeScript | Same findings; fewer imports per change |
| **7** | Sentry reduced to keeping the eager layer current and starting runners | Sentry's comparison with fresh runs shows no differences |

### One parse per file

Decided 2026-11-10 for what remains of stage 2, by where a 100k-file check (20 repositories of 5k) spends its time. Warm, 1.95 s: listing repositories 0.13 s, choosing files 0.19 s, reading kept facts (JSON) under 0.1 s, writing facts into the grammar 0.38 s, Soufflé 0.5 s, tracing 0.24 s. Cold, 18.7 s: parsing every file for code facts about 10 s, encoding them as JSON 0.2 s, Soufflé and inference 1.7 s, and tracing parsing every file again, 5.9 s. A fresh `lemma index` of the same files parses them a third time, 4.7 s. The format of facts matters little; the number of parses matters a lot.

- **Tracing reads the store, first.** Its function records come from the same syntax extraction as `lemma index`, and hold nearly what segments hold: names, spans and mention lists. Segments gain what tracing needs besides, such as a file's top-level code and the names it binds. `check` commits each repository it reads, at a commit or in the working directory, as a root of that repository's `index.ls`, and tracing walks the stored mention lists (`Index::reaching`). Function records and functions segments go. This removes tracing's second parse, about 5 s of a cold 100k check.
- **Code facts do not depend on the description.** Extraction keeps every string that may be a path and every call through a receiver; path markers and client receivers are applied when facts are read. One parse then serves `lemma index` and `check`, so a check after an index parses nothing.
- **Facts are records of typed columns** in the store's record codec (`lemma-store::records`): a sorted string table, length-prefixed columns per relation, and a closing digest, filed as attributes beside the segment. Selection reads the terms column alone.
- **Records live in each repository's `index.ls`, keyed by a profile digest**: the format version, each language's declared extractor version and the segment policy, with a golden test per language that fails when extraction output changes without a version bump. Upgrading Lemma keeps a repository's records unless its profile changed.
- **Listings stay with Git.** `lemma index` and `check` choose files the same way, and Git remains the source of each file's blob id.

Landed so far, on the same 100k-file system with byte-identical reports: reading code facts parsed each file twice, once for its definitions and again for the walk, and now parses it once (cold 18.6 to 17.1 s); and the parse that reads a file's facts also keeps tracing's record of its functions, under the key tracing looks it up by, so tracing reads and parses nothing (cold 17.1 to 12.8 s, tracing 5.7 to 1.4 s). Those records still live in the per-build facts cache. They move into each repository's `index.ls` with facts that do not depend on the description and one file selection, when `lemma index` can share the parse too. Tracing taking a function's mentions from its syntax tree, as segments hold them, rather than from every word on its lines left replay, recall and the mutation gates unchanged, so segments need add only a file's top-level code and its destructurings.

## What landed

Stages 1–3 are on the `lemma-store` branch. `index.ls` holds segments as described above; navigation, check's unit records, index terms and segments, and workspace mounts all read it, and SQLite is gone. `Index::reaching` follows the stored mention lists; nothing calls it until stage 5. Commit roots built from Git, and the scan adopting `lemma-index`'s file selection, remain.

Stage 1's tree of nodes was measured and replaced. A one-file commit appended 9.7 KiB to a 102,000-file system, and on a real repository a fresh `lemma index` took eight times as long as SQLite and a symbol search 27 times as long, because search decoded every file and indexing wrote attributes for every symbol.

Segments against SQLite on `main`, as medians of three interleaved rounds on an 8-CPU Linux VM. Scale systems come from `bench/lemmabench/scale.py`, and findings were identical in every case. Each cell is full evaluation, then change-driven.

| | SQLite | `index.ls` | Gain |
|---|---|---|---|
| `check`, 10k files, cold | 3.19 s, 2.38 s | 2.69 s, 1.95 s | 1.19×, 1.22× |
| `check`, 10k files, warm | 1.63 s, 0.57 s | 1.46 s, 0.56 s | 1.12×, 1.01× |
| `check`, 100k files, cold | 35.1 s, 25.9 s | 27.6 s, 18.6 s | 1.27×, 1.39× |
| `check`, 100k files, warm | 16.1 s, 4.21 s | 14.6 s, 4.00 s | 1.10×, 1.05× |
| Check cache, 100k files | 496 MB, 501 MB | 418 MB, 441 MB | 1.19×, 1.14× smaller |

On a private repository whose SQLite index is 14 MB, a fresh `lemma index` took the same time as SQLite (7.7 s), a re-index with nothing changed 99 ms against 109 ms, `lemma symbols` 5 ms against 7 ms, and the index is 10.5 MB, a quarter smaller.

Peak memory is 7–19% lower on cold runs, but higher on warm change-driven runs at 100k files: 327 MB against 219 MB. Anonymous memory is the same, 208 MB against 199 MB; the difference is pages of the mapped cache file counted as resident, 197 MB against 43 MB. They are clean pages the kernel can drop, but peak resident memory counts them, so whether to read values positionally is left to tuning.

**Tuning.** `crates/lemma-store/examples/engine_bench.rs` times every engine operation on a system of 102,000 files and 400,000 symbols. Segments are built in parallel in a buffer of their own and written as a part of the commit, rows are written in place, a process does not check again what it wrote or already checked, at most 64 commits wait unflushed, attribute runs merge by size tier, lookups search runs where they lie, and search ranks candidates from their rows' string ids. As medians on a quiet 8-CPU VM, against the first segments: a full segment 958 to 229 ms, a fresh index 1.38 s to 216 ms, and a substring search 53 to 10 ms. Against the first timing of small writes: fifty one-file updates 106 to 6 ms, fifty puts of ten units 327 to 18 ms, and opening after small commits 0.69 to 0.03 ms.

Commands against `main`, as medians of three interleaved rounds, on one 5,100-file repository and on 100,000 files indexed as one root:

| | 5k files | 100k files |
|---|---|---|
| `lemma index`, fresh | 421 → 367 ms | 5.22 → 4.69 s |
| `lemma index`, nothing changed | 45 → 22 ms | 402 → 324 ms |
| `lemma symbols`, exact | 14 → 5 ms | 85 → 9 ms |
| `lemma symbols`, containing | 19 → 6 ms | 99 → 36 ms |
| `lemma graph`, references cached | 103 → 4 ms | 1.80 s → 5 ms |

`graph` gained most because it asks the store about only the files and names it meets, where it used to read every symbol first. Commands that wait on a language server, such as `refs` asked for the first time or `definition`, spend their time starting it and did not change. A fresh index spends most of its time parsing.

**A warm check**, change-driven on the same 100k-file system, as medians of eleven interleaved runs on a quiet VM: 1.73 to 1.29 s, with the same report. Soufflé runs on every core and its input files are written row by row into one buffer (Soufflé's step 369 to 281 ms); facts are written into the grammar's relations without copying a relation's name per row, and each function's consumer id is made and interned once per file (reading code 697 to 555 ms); tracing reads its kept function segments in parallel (241 to 144 ms); a check is dated for its allowances only when it has some (79 to 7 ms after tracing); and the executable's digest is remembered under its file identity rather than hashed on every run. Soufflé compiled to a binary ran no faster than its interpreter on these inputs, after a 33 s compile.

## Open questions

- **File-level extraction on real apps.** Circular imports, imports of an entry point and schemas held only on route objects may send many files back to the whole-service path. This needs measuring per service before stage 6.
- **One process per batch or per file.** Import order can change results through global registries, and batches depend on timing. Start with one per batch, and let the sentry comparison decide.
- **Evaluating rules for part of a workspace.** The store says what changed; limiting Datalog evaluation to it needs its own design at stage 5, building on [spec 11](11-change-driven-evaluation.md).
- **Language servers at base.** A base-side answer needs a checkout of the base. Decide whether to pay for it or to answer `unknown`.
- **Whether the file should be committed.** Sharing through Git would be simple, but it would add a binary that changes on every commit. The default is no.

### Parked for experiment

Alternatives to the decisions in [One parse per file](#one-parse-per-file), to be measured rather than argued:

- **A stat cache for listings** in `index.ls`, like Git's index and untracked cache, Mercurial's dirstate or Jujutsu's working-copy state: about 0.1 s per run at 100k files outside the daemon, against reimplementing Git's rules for racily clean files, sparse checkouts, submodules, links and conversions, where a mistake gives a wrong blob id and stale facts.
- **Facts keyed by the description's options**, if keeping every candidate path and receiver makes records too large or reading them too slow.
- **A serde binary format** such as postcard for facts, if reading only some columns turns out not to matter.
- **Keeping facts as JSON**, which costs about 0.2 s cold and under 0.1 s warm at 100k files.
- **Records in the per-build user cache** rather than each repository's `index.ls`.

What else a warm check could shed, estimated on the same 100k-file system, about 0.2–0.3 s:

- **Relations kept as ids between verification and tracing.** Verification renders some 250,000 strings of the relations tracing walks, which tracing parses back: about 70 ms.
- **Facts appended and sorted once per relation** rather than each inserted into a sorted set, flushed before anything reads them mid-extraction: about 30–50 ms.
- **The listing stat cache** above: about 100 ms of the 136 ms spent listing 21 repositories through Git.
- **Code facts as typed-column records** rather than JSON: about 40–60 ms of decoding.
