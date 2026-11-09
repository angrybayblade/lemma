# Performance

Lemma is built for agents to call in a tight loop: edit, re-index, ask where something is used. Every step has to be close to free, and the answers have to be right. This page shows where Lemma's time goes and what was tried to get there; [the comparison](comparison.md) sets it against the text and syntax search tools agents use today.

## Setup

Measured on an Apple M4 Max (14 cores), macOS 15, with two private monorepos:

- **monorepo A**: 73,620 indexed files (71,506 Python, 2,113 TypeScript), 426 MB of source, 538,062 symbols.
- **monorepo B**: 5,507 files (3,667 TypeScript, 1,257 Python, 583 Go), 36 MB, 42,205 symbols.

Times are wall-clock medians from [hyperfine](https://github.com/sharkdp/hyperfine), with warm file system caches. On this machine starting any process, even `/usr/bin/true`, takes about 15 ms, so that is the floor for any command, Lemma's included.

## Method

The work ran in four rounds, each checking the one before:

1. **Speed.** Each command was profiled, one change was tried at a time against the previous best, and a change was kept only if it helped without hurting correctness. The tables on this page come from this round.
2. **Head to head.** Lemma against grep, ripgrep, git grep and ast-grep, on 10 hand-picked symbols, on the same Mac.
3. **Bias check.** Two problems turned up. Opening files on that Mac barely scales across threads, so text search looked about 25 times slower than it is (ripgrep on monorepo A: 2.3 s there, 86 ms on Linux). And "warm" Lemma figures repeated the same question, which language servers cache; the first ask for a symbol is slower.
4. **Fixed protocol.** On a clean Linux machine, with the protocol written and fingerprinted before measuring: 241 randomly drawn symbols bucketed by how common their names are, every reported line checked by two independent language servers, cold and warm caches. That is [the comparison](comparison.md), and it replaces round 2's figures.

## Lemma vs grep, ripgrep and ast-grep

[Lemma vs grep, ripgrep, git grep and ast-grep](comparison.md) has the full comparison, measured on a clean Linux machine to a protocol fixed beforehand, with 241 randomly drawn symbols checked by independent language servers. In short:

- Lemma's precision was 96% per symbol, against 31% for ripgrep, grep and git grep and 35% for ast-grep; recall was 99% per symbol against 100%. Lemma misses calls through interfaces, files outside a TypeScript project, and references needing deeper type inference than ty does.
- With Lemma running, a symbol's first lookup took 19-126 ms (median per repository), a repeat 5-37 ms and a cached answer 1-3 ms. Those are single repositories: from a workspace's root, each query first checks every member for changes and reuses or rebuilds its exports, which took about 0.1 s across six repositories, most of it `git status` in the largest. ripgrep took 11-86 ms with files in memory and up to about 1 s from disk.
- From nothing running, Lemma took 0.5-6.6 s to its first answer; ripgrep was faster for every session of up to 32 questions.

The Mac figures on this page come from a machine where opening files is unusually slow, which made text search look slower than it is; on the Linux machine, ripgrep searched monorepo A in 86 ms rather than 2.3 s.

### Time to the first answer

When nothing is running yet (the daemon is stopped, the index exists), `lemma index` followed immediately by `lemma refs`:

| Lookup | Index | First lookup | Total |
| --- | --- | --- | --- |
| monorepo A Python | 0.28 s | 2.1 s | 2.4 s |
| monorepo A TypeScript | 0.40 s | 3.0 s | 3.4 s |
| monorepo B TypeScript | 0.17 s | 6.9 s | 7.1 s |
| monorepo B Go | 0.16 s | 1.2 s | 1.3 s |

The first lookup waits for the language server to load the project. `lemma index` starts the servers in the background, so an agent that does anything else for a few seconds in between finds them ready: with 10 seconds between the two commands, the first lookup took 72 ms on monorepo B TypeScript and 136 ms on monorepo A TypeScript.

## Lemma's own commands

### Indexing

| Scenario | monorepo A, first version | monorepo A now | monorepo B, first version | monorepo B now |
| --- | --- | --- | --- | --- |
| First index | 33.1 s | 5.4-6.0 s | 2.86 s | 0.53-0.58 s |
| Re-index, nothing changed | 31.9 s | **32 ms** | 2.69 s | **25 ms** |
| Re-index after editing 1 file | 35.8 s | **85 ms** | 2.78 s | **75 ms** |
| Re-index after editing 10 files | 34.3 s | **91 ms** | 2.91 s | **78 ms** |

An edit costs one `git status` limited to the changed files (about 50 ms here) plus parsing them; nothing scales with the size of the repository.

### Symbol search

| Query | First version | Now |
| --- | --- | --- |
| monorepo A, exact name | 97 ms | 16 ms |
| monorepo A, substring (`get_user`) | 120 ms | 19 ms |
| monorepo A, substring with 49,000 matches (`execute`) | 151 ms | 27 ms |
| monorepo B, exact name | 21 ms | 17 ms |

## Key changes

Each change was measured against the previous best and kept only if it helped without hurting correctness.

| Change | Effect |
| --- | --- |
| Parse files on every core | First index on monorepo A 33 s → 7.8 s. |
| Re-index incrementally: skip files whose size, timestamps and inode are unchanged; parse only changed contents; rewrite only changed rows | Re-index with nothing changed 5.5 s → 1.7 s. |
| One parser per thread, created only when needed | 1.7 s → 0.71 s. Building a parser compiles every Tree-sitter query (about 10 ms), and the first version did that hundreds of times per run. |
| Walk directories in parallel; run Git, the walk and loading the stored index at the same time | 0.71 s → 0.34 s. |
| Ask Git about every file only on the first index or a new commit, otherwise only about changed files | 0.34 s → 0.15 s. |
| Run every command in a per-user daemon; the `lemma` command only forwards it | Cached lookups 43 ms → 16 ms, the process floor. |
| Keep language servers running in the daemon, telling them which files changed between snapshots | Lookups after the first: TypeScript 2.8 s → about 100-250 ms for a new symbol and 32-64 ms when repeated; Python similar. |
| Use ty as the first Python server | monorepo A lookups 6-10 s (pyright) → 2 s cold, under 20 ms warm. |
| Keep documents open in warm servers instead of reopening them per lookup | Server time per lookup 43 ms → 29 ms. |
| Watch indexed directories and update the in-memory index per changed file, with an order-independent fingerprint | Re-index on monorepo A: nothing changed 147 ms → 32 ms, one edit 213 ms → 85 ms. |
| Start `git status` for the changed paths before parsing, in parallel | Part of the above. |
| Start language servers in the background after indexing, opening a file from the densest directory | First lookup after a short pause: seconds → 72-136 ms. |
| Use the name indexes for exact search; search substrings in memory with a vectorised scan | Exact 97 ms → 16 ms, substring 120 ms → 19 ms. |

## Idle language servers

Warm servers make lookups fast, so the daemon keeps them and looks after them when nobody asks anything (see [`lemma daemon`](commands.md#lemma-daemon)):

| Measured | Result |
| --- | --- |
| Resuming a paused gopls on the next lookup | About 45 ms (115 ms against 70 ms for a running server) |
| Paused server's CPU use | None; the whole process group is stopped with `SIGSTOP` |
| Starting servers again after an edit in a project whose servers were stopped | Ready about 20 s later, before the next lookup in most agent loops |
| Login service on Linux (systemd user unit) | Starts at login, restarts after a crash, stays stopped after `lemma daemon stop` |

## Python servers measured

Server choice matters most for Python. Besides ty (the default), pyright and the jedi library were measured on Linux, on django (about 2,900 files) and monorepo A (71,500 Python files), with the same random symbols as the comparison:

| Repository | pyright mode | Load | Peak memory | First lookup, median / p90 | Repeat |
| --- | --- | --- | --- | --- | --- |
| django | open files only (Lemma's setting) | 1.6 s | 1.5 GB | 400 ms / 1.2 s | 76 ms |
| django | whole workspace | 69 s | 2.8 GB | 49 ms / 143 ms | 40 ms |
| monorepo A | open files only | 4 s | 4.1 GB | 1.1 s / 128 s | 947 ms |
| monorepo A | whole workspace | 30 min | 4.2 GB | 1.5 s / 183 s | 1.5 s |

- pyright keeps no reference index. Every lookup searches the project's text for the name and then checks each candidate, so analysing the workspace up front helps on a few thousand files and not at all on monorepo A.
- jedi resolves one position in 3-8 ms, but its project-wide reference search is capped internally (30 parsed files) and missed references for 2 of 32 django symbols and 4 of 32 monorepo A symbols. Resolving every identifier in monorepo A (40.8 million) one at a time would take about 38 hours on one core.
- ty answered monorepo A lookups in about 2 s cold and under 20 ms warm, with the most memory (more than 6.6 GB). It stays the default; pyright in workspace mode is a reasonable choice for repositories of a few thousand files.

## Correctness bugs found by benchmarking

- **pyright** published diagnostics for the opened file before it had found the project's files, and answered from the files it knew. On monorepo A it returned 1 of 4 references, and Lemma cached that.
- **typescript-language-server** published diagnostics and went quiet before it started loading the project. On monorepo A it returned no references for a function used in 183 places.
- **ty** does not count import statements as references unless declarations are included, which cut Python recall to 57-75%. Lemma now always asks for declarations and removes only the definition itself.

Providers can declare the message they send once a project is loaded (`ready_messages`), and Lemma waits for it. See [Language servers](providers.md).

## Tried and not kept

| Approach | Why not |
| --- | --- |
| SQLite tuning for the first index (`synchronous`, larger cache, rebuilding indexes after the bulk insert) | Writing 538k symbol rows stayed at 1.6-1.7 s. |
| mimalloc as the memory allocator | No measurable change; Tree-sitter allocates through C `malloc`. |
| A trigram full-text index in SQLite for substring search | 7 ms per query, but 4.6 s and 45 MB more on monorepo A's first index, and slower for common substrings. The in-memory search is as fast without either cost. |
| Restricting `git status` to supported extensions to make it cheaper | Same cost. Kept anyway, because it defines what "uncommitted changes" means. |
| Killing an unneeded `git status` | Left a stale `.git/index.lock`. Lemma now runs Git with `--no-optional-locks`, so it never writes the user's index. |
| pyrefly as the Python server | Returned references from the open file only. |
| basedpyright, or pyright kept warm | About 5.5 s per lookup on monorepo A even when warm: it re-reads the project for every request. |
| pyright analysing the whole workspace up front | 30 minutes to load monorepo A, and lookups no faster (see above). |
| jedi's project-wide reference search | Fast, but capped internally and missed references. |
| Opening the first file by path to warm a server | On monorepo B it opened a configuration file at the root, so the server loaded the wrong project first: the first lookup took 15.8 s instead of 6.9 s. |

## Open opportunities

- **Edits** still pay for one `git status` process (about 50 ms here). Comparing file contents with the commit in-process would remove it.
- **Lookups with thousands of results** spend most of their time naming the enclosing function of each reference, one query each.
- **The first lookup of each symbol** costs the language server's own search (about 100-250 ms here, seconds for very common names). Lemma cannot make the server faster, but it could store answers as symbol occurrences shared across lookups and fill them in while idle, so most lookups never reach a server.
- **The first index** of monorepo A takes 5-6 s. Writing to SQLite while parsing, and a hand-written tree walk instead of Tree-sitter queries, could shorten it.
- **The daemon** runs one command at a time.
