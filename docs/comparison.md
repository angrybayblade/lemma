# Lemma vs grep, ripgrep, git grep and ast-grep

Agents usually answer "where is this used?" with a text search. This page compares Lemma with grep, git grep, ripgrep and ast-grep on that question: where Lemma is better, where it is not, and how far the numbers can be trusted.

## Summary

Lemma answers a narrower question than the other tools, and answers it better:

- **It reports only real references.** On a name shared by several symbols, text search reports every one of them; on common names, 98% or more of what it reports is noise. Lemma's precision was 100% on every symbol tested, checked against two independent implementations for TypeScript.
- **It is faster when a question is asked on a large repository with Lemma already running.** On a 73,000-file repository, a lookup took about 100-250 ms the first time a symbol was asked and 17-84 ms after that, against 2.0-2.3 s for ripgrep on the same machine.
- **It says where each reference sits**: the enclosing function or class, and exact positions in JSON.

The other tools are better at everything else:

- **One-off questions and small repositories.** Lemma first has to index the code (5-6 s for 73,000 files) and start a language server (1-7 s). On a 5,500-file repository, ripgrep (34-62 ms) beat Lemma's first lookup of a symbol (about 100-470 ms).
- **Anything that is not a code reference.** Strings, comments, configuration, documentation, other languages, and names built at run time. A rename needs those too, such as `mock.patch("functions.config.get_secret")`, which Lemma does not report.
- **Very common names.** For `execute`, defined 500 times, the language server took 11.5 s the first time; ripgrep took 2 s, though nearly everything it returned was a different `execute`.
- **No setup and no state.** Text search needs no index, no language server, no running process, and always sees the files as they are now.

## How it was measured

Ten symbols from two private monorepos, from rare to very common names:

- **functions**: 73,620 indexed files (71,506 Python, 2,113 TypeScript), 426 MB, 538,062 symbols.
- **platform**: 5,507 files (3,667 TypeScript, 1,257 Python, 583 Go), 36 MB.

| Tool | Command |
| --- | --- |
| grep | `grep -rnw --include=<ext> --exclude-dir=node_modules --exclude-dir=.git NAME .` (BSD grep 2.6) |
| git grep | `git grep -nw NAME -- <globs>` |
| ripgrep | `rg -nw -t <type> NAME .` (15.2) |
| ast-grep | every identifier-like node whose text is `NAME` (0.51) |
| Lemma | `lemma refs <symbol>`, with ty (Python), typescript-language-server (TypeScript) and gopls (Go) |

**Ground truth**: every place the name appears as a word was checked by asking a language server "go to definition" there; it counts as a reference if the answer is the target symbol. Python was checked with pyright, a different implementation from ty, which Lemma uses. TypeScript was checked twice: with typescript-language-server, and with TypeScript 7's separate native implementation (`tsc --lsp`). Go was checked with gopls only. For the two symbols with thousands of matches, 400 random matches were checked, plus every line Lemma reported.

Precision is the share of reported lines that are real references; recall is the share of real references reported. The definition line itself is not counted.

All timings are medians on one machine, an Apple M4 Max (14 cores) running macOS 15, with file contents cached in memory.

## Results

### Accuracy

Precision / recall per tool:

