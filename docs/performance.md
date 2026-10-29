# Performance

Lemma is meant to be called by agents in a tight loop: edit, re-index, ask where something is used. Every step has to be close to free, and the answers have to be right. This page shows where Lemma's time goes and what was tried to get there; [the comparison](comparison.md) sets it against the text and syntax search tools agents use today.

## Setup

Measured on an Apple M4 Max (14 cores), macOS 15, with two private monorepos:

- **mercury**: 73,620 indexed files (71,506 Python, 2,113 TypeScript), 426 MB of source, 538,062 symbols.
- **platform**: 5,507 files (3,667 TypeScript, 1,257 Python, 583 Go), 36 MB, 42,205 symbols.

Times are wall-clock medians from [hyperfine](https://github.com/sharkdp/hyperfine), with warm file system caches. On this machine starting any process, even `/usr/bin/true`, takes about 15 ms, so that is the floor for any command, Lemma's included.

## Lemma vs grep, ripgrep and ast-grep

The full comparison, with method, results, the biases that could skew them and the trade-offs, is in [Lemma vs grep, ripgrep, git grep and ast-grep](comparison.md). In short, on ten symbols across both repositories:

- Lemma's precision was 100% on every symbol. Text search fell to 0-2% on common names shared by many symbols, and was 80-100% on rare ones. Lemma missed references the language server could not see.
- On mercury, a symbol's first lookup on a running server took about 100-250 ms (up to 11.5 s for a name defined 500 times), and 17-80 ms when asked again. ripgrep took 2.0-2.3 s, but file opens are unusually slow on this machine, which inflates that gap.
- On platform, ripgrep (34-62 ms) was faster than Lemma's first lookup of a symbol (about 100-470 ms).
- Lemma needs an index and a running language server first: 1.3-7.1 s to the first answer when nothing is running.

### Time to the first answer

When nothing is running yet (the daemon is stopped, the index exists), `lemma index` followed immediately by `lemma refs`:

| Lookup | Index | First lookup | Total |
| --- | --- | --- | --- |
| mercury Python | 0.28 s | 2.1 s | 2.4 s |
| mercury TypeScript | 0.40 s | 3.0 s | 3.4 s |
| platform TypeScript | 0.17 s | 6.9 s | 7.1 s |
| platform Go | 0.16 s | 1.2 s | 1.3 s |

The first lookup waits for the language server to load the project. `lemma index` starts the servers in the background, so an agent that does anything else for a few seconds in between finds them ready: with 10 seconds between the two commands, the first lookup took 72 ms on platform TypeScript and 136 ms on mercury TypeScript.

## Lemma's own commands

### Indexing

| Scenario | mercury, first version | mercury now | platform, first version | platform now |
| --- | --- | --- | --- | --- |
| First index | 33.1 s | 5.4-6.0 s | 2.86 s | 0.53-0.58 s |
| Re-index, nothing changed | 31.9 s | **32 ms** | 2.69 s | **25 ms** |
| Re-index after editing 1 file | 35.8 s | **85 ms** | 2.78 s | **75 ms** |
| Re-index after editing 10 files | 34.3 s | **91 ms** | 2.91 s | **78 ms** |

An edit costs one `git status` limited to the changed files (about 50 ms here) plus parsing them; nothing scales with the size of the repository.

### Symbol search

| Query | First version | Now |
| --- | --- | --- |
| mercury, exact name | 97 ms | 16 ms |
| mercury, substring (`get_user`) | 120 ms | 19 ms |
| mercury, substring with 49,000 matches (`execute`) | 151 ms | 27 ms |
| platform, exact name | 21 ms | 17 ms |

## What made the difference

Each change was measured against the previous best and kept only if it helped without hurting correctness.

| Change | Effect |
| --- | --- |
| Parse files on every core | First index on mercury 33 s → 7.8 s. |
| Re-index incrementally: skip files whose size, timestamps and inode are unchanged; parse only changed contents; rewrite only changed rows | Re-index with nothing changed 5.5 s → 1.7 s. |
| One parser per thread, created only when needed | 1.7 s → 0.71 s. Building a parser compiles every Tree-sitter query (about 10 ms), and the first version did that hundreds of times per run. |
| Walk directories in parallel; run Git, the walk and loading the stored index at the same time | 0.71 s → 0.34 s. |
| Ask Git about every file only on the first index or a new commit, otherwise only about changed files | 0.34 s → 0.15 s. |
| Run every command in a per-user daemon; the `lemma` command only forwards it | Cached lookups 43 ms → 16 ms, the process floor. |
| Keep language servers running in the daemon, telling them which files changed between snapshots | Lookups after the first: TypeScript 2.8 s → about 100-250 ms for a new symbol and 32-64 ms when repeated; Python similar. |
| Use ty as the first Python server | mercury lookups 6-10 s (pyright) → 2 s cold, under 20 ms warm. |
| Keep documents open in warm servers instead of reopening them per lookup | Server time per lookup 43 ms → 29 ms. |
| Watch indexed directories and update the in-memory index per changed file, with an order-independent fingerprint | Re-index on mercury: nothing changed 147 ms → 32 ms, one edit 213 ms → 85 ms. |
| Start `git status` for the changed paths before parsing, in parallel | Part of the above. |
| Start language servers in the background after indexing, opening a file from the densest directory | First lookup after a short pause: seconds → 72-136 ms. |
| Use the name indexes for exact search; search substrings in memory with a vectorised scan | Exact 97 ms → 16 ms, substring 120 ms → 19 ms. |

## Correctness bugs found by benchmarking

- **pyright** published diagnostics for the opened file before it had found the project's files, and answered from the files it knew. On mercury it returned 1 of 4 references, and Lemma cached that.
- **typescript-language-server** published diagnostics and went quiet before it started loading the project. On mercury it returned no references for a function used in 183 places.
- **ty** does not count import statements as references unless declarations are included, which cut Python recall to 57-75%. Lemma now always asks for declarations and removes only the definition itself.

Providers can declare the message they send once a project is loaded (`ready_messages`), and Lemma waits for it. See [Language servers](providers.md).

## Tried and not kept

| Approach | Why not |
| --- | --- |
| SQLite tuning for the first index (`synchronous`, larger cache, rebuilding indexes after the bulk insert) | Writing 538k symbol rows stayed at 1.6-1.7 s. |
| mimalloc as the memory allocator | No measurable change; Tree-sitter allocates through C `malloc`. |
| A trigram full-text index in SQLite for substring search | 7 ms per query, but 4.6 s and 45 MB more on mercury's first index, and slower for common substrings. The in-memory search is as fast without either cost. |
| Restricting `git status` to supported extensions to make it cheaper | Same cost. Kept anyway, because it defines what "uncommitted changes" means. |
| Killing an unneeded `git status` | Left a stale `.git/index.lock`. Lemma now runs Git with `--no-optional-locks`, so it never writes the user's index. |
| pyrefly as the Python server | Returned references from the open file only. |
| basedpyright, or pyright kept warm | About 5.5 s per lookup on mercury even when warm: it re-reads the project for every request. |
| Opening the first file by path to warm a server | On platform it opened a configuration file at the root, so the server loaded the wrong project first: the first lookup took 15.8 s instead of 6.9 s. |

## Open opportunities

- **Edits** still pay for one `git status` process (about 50 ms here). Comparing file contents with the commit in-process would remove it.
- **Lookups with thousands of results** spend most of their time naming the enclosing function of each reference, one query each.
- **The first lookup of each symbol** costs the language server's own search (about 100-250 ms here, seconds for very common names); Lemma cannot make that faster, only cache it.
- **The first index** of mercury takes 5-6 s. Writing to SQLite while parsing, and a hand-written tree walk instead of Tree-sitter queries, could shorten it.
- **The daemon** runs one command at a time.
