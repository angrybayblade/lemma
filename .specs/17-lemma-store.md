# The Lemma store

**Status: proposal, 2026-11-10. Nothing here is implemented or measured.** This spec replaces the two pipelines that read code today with one store that every command reads. It takes over from the physical designs in [spec 10](10-native-datastore.md) and [spec 12](12-segments-and-shared-storage.md), and keeps their logical rules: coverage as data, four outcomes, determinism, safe Rust and full evaluation as the oracle. [Spec 14](14-sentry.md)'s prepared inputs shrink to keeping this store current.

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

**Eager, on every change:** the tree down to symbols, with name-level references and index terms. Tree-sitter produces all of it from one file, in one parse.

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

Each repository, or module with its own index, has one `.lemma/index.ls`. It is a single append-only file of immutable nodes; a commit appends new nodes and then a footer.

```text
file      magic[4]  major:u16  minor:u16  nodes...  footer
node      kind:u8  length:u32  payload  blake3[32]
footer    magic[4]  roots:u64  attributes:u64  previous_footer:u64  blake3[32]

roots       (name, node offset)[]          -- head, work, commit:<id>
source      repository, revision, profile digest, tree offset, description offset
directory   (name, kind, digest, offset)[] -- sorted by name
file        blob digest, language, string table, symbols
symbol      name:sid, qualified:sid, kind:u8, spans, discriminator, digest, mentions:sid[]
```

Index terms join the file node at stage 2, when `check` starts reading the store. The layout may change freely until that stage is on `main`.

**Merkle tree.** A directory's digest covers its children's digests, not their offsets. The same content has the same digest in any file, at any offset. An edit appends one file node and the directories above it, and a new source and footer, so the size of a write follows the depth of the path, not the size of the repository. That removes spec 12's cost of rewriting a whole repository's segment after one edit.

**Base and head are roots of one tree.** They share every node they have in common, and comparing them descends only where digests differ. The change-to-symbol step is that walk.

**Attributes sit beside the tree, not inside it.** Filling in a lazy attribute must not change any digest above it. The footer points at a second structure: file path to a block of `(symbol digest, kind, input key) → value`, updated by appending a new block and the small path above it. Logically an attribute belongs under its symbol. Physically it is a side table keyed by that symbol's digest, so a symbol that moves keeps its attributes.

**Durability.** A reader takes the last footer whose digest checks. A crash leaves at worst a tail that is ignored. There is one writer at a time, under a file lock. A run that cannot get the lock keeps its results in memory and tries again at the end. Readers need no lock, because bytes before a footer never change.

**Reads** are memory-mapped, with positional reads as the fallback for filesystems where maps are unreliable. The workspace forbids `unsafe`; the `lemma-mapped` crate is the one exception, an experiment holding a single call that hands everyone else a `&[u8]`. Decoding stays safe and bounds-checked, and both paths read the same bytes. Each node's digest is checked when it is first read. The conventions of `records.rs` carry over: little-endian fixed-width integers, sorted string tables, every length checked before use.

**Compaction** copies what the kept roots reach into a new file and renames it over the old one. It runs when the unreachable share of the file passes a threshold.

**Portability.** Language-server and interpreter attributes depend on the machine's installed dependencies. They are flagged local and dropped by a pack for sharing. Uncommitted work is a root that never leaves the machine.

## Workspaces subsume members

A workspace's `index.ls` **mounts** its members. It holds no copy of their nodes.

```text
mount     (member name, path to its index.ls, root name, root digest)[]
```

Joining reads each member's last footer and compares the root digest. **The cost follows the number of members, not the number of symbols**, and it is only possible because nothing in a member's tree refers to anything outside it:

- Slots and string ids are private to a file node, and are never compared across nodes.
- References across members go through names, and are resolved in the workspace's own layer.
- Digests are independent of position, so a mounted node is the same node wherever it is read.

**What a workspace stores for itself** is only what no member can know: the description, links, cross-member resolved references and term postings, each kept per member and keyed by that member's root digest. A change in one member rebuilds that member's contribution and leaves the others.

**Lookups** across members probe each member's sorted sections, as spec 12 reads several segments. A merged index is built only if a measured query needs one; spec 12 found rebuilding is often cheaper than storing.

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

## What landed

Stage 1's format, reader, writer and recovery exist in `lemma-store`; the SQLite crate that held that name is `lemma-legacy-store` until stage 2 removes it, and `lemma-syntax`'s `outline` gives each symbol its digest and mentions from one parse. Nothing else reads the store yet. Compaction, attributes and adopting `lemma-index`'s file selection remain.

Measured with `crates/lemma-store/examples/store_bench.rs` on an 8-CPU Linux VM, on a system from `bench/lemmabench/scale.py` read as one tree: 102,000 files, 400,000 symbols, medians of seven runs with the file already in the page cache.

| | Mapped | Positional |
|---|---|---|
| Open | 0.02 ms | 0.06 ms |
| Compare two sources after a one-file edit | 0.10–0.14 ms | 0.19 ms |
| 2,000 file lookups | 31–40 ms | 50 ms |
| Decode every file | 470–514 ms | 675–746 ms |

A full build took 11 s to parse and under 1 s to write, for a 71 MiB file. A one-file commit appended 9.7 KiB and took 4–51 ms across two runs, mostly the two flushes. Mapping is a real but modest gain; opening and comparing are already negligible either way. Whether to keep it should wait for stage 2, when the store is on a real read path.

## Open questions

- **File-level extraction on real apps.** Circular imports, imports of an entry point and schemas held only on route objects may send many files back to the whole-service path. This needs measuring per service before stage 6.
- **One process per batch or per file.** Import order can change results through global registries, and batches depend on timing. Start with one per batch, and let the sentry comparison decide.
- **Evaluating rules for part of a workspace.** The store says what changed; limiting Datalog evaluation to it needs its own design at stage 5, building on [spec 11](11-change-driven-evaluation.md).
- **Language servers at base.** A base-side answer needs a checkout of the base. Decide whether to pay for it or to answer `unknown`.
- **Directory fan-out.** Very large directories make a node expensive to rewrite; decide whether to split them.
- **Whether the file should be committed.** Sharing through Git would be simple, but it would add a binary that changes on every commit. The default is no.