| Symbol | Real references | grep | git grep | ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- | --- | --- | --- |
| Python `get_md5` | 4 | 19% / 100% | 80% / 100% | 80% / 100% | 100% / 100% | 100% / 100% |
| Python `get_secret` (2 definitions) | 26 | 72% / 100% | 76% / 100% | 76% / 100% | 90% / 100% | 100% / 96% |
| Python `get_keyring` | 7 | 54% / 100% | 54% / 100% | 54% / 100% | 100% / 100% | 100% / 100% |
| Python `McpExecuteResult.to_dict` (7 definitions) | 3 | 0% / 100% | 21% / 100% | 21% / 100% | 25% / 100% | 100% / 100% |
| Python `Action.execute` (500+ definitions) | none in 400 sampled matches | 0% | 0% | 0% | 0% | 100% (9 lines) |
| TypeScript `createUrlFieldValidator` | 183 | 97% / 100% | 97% / 100% | 97% / 100% | 100% / 100% | 100% / 100% |
| TypeScript `getOrgJwtConfig` | 1 | 100% / 100% | 100% / 100% | 100% / 100% | 100% / 100% | 100% / 100% |
| TypeScript `logger` (7 definitions) | 252 of 400 sampled | 65% / 100% | 65% / 100% | 65% / 100% | 73% / 100% | 100% / 100% |
| Go `queue.New` (2 definitions) | 15 | 2% / 100% | 2% / 100% | 2% / 100% | 2% / 100% | 100% / 100% |
| Go `isvcauth.New` (used from another module) | 6 | 1% / 100% | 1% / 100% | 1% / 100% | 1% / 100% | 100% / 50% |

With TypeScript's native implementation as the oracle instead, `createUrlFieldValidator` and `getOrgJwtConfig` came out the same, and `logger` gave text search 59% (ast-grep 65%) and Lemma 100% / 100%.

The gap between ripgrep and ast-grep is matches in comments and strings. The gap between ast-grep and the truth is other symbols with the same name, which only type information can tell apart.

### Latency

| Symbol | grep | git grep | ripgrep | ast-grep | Lemma, first ask | Lemma, asked again | Lemma, cached |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Python `get_md5` | 10.9 s | 4.19 s | 2.29 s | 4.04 s | 1.90 s* | 32-84 ms | 17 ms |
| Python `get_secret` | 7.03 s | 4.20 s | 2.28 s | 4.11 s | 128 ms | 35-67 ms | 18 ms |
| Python `get_keyring` | 10.3 s | 4.16 s | 2.30 s | 3.99 s | 99 ms | 32-62 ms | 18 ms |
| Python `to_dict` | 8.56 s | 4.20 s | 2.32 s | 4.00 s | 119 ms | 33-65 ms | 19 ms |
| Python `execute` | 18.1 s | 4.24 s | 2.02 s | 4.10 s | 11.5 s | 342-617 ms | 18 ms |
| TypeScript `createUrlFieldValidator` | 914 ms | 139 ms | 80 ms | 102 ms | 234 ms | 32-64 ms | 18 ms |
| TypeScript `getOrgJwtConfig` | 293 ms | 68 ms | 57 ms | 270 ms | 108 ms | 20-48 ms | 17 ms |
| TypeScript `logger` (2,046 references) | 341 ms | 67 ms | 62 ms | 265 ms | 1.17 s | 207-420 ms | 28 ms |
| Go `queue.New` | 98 ms | 42 ms | 34 ms | 85 ms | 112 ms | 17-50 ms | 17 ms |

- **First ask**: the first time this symbol was asked, on a language server already running and loaded. This is the number that matters most for an agent exploring code.
- **Asked again**: the same question again without the cache (`--refresh`). Servers keep internal caches, so this is faster than the first ask. The low end is from repeated runs; the high end from a single repeat.
- **Cached**: Lemma's own cache, valid until the code changes.
- \* The first Python question after the server started; ty was still loading parts of the project.

Before any of this, Lemma needs an index and a running server. From nothing running, with the index already built, `lemma index` and the first lookup took 2.4 s (functions Python), 3.4 s (functions TypeScript), 7.1 s (platform TypeScript) and 1.3 s (platform Go). Building the index from scratch takes 5-6 s for functions.

## Where the numbers could be biased

