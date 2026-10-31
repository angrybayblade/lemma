"""Command line: select tasks, prepare snapshots, run a round, report.

python3 -m lemmabench select  --lemma BIN [--per-language 4] [--impact-per-corpus 3]
python3 -m lemmabench prepare --lemma BIN
python3 -m lemmabench run     --round NAME --lemma BIN [--repeats 3] [--parallel 4]
python3 -m lemmabench report  --round NAME
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from . import prepare as preparing
from . import report as reporting
from .runner import ARMS, Limits, Setup, execute
from .select import localization_candidates, select_impact
from .tasks import Source, Task, load_tasks, save_tasks

HERE = Path(__file__).resolve().parent.parent
DEFAULT_TASKS = HERE / "tasks"
DEFAULT_HOME = Path.home() / "lemmabench"
DEFAULT_TOOL_PATH = ("~/.local/bin", "~/go/bin", "~/.npm-global/bin", "~/.cargo/bin")


def main() -> int:
    parser = argparse.ArgumentParser(prog="lemmabench")
    parser.add_argument("--home", type=Path, default=DEFAULT_HOME, help="cache and rounds live here")
    parser.add_argument("--tasks", type=Path, default=DEFAULT_TASKS, help="directory of task files")
    commands = parser.add_subparsers(dest="command", required=True)

    select = commands.add_parser("select", help="choose tasks and write them to --tasks")
    select.add_argument("--lemma", type=Path, required=True)
    select.add_argument("--per-language", type=int, default=4)
    select.add_argument("--impact-per-corpus", type=int, default=3)
    select.add_argument("--round3-results", type=Path, default=Path.home() / "bench" / "results")
    select.add_argument("--corpora", type=Path, default=Path.home() / "corpora")
    select.add_argument("--corpus-commits", type=Path, default=Path.home() / "bench" / "corpora.txt")
    select.add_argument("--impact-corpora", default="django,nest,terraform")
    select.add_argument("--seed", type=int, default=11)

    prepare = commands.add_parser("prepare", help="snapshot every task")
    prepare.add_argument("--lemma", type=Path, required=True)

    run = commands.add_parser("run", help="run a round")
    run.add_argument("--round", required=True)
    run.add_argument("--lemma", type=Path, required=True)
    run.add_argument("--arms", default=",".join(ARMS))
    run.add_argument("--repeats", type=int, default=3)
    run.add_argument("--parallel", type=int, default=4)
    run.add_argument("--only", default="", help="comma-separated task ids")
    run.add_argument("--codex-arg", action="append", default=[], help="extra `codex exec` argument")
    run.add_argument("--codex", default="codex", help="Codex executable")
    run.add_argument("--cpu-quota", default=Limits.cpu_quota)
    run.add_argument("--memory-max", default=Limits.memory_max)
    run.add_argument("--timeout", type=int, default=Limits.timeout_seconds)
    run.add_argument("--seed", type=int, default=0)

    report = commands.add_parser("report", help="summarize a round")
    report.add_argument("--round", required=True)
    report.add_argument("--arms", default=",".join(ARMS))
    report.add_argument("--repeats", type=int, default=3)

    args = parser.parse_args()
    return {"select": cmd_select, "prepare": cmd_prepare, "run": cmd_run, "report": cmd_report}[args.command](args)


def all_tasks(directory: Path) -> list[Task]:
    return [task for path in sorted(directory.glob("*.jsonl")) for task in load_tasks(path)]


def cmd_select(args: argparse.Namespace) -> int:
    cache = args.home / "cache"
    lemma = args.lemma.resolve()
    chosen: list[Task] = []
    for language, candidates in localization_candidates(args.seed).items():
        kept = 0
        for task in candidates:
            if kept == args.per_language:
                break
            failures = preparing.prepare(cache, [task], lemma)
            if failures or not task.gold.get("symbols"):
                print(f"skip {task.id}: {failures[0] if failures else 'fix changes no definition'}", file=sys.stderr)
                continue
            chosen.append(task)
            kept += 1
        print(f"{language}: {kept} localization tasks", file=sys.stderr)
    save_tasks(args.tasks / "localization.jsonl", chosen)

    commits = dict(line.split() for line in args.corpus_commits.read_text().splitlines() if line.strip())
    corpora = {
        name: Source(repo=name, commit=commits[name], url=str(args.corpora / name))
        for name in args.impact_corpora.split(",")
    }
    impact = select_impact(
        args.round3_results, corpora, args.impact_per_corpus, min_references=3, max_references=60, seed=args.seed
    )
    save_tasks(args.tasks / "impact.jsonl", impact)
    print(f"impact: {len(impact)} tasks", file=sys.stderr)
    return 0


def cmd_prepare(args: argparse.Namespace) -> int:
    failures = preparing.prepare(args.home / "cache", all_tasks(args.tasks), args.lemma.resolve())
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


def make_setup(args: argparse.Namespace) -> Setup:
    tool_path = tuple(str(Path(path).expanduser()) for path in DEFAULT_TOOL_PATH)
    limits = Limits(cpu_quota=args.cpu_quota, memory_max=args.memory_max, timeout_seconds=args.timeout)
    return Setup(
        cache=args.home / "cache",
        round_dir=args.home / "rounds" / args.round,
        lemma=args.lemma.resolve(),
        limits=limits,
        tool_path=tool_path,
        codex_args=tuple(args.codex_arg),
        codex=args.codex,
    )


def cmd_run(args: argparse.Namespace) -> int:
    setup = make_setup(args)
    tasks = all_tasks(args.tasks)
    if args.only:
        wanted = set(args.only.split(","))
        tasks = [task for task in tasks if task.id in wanted]
    preparing.prepare(setup.cache, tasks, setup.lemma)
    plan = [(task, arm, repeat) for task in tasks for arm in args.arms.split(",") for repeat in range(args.repeats)]
    # Interleave arms and tasks so drift over the round (service load,
    # rate limits) does not land on one arm.
    random.Random(args.seed).shuffle(plan)
    setup.round_dir.mkdir(parents=True, exist_ok=True)
    (setup.round_dir / "setup.json").write_text(
        json.dumps(
            {
                "lemma": str(setup.lemma),
                "limits": setup.limits.__dict__,
                "codex_args": setup.codex_args,
                "arms": args.arms,
                "repeats": args.repeats,
                "tasks": [t.id for t in tasks],
            },
            indent=2,
        )
    )
    failed = 0
    with ThreadPoolExecutor(max_workers=args.parallel) as pool:
        futures = {pool.submit(execute, setup, *item): item for item in plan}
        for done, future in enumerate(as_completed(futures), 1):
            task, arm, repeat = futures[future]
            result = future.result()
            status = "ok" if result.answer is not None else f"NO ANSWER ({(result.error or '')[:80]})"
            failed += result.answer is None
            print(
                f"[{done}/{len(plan)}] {task.id} {arm}#{repeat} {result.wall_seconds:.0f}s {status}",
                file=sys.stderr,
                flush=True,
            )
    print(f"{len(plan) - failed}/{len(plan)} runs answered", file=sys.stderr)
    return 0


def cmd_report(args: argparse.Namespace) -> int:
    setup = Setup(cache=args.home / "cache", round_dir=args.home / "rounds" / args.round, lemma=Path("lemma"))
    arms = args.arms.split(",")
    rows = reporting.collect(setup, all_tasks(args.tasks), arms, args.repeats)
    text = reporting.render(rows, arms)
    (setup.round_dir / "report.md").write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
