# lemmabench

A small benchmark for coding agents with and without Lemma. Each run is one `codex exec` agent on one task, in its own cgroup, with a fixed set of tools (an *arm*). Results are scored against answer keys and summarized per arm, with each arm's paired effect against a shell baseline. Results so far are in [Agents with and without Lemma](../docs/agent-benchmark.md).

It needs Linux with cgroup v2 delegated to the user's systemd instance (any recent distribution), Python 3.11+, Codex (logged in), ripgrep, and, for the arms that use them, grep, ast-grep, the language servers Lemma uses, and a `lemma` build. Nothing is installed from PyPI.

## Tracks

| Track | Task | Answer key | Scored by |
| --- | --- | --- | --- |
| `localization` | An issue from [SWE-PolyBench](https://huggingface.co/datasets/AmazonScience/SWE-PolyBench_500) (Python, TypeScript) or [SWE-bench Multilingual](https://huggingface.co/datasets/SWE-bench/SWE-bench_Multilingual) (Go): name the files and definitions the fix must change. | The real fix: SWE-PolyBench's own `modified_nodes` where it has them, otherwise computed the same way (functions, methods and classes the fix changes or adds). | File and definition precision, recall, F1 |
| `impact` | A definition with a common name: list every reference to it. | Every whole-word occurrence of the name, checked with go-to-definition by servers Lemma does not use first: pyright and jedi, tsserver and TypeScript 7, gopls. Lines both servers confirm are references; lines a server could only follow to an import are *unknown* and count neither way. | Line precision, recall, F1 |

Tasks live in `tasks/*.jsonl`, committed, so every round runs the same tasks.

## Arms

| Arm | Shell | Search | Reading |
| --- | --- | --- | --- |
| `baseline` | yes | whatever the agent runs | shell |
| `lemma` | yes | shell, plus `lemma mcp` | shell |
| `lemma-guided` | yes | shell, plus `lemma mcp` and one line asking to prefer it (as in `AGENTS.md`) | shell |
| `grep`, `ripgrep`, `ast-grep` | no | that tool only | `read_file`, `list_files` |
| `lemma-only` | no | `lemma mcp` only | `read_file`, `list_files` |

Arms without a shell disable Codex's `shell_tool`, `unified_exec` and `multi_agent` features and run read-only. Codex calls MCP tools through code mode, which cannot run commands without a shell tool. Every arm disables Codex's ChatGPT app tools and web search. The restricted arms' tools come from `lemmabench.toolserver`, which caps every tool's output the same way.

## Isolation

Every run is a transient systemd user service limited to 2 CPUs, 8 GB of memory without swap and 1,024 processes. Each run gets a fresh copy of the task's snapshot (the repository at its commit, history cut to one commit, so the real fix cannot be looked up), its own `CODEX_HOME` and its own temporary directory, so a Lemma daemon and its language servers belong to that run and die with it. Peak memory and CPU time are read from the run's cgroup. Runs are ordered randomly so drift over a round does not land on one arm.

## Commands

```sh
cd bench
python3 -m lemmabench select --lemma BIN                  # choose tasks, build answer keys
python3 -m lemmabench rekey                               # recompute impact keys for the same tasks
python3 -m lemmabench run --round NAME --lemma BIN [--warm] [--arms a,b] [--repeats 3] [--parallel 4]
python3 -m lemmabench report --round NAME [--arms a,b] [--baseline-round OTHER]
```

`--warm` starts the agent once Lemma's language servers are up, as in a session where the daemon is already running; the wait is recorded but not counted. Reports compare `agent_seconds`, the agent's own time, without indexing or warm-up. A run that already has a `result.json` is skipped, so a crashed round resumes where it stopped. `--baseline-round` reuses another round's baseline runs; prefer a baseline in the same round, since token counts drift between rounds.

Runs and reports go to `~/lemmabench/rounds/NAME/`. `report.md` has per-arm means, paired effects with 95% bootstrap intervals over tasks, and per-task scores.

## Tests

```sh
python3 -m unittest tests.test_lemmabench tests.test_toolserver
```