| Possible bias | Effect | What was done |
| --- | --- | --- |
| The ground truth for TypeScript and Go comes from the same servers Lemma uses, so their mistakes would agree. | Lemma's precision there could look better than it is. | TypeScript was rechecked with an independent implementation; the conclusions held. Go was not rechecked. Python always used an independent server. |
| The ground truth only considers places where the name appears. | Text search's 100% recall holds by construction. References that do not spell the name (`import f as g` then `g()`, `getattr`, re-exports under another name) are invisible to every tool here, including any Lemma would find. | Not measured. |
| A string or comment naming the symbol does not count as a reference. | Text search is marked down for matches a rename or deletion would need, such as `mock.patch("functions.config.get_secret")`. | Called out as a trade-off below. ast-grep's column shows the effect of removing comments and strings alone. |
| The symbols were chosen by hand, half of them for being ambiguous, and two after seeing how Lemma behaved on them. | The 0-2% precision of text search applies to common, shared names, not to code in general. On rare names text search scored 80-100%. | All ten are reported, including the two where Lemma loses recall. |
| The first published "warm" Lemma figures repeated the same question 10 times. | They showed 17-35 ms; a symbol's first ask is usually 100-250 ms and can take seconds. | Corrected here: first ask, repeated and cached are reported separately. |
| Lemma's set-up costs are not in the per-question numbers. | One question from nothing running costs Lemma 1.3-7.1 s, about the same as or more than ripgrep. | Reported above as time to the first answer. |
| File opens on this machine barely scale across threads: ripgrep with 14 threads took 1.24 s for a search with no matches, and 1.57 s with one thread. Security software checking each file open is the likely cause. | Text search is slower here than on a typical machine, so Lemma's lead on functions is overstated, perhaps several times. Lemma rarely opens files per question, so it is barely affected. | Not corrected; no other machine was measured. On the smaller repository, where this matters less, ripgrep already beats Lemma's first ask. |
| grep searched ignored directories such as `build/`. | It reported copies of files (1,358 lines for `to_dict`) and was the slowest. | It stands for naive use; ripgrep and git grep are the fair text baselines. |
| The ast-grep query was written for this comparison. | ast-grep is a structural search and rewrite tool, not a reference finder; a different rule could score differently. | The rule matches every identifier, member and type name equal to the symbol, the most a type-blind search can do. |
| Two symbols were sampled (400 matches each). | Their figures have sampling error; `execute`'s recall could not be estimated, because no real reference fell in the sample. | Every line Lemma reported for them was checked, so its precision is exact. |
| Python ground truth comes from pyright, which treats some things differently from ty. | `get_secret`'s 96% is an alternative definition in a `try/except ImportError` block that pyright merges with the real one and ty does not. | Reported as is. |
| Results are counted per line, from one run, on one machine. | Several uses on one line count once; run-to-run variation is not shown beyond the ranges given. | Same for all tools. |

## Trade-offs

| | grep / git grep / ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- |
| What it matches | Text | Syntax nodes | Symbols, resolved by a language server |
| Same name, different symbol | Reported | Reported | Not reported |
| Comments, strings, config, docs | Reported | Not reported | Not reported |
| Languages | Any text | About 25 | TypeScript, Python, Go |
| Set-up | None | None | Index (seconds), language servers installed |
| First question | Full cost every time | Full cost every time | Seconds while servers start; ~100-250 ms per new symbol after |
| Repeated questions | Full cost every time | Full cost every time | 17-80 ms |
| Freshness | Always current | Always current | As of the last `lemma index`, which is cheap (25-85 ms) but must be run |
| Background cost | None | None | A daemon and one language server per language and project, holding memory (12 MB for the daemon and 14-140 MB per server as measured here; macOS compresses memory, so the true footprint may be larger) |
| Failure mode | Too much output, never a missed spelling | Misses names in strings | Misses what the language server cannot see: other modules without `go.work`, dynamic code, projects that fail to load |

## Which to use

- **Use Lemma** to trace a function, method or class through code before changing it, especially when its name is common or shared, and when asking many questions about the same repository.
- **Use ripgrep** for one-off questions, small repositories, text that is not a code reference (log messages, configuration keys, strings), languages Lemma does not support, and as a cross-check before deleting or renaming anything: a rename has to update strings and documentation too.
- **Use ast-grep** to match or rewrite code shapes; for finding uses of one name it sits between the two, removing comments and strings but not same-named symbols.

An agent gets the best of both by asking Lemma for references and ripgrep for any mentions Lemma does not cover, such as strings and other file types.
