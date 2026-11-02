# Agents with and without Lemma

[The comparison](comparison.md) measured Lemma as a tool: how many of the lines it reports are real references, and how fast. This page measures what that buys an agent. Codex agents were given the same tasks with different tools: a shell, Lemma, or a single search tool (grep, ripgrep, ast-grep, or Lemma) with no shell. They were scored on their answers and costed in time and tokens. The harness is [`bench/`](../bench/README.md).

## Summary

- **Agents were equally accurate with every tool except ast-grep.** On 30 localization tasks from SWE-PolyBench and SWE-bench Multilingual, every arm found 0.63-0.69 of the definitions a fix changes (F1), and no difference was larger than its 95% interval. On 11 impact tasks (list every reference to a commonly named definition), every arm scored 1.00 once the answer keys were corrected, and so did grep alone with no shell. A capable agent reads the matches of a text search and drops the wrong ones. ast-grep was the one tool that cost accuracy (0.92 on impact), and it once ran out of memory.
- **Lemma's advantage showed up as cost, and only when its server was already running.** With the daemon warm, agents using Lemma for impact tasks read 2-3.5k fewer input tokens per task than with a shell alone, at a similar time. An agent limited to ripgrep read 6.8k more. On localization tasks, any arm without a shell took 6-7 s longer, ripgrep included: a shell chains `rg | head` and `sed` into one command, a tool takes one call each.
- **Agents do not choose Lemma unprompted.** With Lemma's tools available next to a shell, they made no Lemma calls in hundreds of runs, even after the tool descriptions said when to prefer them. One line of guidance, as a user would put in `AGENTS.md`, was enough for them to use it.
- **The benchmark found four Lemma bugs, now fixed.** In a TypeScript monorepo, references stopped at the boundary of the project that defines the symbol (3 of 27 found), and then, right after the fix, came back empty while the other projects loaded. Agents reacted to an incomplete answer by reading every file in the repository: thousands of tool calls for one task.

## Setup

Codex CLI 0.165 with its default model, one agent per run, on an 8-core, 31 GB Linux VM. Each run is limited to 2 CPUs and 8 GB, uses a fresh copy of the repository with its history cut to one commit, and has no web or ChatGPT app tools. Lemma arms index the project before the agent starts; *warm* rounds also wait for Lemma's language servers to be up, as in a session where the daemon is already running, and that wait is not counted. Times are the agent's own (`agent_seconds`), tokens are uncached input tokens. Each task runs three times per arm; effects are means of per-task differences against a baseline in the same round, with 95% bootstrap intervals over tasks.

### Tasks

| Track | Tasks | Source | Answer key |
| --- | --- | --- | --- |
| Localization | 30: 10 Python, 10 TypeScript, 10 Go, at most a few per repository (vscode, transformers, keras, langchain, angular, svelte, material-ui, caddy, hugo, gin, terraform, prometheus, ...) | SWE-PolyBench 500 (Python, TypeScript), SWE-bench Multilingual (Go): small fixes to 1-3 existing source files | SWE-PolyBench's `modified_nodes` where available, otherwise the functions, methods and classes the fix changes or adds |
| Impact | 11 in django, nest and terraform; names occurring 54-328 times (two with 300+), 5-81 references | Definitions picked at random among common names, interface methods excluded | Every occurrence checked by go-to-definition with two servers Lemma does not use first (pyright and jedi; tsserver and TypeScript 7), gopls for Go |

## Results

### Search tools head to head (round 2, servers cold)

Agents without a shell, each with one search tool and the same two reading tools. 12 localization and 9 impact tasks, 3 runs each.

| Arm | Localization F1 | Impact F1 | Agent time vs shell (loc / impact) | Input tokens vs shell (loc / impact) |
| --- | --- | --- | --- | --- |
| shell (baseline) | 0.65 | 1.00 | | |
| grep | 0.62 | 1.00 | +2.6 s / +2.2 s | +2.0k / +1.9k |
| ripgrep | 0.68 | 1.00 | +2.7 s / +2.3 s | +2.4k / +1.6k |
| ast-grep | 0.62 | 0.96 | +5.1 s / +19.6 s | +1.9k / +6.7k |
| Lemma | 0.62 | 1.00 | +6.9 s / +18.9 s | +2.6k / +2.9k |

Lemma's impact cost here was almost entirely one TypeScript task where it returned 3 of 27 references and the agent read the whole repository. Lemma has since been fixed (see below). Impact F1 is rescored with the corrected answer keys.

### With the server warm (round 5)

30 localization and 11 impact tasks, 3 runs each, Lemma with the fixes and the changes kept below.

| Arm | Localization F1 | Impact F1 | Agent time vs shell (loc / impact) | Input tokens vs shell (loc / impact) |
| --- | --- | --- | --- | --- |
| shell (baseline) | 0.66 | 1.00 | | |
| ripgrep only | 0.63 | 1.00 | +5.9 s / +0.8 s | +2.6k / **+6.8k** |
| shell + Lemma, guided | 0.69 | 1.00 | +7.0 s / +6.2 s | +1.9k / **−3.5k** |
| Lemma only | 0.66 | 1.00 | +7.0 s / +1.8 s | +5.0k / −2.0k |

All localization differences are inside their intervals (for example, guided Lemma +0.03, from −0.02 to +0.11).

## What was changed, and why

Each change was measured on the Lemma arms against a baseline; kept only if it helped.

| Change | Result | Kept |
| --- | --- | --- |
| Mark MCP tools read-only | Codex rejected every Lemma call in unattended runs until tools carried MCP annotations | yes |
| Load every TypeScript project that may reference a symbol | References in nest: 3 of 27 → 27 of 27; lemma-only impact tool calls 397 → 5 | yes |
| Ask again until newly loaded projects settle, ignoring the declaration itself | The first query on a warm server no longer comes back empty | yes |
| Tool descriptions saying when Lemma beats text search | No change in how often agents chose Lemma | yes, with the next row |
| Refresh the index per query; compact text results instead of JSON | Guided arm, impact: input tokens 11.3k → 6.7k, time 28 → 23 s | yes |
| Include each reference's line text | No fewer calls, slightly more tokens | no |

## Caveats

- **The tasks are within a strong agent's reach without Lemma.** Impact tasks with up to 81 references, and localization tasks whose fix touches 1-3 files, do not stress text search enough to separate the tools on accuracy. Tasks with hundreds of references were rare in these repositories: two qualified, and every arm solved both.
- **One agent, one model.** Codex's default model; a weaker model may lean on tool quality more.
- **Answer keys have limits.** Impact keys depend on go-to-definition; uses reached only through an unresolved import or path alias are unknown and not scored. Localization keys use SWE-PolyBench's node definitions, and an agent naming a class instead of the changed method scores a miss.
- **Warm rounds exclude the start-up wait** (80-100 s for the Lemma arms here), which a real session pays once, at its start.
- **Token counts drift between rounds** (cache hits vary), so only effects against a baseline in the same round are compared.
