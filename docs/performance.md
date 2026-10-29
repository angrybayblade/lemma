# Performance

Lemma is meant to be called by agents in a tight loop: edit, re-index, ask where something is used. Every step has to be close to free, and the answers have to be right. This page compares Lemma with the text and syntax search tools agents use today, shows where its time goes, and lists what was tried to get there.

## Setup

Measured on an Apple M4 Max (14 cores), macOS 15, with two private monorepos:

- **functions**: 73,620 indexed files (71,506 Python, 2,113 TypeScript), 426 MB of source, 538,062 symbols.
- **platform**: 5,507 files (3,667 TypeScript, 1,257 Python, 583 Go), 36 MB, 42,205 symbols.

Times are wall-clock medians from [hyperfine](https://github.com/sharkdp/hyperfine), with warm file system caches. On this machine starting any process, even `/usr/bin/true`, takes about 15 ms, so that is the floor for any command, Lemma's included.

## Lemma vs grep, ripgrep and ast-grep

Each tool was asked the same question, "where is this symbol used?", for ten symbols chosen from rare to very common names:

| Tool | Command |
| --- | --- |
| grep | `grep -rnw --include=<ext> --exclude-dir=node_modules --exclude-dir=.git NAME .` (BSD grep 2.6) |
| git grep | `git grep -nw NAME -- <globs>` |
| ripgrep | `rg -nw -t <type> NAME .` (15.2) |
| ast-grep | every identifier-like node whose text is `NAME` (0.51) |
| Lemma | `lemma refs <symbol>`: warm language server, and cached |

The ast-grep query is the best a structural search can do without types: identifiers, member names and type names, so comments and strings drop out. A bare `NAME` pattern misses member access such as `queue.New`.

### Accuracy

Ground truth comes from asking a language server "go to definition" at every place the name appears, and counting it when the answer is the target symbol. Python is checked with pyright, which is independent of ty, the server Lemma uses. TypeScript uses typescript-language-server and Go uses gopls. Two symbols have thousands of matches; for those, 400 random matches were checked, and every match Lemma reported was checked too.

Precision is the share of reported lines that really refer to the symbol; recall is the share of real references reported. Counts are per line, without the definition itself.

| Symbol | Real references | grep | git grep | ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- | --- | --- | --- |
| functions Python `get_md5` | 4 | 19% / 100% | 80% / 100% | 80% / 100% | 100% / 100% | **100% / 100%** |
| functions Python `get_secret` (2 definitions) | 26 | 72% / 100% | 76% / 100% | 76% / 100% | 90% / 100% | **100% / 96%** |
| functions Python `get_keyring` | 7 | 54% / 100% | 54% / 100% | 54% / 100% | 100% / 100% | **100% / 100%** |
| functions Python `McpExecuteResult.to_dict` (7 definitions) | 3 | 0% / 100% | 21% / 100% | 21% / 100% | 25% / 100% | **100% / 100%** |
| functions Python `Action.execute` (500+ definitions) | 0 in 400 sampled matches | 0% (67,602 lines) | 0% (58,643) | 0% (58,635) | 0% (56,744) | **100% (9 lines)** |
| functions TypeScript `createUrlFieldValidator` | 183 | 97% / 100% | 97% / 100% | 97% / 100% | 100% / 100% | **100% / 100%** |
| platform TypeScript `getOrgJwtConfig` | 1 | 100% / 100% | 100% / 100% | 100% / 100% | 100% / 100% | **100% / 100%** |
| platform TypeScript `logger` (7 definitions) | 252 in 400 sampled | 65% / 100% | 65% / 100% | 65% / 100% | 73% / 100% | **100% / 100%** |
| platform Go `queue.New` (2 definitions) | 15 | 2% / 100% | 2% / 100% | 2% / 100% | 2% / 100% | **100% / 100%** |
| platform Go `isvcauth.New` (used from another module) | 6 | 1% / 100% | 1% / 100% | 1% / 100% | 1% / 100% | **100% / 50%** |

Text and syntax search always find every use of a name, and everything else with that name. On rare names that is fine; on names agents actually need to trace, such as `execute`, `New` or `to_dict`, almost everything they report is noise. Lemma reports only real references. It misses references a language server cannot see: here, uses of a Go function from a second Go module without a `go.work`, and one alternate definition in a `try/except ImportError` fallback.

### Latency

| Symbol | grep | git grep | ripgrep | ast-grep | Lemma, warm | Lemma, cached |
| --- | --- | --- | --- | --- | --- | --- |
| functions Python `get_md5` | 10.9 s | 4.19 s | 2.29 s | 4.04 s | **32 ms** | **17 ms** |
| functions Python `get_secret` | 7.03 s | 4.20 s | 2.28 s | 4.11 s | **35 ms** | **18 ms** |
| functions Python `get_keyring` | 10.3 s | 4.16 s | 2.30 s | 3.99 s | **32 ms** | **18 ms** |
| functions Python `to_dict` | 8.56 s | 4.20 s | 2.32 s | 4.00 s | **33 ms** | **19 ms** |
| functions Python `execute` | 18.1 s | 4.24 s | 2.02 s | 4.10 s | 342 ms | **18 ms** |
| functions TypeScript `createUrlFieldValidator` | 914 ms | 139 ms | 80 ms | 102 ms | **32 ms** | **18 ms** |
| platform TypeScript `getOrgJwtConfig` | 293 ms | 68 ms | 57 ms | 270 ms | **20 ms** | **17 ms** |
| platform TypeScript `logger` (2,033 references) | 341 ms | 67 ms | 62 ms | 265 ms | 207 ms | **28 ms** |
| platform Go `queue.New` | 98 ms | 42 ms | 34 ms | 85 ms | **17 ms** | **17 ms** |

"Warm" means the language server was running and the answer was not cached; "cached" means the same question was asked since the code last changed. Text search has to read every file each time, which on this machine costs about 2 seconds for functions's 426 MB. Lemma asks a language server that already holds the project, so the cost is the server's query plus about 3 ms of Lemma, on top of process start-up.

### Time to the first answer

When nothing is running yet (the daemon is stopped, the index exists), `lemma index` followed immediately by `lemma refs`:

| Lookup | Index | First lookup | Total |
| --- | --- | --- | --- |
| functions Python | 0.28 s | 2.1 s | 2.4 s |
| functions TypeScript | 0.40 s | 3.0 s | 3.4 s |
| platform TypeScript | 0.17 s | 6.9 s | 7.1 s |
| platform Go | 0.16 s | 1.2 s | 1.3 s |

The first lookup waits for the language server to load the project. `lemma index` starts the servers in the background, so an agent that does anything else for a few seconds in between finds them ready: with 10 seconds between the two commands, the first lookup took 72 ms on platform TypeScript and 136 ms on functions TypeScript.

## Lemma's own commands

### Indexing

| Scenario | functions, first version | functions now | platform, first version | platform now |
| --- | --- | --- | --- | --- |
| First index | 33.1 s | 5.4-6.0 s | 2.86 s | 0.53-0.58 s |
| Re-index, nothing changed | 31.9 s | **32 ms** | 2.69 s | **25 ms** |
| Re-index after editing 1 file | 35.8 s | **85 ms** | 2.78 s | **75 ms** |
| Re-index after editing 10 files | 34.3 s | **91 ms** | 2.91 s | **78 ms** |

An edit costs one `git status` limited to the changed files (about 50 ms here) plus parsing them; nothing scales with the size of the repository.

### Symbol search

| Query | First version | Now |
| --- | --- | --- |
| functions, exact name | 97 ms | 16 ms |
| functions, substring (`get_user`) | 120 ms | 19 ms |
| functions, substring with 49,000 matches (`execute`) | 151 ms | 27 ms |
| platform, exact name | 21 ms | 17 ms |

## What made the difference

Each change was measured against the previous best and kept only if it helped without hurting correctness.

| Change | Effect |
| --- | --- |
| Parse files on every core | First index on functions 33 s → 7.8 s. |
| Re-index incrementally: skip files whose size, timestamps and inode are unchanged; parse only changed contents; rewrite only changed rows | Re-index with nothing changed 5.5 s → 1.7 s. |
| One parser per thread, created only when needed | 1.7 s → 0.71 s. Building a parser compiles every Tree-sitter query (about 10 ms), and the first version did that hundreds of times per run. |
| Walk directories in parallel; run Git, the walk and loading the stored index at the same time | 0.71 s → 0.34 s. |
| Ask Git about every file only on the first index or a new commit, otherwise only about changed files | 0.34 s → 0.15 s. |
| Run every command in a per-user daemon; the `lemma` command only forwards it | Cached lookups 43 ms → 16 ms, the process floor. |
| Keep language servers running in the daemon, telling them which files changed between snapshots | Warm lookups: TypeScript 2.8 s → 32 ms, Python 1.8 s → 32 ms. |
| Use ty as the first Python server | functions lookups 6-10 s (pyright) → 2 s cold, under 20 ms warm. |
| Keep documents open in warm servers instead of reopening them per lookup | Server time per lookup 43 ms → 29 ms. |
| Watch indexed directories and update the in-memory index per changed file, with an order-independent fingerprint | Re-index on functions: nothing changed 147 ms → 32 ms, one edit 213 ms → 85 ms. |
| Start `git status` for the changed paths before parsing, in parallel | Part of the above. |
| Start language servers in the background after indexing, opening a file from the densest directory | First lookup after a short pause: seconds → 72-136 ms. |
| Use the name indexes for exact search; search substrings in memory with a vectorised scan | Exact 97 ms → 16 ms, substring 120 ms → 19 ms. |

## Correctness bugs found by benchmarking

- **pyright** published diagnostics for the opened file before it had found the project's files, and answered from the files it knew. On functions it returned 1 of 4 references, and Lemma cached that.
- **typescript-language-server** published diagnostics and went quiet before it started loading the project. On functions it returned no references for a function used in 183 places.
- **ty** does not count import statements as references unless declarations are included, which cut Python recall to 57-75%. Lemma now always asks for declarations and removes only the definition itself.

Providers can declare the message they send once a project is loaded (`ready_messages`), and Lemma waits for it. See [Language servers](providers.md).

## Tried and not kept

| Approach | Why not |
| --- | --- |
| SQLite tuning for the first index (`synchronous`, larger cache, rebuilding indexes after the bulk insert) | Writing 538k symbol rows stayed at 1.6-1.7 s. |
| mimalloc as the memory allocator | No measurable change; Tree-sitter allocates through C `malloc`. |
| A trigram full-text index in SQLite for substring search | 7 ms per query, but 4.6 s and 45 MB more on functions's first index, and slower for common substrings. The in-memory search is as fast without either cost. |
| Restricting `git status` to supported extensions to make it cheaper | Same cost. Kept anyway, because it defines what "uncommitted changes" means. |
| Killing an unneeded `git status` | Left a stale `.git/index.lock`. Lemma now runs Git with `--no-optional-locks`, so it never writes the user's index. |
| pyrefly as the Python server | Returned references from the open file only. |
| basedpyright, or pyright kept warm | About 5.5 s per lookup on functions even when warm: it re-reads the project for every request. |
| Opening the first file by path to warm a server | On platform it opened a configuration file at the root, so the server loaded the wrong project first: the first lookup took 15.8 s instead of 6.9 s. |

## Open opportunities

- **Edits** still pay for one `git status` process (about 50 ms here). Comparing file contents with the commit in-process would remove it.
- **Lookups with thousands of results** spend most of their time naming the enclosing function of each reference, one query each.
- **The first index** of functions takes 5-6 s. Writing to SQLite while parsing, and a hand-written tree walk instead of Tree-sitter queries, could shorten it.
- **The daemon** runs one command at a time.
