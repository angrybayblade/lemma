# Lemma vs grep, ripgrep, git grep and ast-grep

Agents usually answer "where is this used?" with a text search. This page compares Lemma with grep, git grep, ripgrep and ast-grep on that question, measured to a protocol written before any measurement, with symbols drawn at random and the answers checked by language servers independent of Lemma's.

## Summary

- **Lemma reports real references; text search mostly does not.** Averaged over 241 random symbols, 96% of the lines Lemma reported were real references, against 31% for ripgrep, grep and git grep, and 35% for ast-grep. For names that appear on more than 50 lines, text search fell to 1-7%.
- **Text search finds every reference that spells the name; Lemma misses some.** Lemma found 99% of references on average per symbol, but only 90% when counting references across all symbols together, because a few symbols with hundreds of references lost some: calls through interfaces, files outside a TypeScript project, and Python types ty could not infer.
- **Text search finds strings and comments, which a rename needs.** Counting those as wanted, text search's precision rises to 27% of all lines; Lemma never reports them.
- **Lemma is faster only when it is already running.** A symbol's first lookup on a running server took 19-126 ms (median per repository); repeating it took 5-37 ms, and a cached answer 1-3 ms. ripgrep took 11-86 ms with the files in memory and 55 ms-1 s from disk. From nothing running, Lemma needed 0.5-6.6 s for the first answer, against 0.06-1 s for ripgrep, and never caught up within 32 questions.
- **Lemma needs memory.** On a 71,000-file Python repository, ty, Lemma's Python server, needed more than 6.6 GB; on a 7 GB machine it was killed.

## How it was measured

