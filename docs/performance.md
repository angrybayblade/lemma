# Performance

Lemma is meant to be called by agents in a tight loop: edit, re-index, ask where something is used. Every step has to be close to free. This page shows where Lemma stands and how it got there.

## Results

Measured on an Apple M4 Max (14 cores), macOS 15, with two private monorepos:

- **functions**: 73,620 indexed files (71,506 Python, 2,113 TypeScript), 426 MB of source, 538,062 symbols.
- **platform**: 5,507 files (3,667 TypeScript, 1,257 Python, 583 Go), 36 MB, 42,205 symbols.

Times are wall-clock medians from [hyperfine](https://github.com/sharkdp/hyperfine). On this machine starting any process, even `/usr/bin/true`, takes about 15 ms, so that is the floor for any command.

### Indexing

| Scenario | functions before | functions after | platform before | platform after |
| --- | --- | --- | --- | --- |
| First index | 33.1 s | 5.4 s | 2.86 s | 0.53 s |
| Re-index, nothing changed | 31.9 s | **0.15 s** | 2.69 s | **63 ms** |
| Re-index after editing 1 file | 35.8 s | **0.21 s** | 2.78 s | **114 ms** |
| Re-index after editing 10 files | 34.3 s | 0.23 s | 2.91 s | 125 ms |

### Reference lookups

"Before" started a language server for every lookup that was not cached. "After" keeps servers warm in a background daemon.

| Lookup | Before | After: first lookup | After: next lookups |
| --- | --- | --- | --- |
| functions, Python | 1.35 s, **wrong** (1 of 4 references) | 1.6 s (ty) | 70-100 ms |
| functions, TypeScript | 0.72 s, **wrong** for project-wide symbols (0 of 183) | 2.8 s | 50-320 ms |
| platform, TypeScript | 7.3 s | 6.4 s | 50-200 ms |
| platform, Python | 0.97 s | 0.15 s (ty) | 45-50 ms |
| platform, Go | 1.9 s | 0.6 s | 0.3-1.0 s |
| Any cached answer | 75-80 ms | 42-48 ms | 42-48 ms |

The first lookup is bounded by how long the language server takes to load the project. After that, the cost is the server's own query time plus about 30 ms for Lemma.

### Symbol search

| Query | Before | After |
| --- | --- | --- |
| functions, exact name | 97 ms | 16 ms |
| functions, substring | 120 ms | 91 ms |
| platform, exact name | 21 ms | 17 ms |
| platform, substring | 28 ms | 23 ms |

## What made the difference

Each change was measured on its own against the previous best and kept only if it helped without hurting correctness.

| Change | Effect |
| --- | --- |
| Parse files in parallel | First index on functions 33 s → 7.8 s. |
| Re-index incrementally: skip files whose size, timestamps and inode are unchanged; parse only files whose contents changed; rewrite only changed rows | Re-index with nothing changed 5.5 s → 1.7 s. Unchanged files keep their symbol ids. |
| One parser per thread, created only when that thread has to parse | Re-index 1.7 s → 0.71 s. Building a parser compiles every Tree-sitter query (about 10 ms), and the first version did that hundreds of times per run. |
| Walk directories in parallel | 0.71 s → 0.48 s. |
| Run Git, the directory walk and loading the stored index at the same time | 0.48 s → 0.34 s. |
| Ask Git about every source file only on the first index or after the commit moves; otherwise only about files that changed | Re-index 0.34 s → 0.15 s, edit 0.42 s → 0.28 s. Results matched `git status` through edits, reverts, new and deleted files. |
| Rewrite one cache-invalidation query to check each cached answer instead of each symbol | Edit 0.28 s → 0.20 s. |
| Keep language servers warm in a daemon, and tell them which files changed between snapshots | Next lookups: TypeScript 2.8 s → 50 ms, Python 1.8 s → 80 ms. Edits were seen by warm ty, TypeScript and gopls servers. |
| Use ty as the first Python server | Functions lookups 6-10 s (pyright) → about 2 s cold and under 20 ms warm, with the same call sites. |
| Let exact symbol search use the name indexes | 97 ms → 16 ms. |

## Correctness bugs found by benchmarking

Two servers were answering before they had loaded the project, and the incomplete answers were cached as if they were real:

- **pyright** publishes diagnostics for the opened file before it has found the project's files. On functions it returned 1 of 4 references.
- **typescript-language-server** publishes diagnostics and goes quiet before it starts loading the project. On functions it returned no references for a function used in 183 places.

Providers can now declare the message they send once the project is loaded (`ready_messages`), and Lemma waits for it. See [Language servers](providers.md).

## Tried and not kept

| Approach | Why not |
| --- | --- |
| SQLite tuning for the first index (`synchronous`, larger cache, rebuilding indexes after the bulk insert) | Writing 538k symbol rows stayed at 1.6-1.7 s; the total did not move beyond noise. |
| mimalloc as the memory allocator | No measurable change. Tree-sitter allocates through C `malloc`, and Rust-side allocation was not the bottleneck. |
| A trigram full-text index for substring search | Queries went from 84 ms to 7 ms, but building it added 4.6 s and 45 MB to functions's first index, and very common substrings got slower. |
| Restricting `git status` to supported extensions to make it cheaper | Same cost (282 ms vs 274 ms). Kept anyway, because it defines what "uncommitted changes" means. |
| Killing an unneeded `git status` | Left a stale `.git/index.lock`, because status rewrites the index. Lemma now runs Git with `--no-optional-locks`, so it never writes the user's index. |
| pyrefly as the Python server | Returned references from the open file only, even after 30 seconds. |
| basedpyright as the Python server | Same per-lookup cost as pyright, and erratic when asked early. |
| pyright with a warm server | Still about 5.5 s per lookup on functions: it re-reads the project for every request. |

## Open opportunities

- **First index**: writing to SQLite (about 1.7 s on functions) could run while files are still being parsed. Replacing Tree-sitter queries with a hand-written tree walk could save some of the roughly 24% of extraction time spent in queries.
- **Substring search** scans every qualified name. It costs about 75 ms on half a million symbols.
- **The daemon** answers one lookup at a time.

## Reproducing

The numbers come from hyperfine runs of the release binary (`cargo build --release`) against local clones, so edits never touched the original repositories:

1. Indexing: delete the index for a cold run; for edit runs, append a comment to tracked Python files before each run.
2. Reference lookups: one cold `--refresh` lookup per language, then a different uncached symbol, the same symbol uncached, and a cached repeat.

The same commands work against any large repository.