The protocol ([`PROTOCOL`](#protocol)) was written and its hash recorded before any measurement; every change made afterwards is listed with its reason.

**Machine**: an exe.dev virtual machine, AMD EPYC 9554P, 8 vCPUs, Ubuntu 24.04, kernel 6.12, no security software, nothing else running. 7 GB of memory, resized to 31 GB for the last repository (see below).

**Repositories**, at fixed commits, with no dependencies installed for any tool:

| Repository | Languages | Commit |
| --- | --- | --- |
| django/django | Python | `817df1c` |
| hashicorp/terraform | Go | `becd281` |
| nestjs/nest | TypeScript | `b7f8501` |
| mercury (private) | Python, TypeScript | `a5ffba0` |
| platform (private) | TypeScript, Python, Go | `d81c7ef` |

**Symbols**: for each repository and language, every function, method and class Lemma indexed, bucketed by how many lines contain its name as a word (1-5, 6-50, 51-500, more than 500), then 8 drawn at random per bucket (seed 3): 241 symbols in all. A drawn symbol whose own definition an oracle could not resolve was replaced by the next random draw; 19 were (16 of them methods).

**Tools**:

| Tool | Command |
| --- | --- |
| grep | `grep -rnw --include=<ext> --exclude-dir=node_modules --exclude-dir=.git NAME .` (GNU grep 3.11) |
| git grep | `git grep -nw NAME -- <globs>` |
| ripgrep | `rg -nw -t <type> NAME .` (14.1) |
| ast-grep | every identifier-like node whose text is `NAME` (0.51) |
| Lemma | `lemma refs <symbol>` (commit `831de14`), with ty, typescript-language-server and gopls |

**Ground truth**: every place the name appears as a word is a candidate. It is a **reference** if a language server's "go to definition" there returns the target; otherwise a **mention** if it is in a string or comment, or **another symbol** if it is code. Each language was checked by servers other than the one Lemma uses where possible:

| Language | Lemma uses | Checked with | Agreement |
| --- | --- | --- | --- |
| Python | ty | pyright and jedi | 100% of 19,764 candidates |
| TypeScript | typescript-language-server | typescript-language-server and TypeScript 7's native compiler | 100% of 12,906 |
| Go | gopls | gopls only | not independent |

Symbols with more than 1,200 candidates were checked on 400 random candidates, plus every line Lemma reported.

**Metrics**, per line, without the definition itself: precision (share of reported lines that are references), precision with mentions (strings and comments also count as wanted), and recall (share of references reported). "Pooled" adds up lines across symbols, so symbols with many references weigh more; "per symbol" averages each symbol's figure.

**Latency**: medians of repeated runs. Text tools were timed with files cached in memory ("warm") and after dropping the page cache ("cold"). Lemma was timed on each symbol's first lookup on a running, loaded server (asked in random order), a repeat of it, and its cached answer. Sessions started from no Lemma daemon and a dropped cache.

## Results

### Precision and recall by how common the name is

Precision / with mentions / recall, pooled:

| Lines containing the name | grep, git grep, ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- |
| 1-5 | 76% / 89% / 100% | 88% / 88% / 100% | 100% / 100% / 100% |
| 6-50 | 29-30% / 45% / 100% | 35% / 35% / 100% | 99% / 99% / 100% |
| 51-500 | 7% / 34% / 100% | 9% / 10% / 100% | 93% / 93% / 100% |
| more than 500 | 1% / 24% / 100% | 1% / 2% / 100% | 96% / 96% / 61% |

### By language

| Language | grep, git grep, ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- |
| Python | 3% / 29% / 100% | 3% / 4% / 100% | 100% / 100% / 70% |
| TypeScript | 5% / 33% / 100% | 7% / 7% / 100% | 95% / 95% / 99% |
| Go | 2% / 18% / 100% | 3% / 3% / 100% | 91% / 91% / 100% |

### Overall

| | grep, git grep, ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- |
| Precision, per symbol | 31% | 35% | 96% |
| Recall, per symbol | 100% | 100% | 99% |
| Precision, pooled | 3% (27% with mentions) | 4% (5%) | 95% |
| Recall, pooled | 100% | 100% | 90% |

Against the second oracle, Lemma's precision was 97% for Python and 95% for TypeScript.

### Why Lemma loses where it does

- **Calls through an interface.** Every symbol where Lemma reported non-references was a method implementing an interface (Go and TypeScript): the language server counts calls through the interface as uses of the method, while "go to definition" at those calls returns the interface. Such a call may reach this method or another implementation. This is why Go's precision is 91%, and nest's TypeScript 71%.
- **Type inference.** ty found 189 of the 334 calls to django's `Template.render`; the others need type inference ty does not do. That one symbol accounts for most of Python's 70% pooled recall; averaged per symbol, Lemma's recall across all languages was 99%.
- **Project boundaries.** In TypeScript, the language server searches the project it loaded; references in test files outside that `tsconfig.json` were missed.

### Latency

| Repository | grep warm / cold | git grep warm / cold | ripgrep warm / cold | ast-grep warm / cold | Lemma first lookup (median, 90th percentile) | Lemma repeat | Lemma cached |
| --- | --- | --- | --- | --- | --- | --- | --- |
| django Python | 39 ms / 507 ms | 17 ms / 97 ms | 15 ms / 108 ms | 318 ms / 430 ms | 52 ms, 99 ms | 16 ms | 2 ms |
| terraform Go | 32 ms / 462 ms | 11 ms / 53 ms | 14 ms / 94 ms | 392 ms / 477 ms | 95 ms, 174 ms | 7 ms | 2 ms |
| nest TypeScript | 15 ms / 272 ms | 9 ms / 103 ms | 11 ms / 69 ms | 179 ms / 244 ms | 77 ms, 502 ms | 12 ms | 2 ms |
| platform TypeScript | 40 ms / 480 ms | 19 ms / 140 ms | 17 ms / 102 ms | 422 ms / 513 ms | 73 ms, 134 ms | 8 ms | 3 ms |
| platform Python | 17 ms / 274 ms | 7 ms / 41 ms | 16 ms / 64 ms | 107 ms / 172 ms | 23 ms, 48 ms | 9 ms | 2 ms |
| platform Go | 21 ms / 210 ms | 7 ms / 37 ms | 16 ms / 55 ms | 123 ms / 182 ms | 58 ms, 274 ms | 5 ms | 2 ms |
| mercury TypeScript | 90 ms / 804 ms | 160 ms / 383 ms | 33 ms / 156 ms | 95 ms / 237 ms | 19 ms, 79 ms | 8 ms | 3 ms |
| mercury Python (71,000 files) | 483 ms / 6.56 s | 5.29 s / 5.92 s | 86 ms / 1.03 s | 6.33 s / 8.18 s | 126 ms, 240 ms | 37 ms | 1 ms |

On repositories of a few thousand files, ripgrep with warm caches is faster than a symbol's first Lemma lookup, and about as fast as a repeated one. On the 71,000-file repository, ripgrep took 86 ms warm and about 1 s cold, Lemma's first lookup 126 ms and a repeat 37 ms. Lemma's cached answers (1-3 ms) were faster than every text tool on every repository.

### Sessions from nothing running

From a stopped daemon and an empty file cache: `lemma index` followed by N lookups, against N ripgrep searches.

| Repository | 1 question: Lemma / ripgrep | 10 questions | 32 questions |
| --- | --- | --- | --- |
| django Python | 0.77 s / 0.11 s | 1.59 s / 0.30 s | 3.37 s / 0.69 s |
| terraform Go | 3.54 s / 0.10 s | 3.39 s / 0.25 s | 6.65 s / 0.57 s |
| nest TypeScript | 3.16 s / 0.08 s | 8.52 s / 0.20 s | 11.70 s / 0.50 s |
| platform TypeScript | 6.64 s / 0.13 s | 7.42 s / 0.33 s | 9.51 s / 0.79 s |
| platform Python | 0.51 s / 0.09 s | 1.37 s / 0.27 s | 2.59 s / 0.81 s |
| platform Go | 1.56 s / 0.06 s | 3.31 s / 0.29 s | 5.25 s / 0.55 s |
| mercury TypeScript | 5.23 s / 0.16 s | 4.99 s / 0.50 s | 5.14 s / 0.83 s (17 questions) |
| mercury Python | 3.41 s / 1.03 s | 7.12 s / 1.96 s | 11.43 s / 4.27 s |

Most of a cold session is the language server starting and loading the project (1-3 s for TypeScript and Go, about 0.1-0.2 s for ty) and indexing with an empty cache (0.2-0.5 s). Keeping the daemon running removes both; see [How Lemma works](concepts.md#the-daemon-and-warm-language-servers).

## Remaining biases and limits

| Issue | Effect |
| --- | --- |
| Go was checked only with gopls, which Lemma also uses. | Go precision and recall could be off in ways both share. |
| 19 drawn symbols were replaced because an oracle could not resolve their own definition, 16 of them methods (often overrides). | The sample likely has fewer polymorphic methods, Lemma's weakest case, than the code does. |
| Ground truth only considers places that spell the name. | Text search's 100% recall holds by construction; references through another name (aliases, re-exports, dynamic code) are not measured for any tool. |
| No dependencies were installed. | References through third-party packages are out of scope; with dependencies installed, servers may find more, and load more slowly. |
| mercury Python ran with 31 GB of memory, the other seven with 7 GB. | ty needed more than 6.6 GB there; with 7 GB it was killed. File caching for that pair also had more room. |
| Pooled and per-symbol figures differ a lot. | Pooled figures are dominated by a few symbols with hundreds of references; both are reported. |
| One machine, one run per symbol for accuracy. | Run-to-run variation in latency is only partly visible. |

## Earlier measurements

An earlier, smaller comparison (10 hand-picked symbols, on a Mac) showed Lemma 60-130 times faster than ripgrep on mercury. That was mostly the Mac: opening files there barely scaled across threads, so ripgrep took 2.3 s on mercury, against 86 ms on this machine. Its "warm" Lemma figures also repeated the same question, which language servers cache.

## Trade-offs

| | grep / git grep / ripgrep | ast-grep | Lemma |
| --- | --- | --- | --- |
| What it matches | Text | Syntax nodes | Symbols, resolved by a language server |
| Same name, different symbol | Reported | Reported | Not reported |
| Strings and comments | Reported | Not reported | Not reported |
| Languages | Any text | About 25 | TypeScript, Python, Go |
| Set-up | None | None | An index, and language servers installed |
| First question, nothing running | Full cost | Full cost | 0.5-6.6 s here |
| Later questions | Full cost each time | Full cost each time | 19-126 ms for a new symbol (median), 5-37 ms repeated, 1-3 ms cached |
| Freshness | Always current | Always current | As of the last `lemma index` (25-85 ms) |
| Memory | None kept | None kept | A daemon plus a language server per language and project; ty needed more than 6.6 GB on 71,000 Python files |
| Misses | Never misses a spelling | Names in strings | Calls through interfaces (over-reports), files outside a TypeScript project, references needing deeper type inference |

## Which to use

- **Lemma** to trace a function, method or class through code before changing it, especially when its name is common, and when asking many questions about one repository with Lemma already running.
- **ripgrep** for one-off questions, small repositories, strings, configuration and other text, languages Lemma does not support, and as a cross-check before renaming or deleting anything.
- **ast-grep** to match or rewrite code shapes. For finding uses of one name it removes comments and strings, but not other symbols with the same name.

## Protocol

The protocol, harness and per-symbol results live in the experiment workspace, not in this repository, since they include private code. The protocol's SHA-256 before any measurement was `7197f0ff4f6e3354e2f18c23f772397acf9b54ac8456bf6d5bbde892ee806e33`; the changes made afterwards, each with its reason, are:

1. The oracle closes documents it no longer needs and restarts a server that exits (typescript-language-server exited once in the pilot).
2. Text tools are timed on two symbols per bucket; their time depends on the repository, not the symbol.
3. Lemma-only lines are checked up to 1,500 per symbol (no symbol reached the cap).
4. ast-grep searches `.ts` files only, not `.tsx` (3 files in platform).
5. Phases are saved so a failure does not repeat earlier ones.
6. mercury Python was rerun with 31 GB of memory after ty was killed at 7 GB.
7. Language server helpers left running by the Lemma build under test were stopped by hand before that rerun.
