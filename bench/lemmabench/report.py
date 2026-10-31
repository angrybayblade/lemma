"""Summarizing a round: scores and costs per arm, and Lemma's paired effect."""

from __future__ import annotations

import json
import random
import statistics
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

from .events import read_activity
from .runner import BASELINE, RunResult, Setup, run_dir
from .scoring import score
from .tasks import LOCALIZATION, Task

BOOTSTRAP_SAMPLES = 2000


@dataclass
class RunRow:
    """One run, scored and costed."""

    task: Task
    arm: str
    repeat: int
    answered: bool
    scores: dict[str, float]
    """`<level>_<precision|recall|f1>`."""
    costs: dict[str, float]


def collect(setup: Setup, tasks: list[Task], arms: list[str], repeats: int) -> list[RunRow]:
    rows = []
    for task in tasks:
        for arm in arms:
            for repeat in range(repeats):
                directory = run_dir(setup, task, arm, repeat)
                result_file = directory / "result.json"
                if not result_file.is_file():
                    continue
                result = RunResult(**json.loads(result_file.read_text()))
                rows.append(row_for(task, result, directory))
    return rows


def row_for(task: Task, result: RunResult, directory: Path) -> RunRow:
    matches = score(task, result.answer)
    scores = {}
    for level, match in matches.items():
        scores[f"{level}_precision"] = match.precision
        scores[f"{level}_recall"] = match.recall
        scores[f"{level}_f1"] = match.f1
    activity = read_activity(directory / "events.jsonl")
    lemma_calls = sum(count for tool, count in activity.tools.items() if tool.startswith("mcp:lemma."))
    costs = {
        "wall_seconds": result.wall_seconds,
        "cpu_seconds": result.resources.get("cpu_seconds", 0.0),
        "memory_peak_mb": result.resources.get("memory_peak_bytes", 0) / 2**20,
        "uncached_input_tokens": activity.uncached_input_tokens,
        "output_tokens": activity.output_tokens,
        "tool_calls": activity.tool_calls,
        "lemma_calls": lemma_calls,
        "failed_mcp_calls": activity.failed_mcp_calls,
    }
    return RunRow(task, result.arm, result.repeat, result.answer is not None, scores, costs)


def primary_metric(task: Task) -> str:
    return "symbol_f1" if task.track == LOCALIZATION else "location_f1"


def per_task_means(rows: list[RunRow], field: str) -> dict[tuple[str, str], float]:
    """Mean of a score or cost over repeats, by (task id, arm)."""
    groups: dict[tuple[str, str], list[float]] = defaultdict(list)
    for row in rows:
        value = row.scores.get(field, row.costs.get(field))
        if value is not None:
            groups[(row.task.id, row.arm)].append(value)
    return {key: statistics.fmean(values) for key, values in groups.items()}


def paired_effect(rows: list[RunRow], field: str, arm: str, seed: int = 0) -> tuple[float, float, float] | None:
    """Mean per-task difference (arm minus baseline) with a bootstrap 95%
    interval over tasks."""
    means = per_task_means(rows, field)
    task_ids = sorted({task_id for task_id, _ in means})
    diffs = [means[(t, arm)] - means[(t, BASELINE)] for t in task_ids if (t, arm) in means and (t, BASELINE) in means]
    if not diffs:
        return None
    generator = random.Random(seed)
    samples = sorted(statistics.fmean(generator.choices(diffs, k=len(diffs))) for _ in range(BOOTSTRAP_SAMPLES))
    low = samples[int(0.025 * BOOTSTRAP_SAMPLES)]
    high = samples[int(0.975 * BOOTSTRAP_SAMPLES) - 1]
    return statistics.fmean(diffs), low, high


SCORE_FIELDS = {
    "localization": ["file_recall", "file_precision", "symbol_recall", "symbol_precision", "symbol_f1"],
    "impact": ["location_recall", "location_precision", "location_f1", "file_recall"],
}
COST_FIELDS = [
    "wall_seconds",
    "uncached_input_tokens",
    "output_tokens",
    "tool_calls",
    "lemma_calls",
    "cpu_seconds",
    "memory_peak_mb",
]


def render(rows: list[RunRow], arms: list[str]) -> str:
    """A Markdown report: per track, each arm's means and Lemma's effect."""
    lines = ["# Round report", ""]
    for track in ("localization", "impact"):
        track_rows = [row for row in rows if row.task.track == track]
        if not track_rows:
            continue
        tasks = sorted({row.task.id for row in track_rows})
        lines += [f"## {track} ({len(tasks)} tasks, {len(track_rows)} runs)", ""]
        lines += table(track_rows, arms, SCORE_FIELDS[track] + COST_FIELDS)
        primary = "symbol_f1" if track == "localization" else "location_f1"
        lines += [""] + per_task_table(track_rows, arms, primary) + [""]
    return "\n".join(lines) + "\n"


def table(rows: list[RunRow], arms: list[str], fields: list[str]) -> list[str]:
    """Each arm's mean, then each other arm's paired effect against the baseline."""
    compared = [arm for arm in arms if arm != BASELINE] if BASELINE in arms else []
    header = "| Metric | " + " | ".join(arms) + "".join(f" | {arm} − baseline (95% CI)" for arm in compared) + " |"
    lines = [header, "|" + " --- |" * (len(arms) + len(compared) + 1)]
    answered = {
        arm: sum(r.answered for r in rows if r.arm == arm) / max(1, sum(r.arm == arm for r in rows)) for arm in arms
    }
    lines.append("| answered | " + " | ".join(f"{answered[a]:.0%}" for a in arms) + " |" * (len(compared) + 1))
    for field in fields:
        means = per_task_means(rows, field)
        cells = []
        for arm in arms:
            values = [v for (task_id, a), v in means.items() if a == arm]
            cells.append(fmt(field, statistics.fmean(values)) if values else "–")
        cells += [effect_cell(field, paired_effect(rows, field, arm)) for arm in compared]
        lines.append(f"| {field} | " + " | ".join(cells) + " |")
    return lines


def effect_cell(field: str, effect: tuple[float, float, float] | None) -> str:
    if effect is None:
        return ""
    mean, low, high = (fmt(field, value, signed=True) for value in effect)
    return f"{mean} ({low}, {high})"


def per_task_table(rows: list[RunRow], arms: list[str], field: str) -> list[str]:
    means = per_task_means(rows, field)
    tasks = sorted({row.task.id: row.task for row in rows}.values(), key=lambda t: t.id)
    lines = ["| Task | Language | " + " | ".join(f"{a} {field}" for a in arms) + " |", "|" + " --- |" * (len(arms) + 2)]
    for task in tasks:
        cells = [fmt(field, means[(task.id, a)]) if (task.id, a) in means else "–" for a in arms]
        lines.append(f"| {task.id} | {task.language} | " + " | ".join(cells) + " |")
    return lines


def fmt(field: str, value: float, signed: bool = False) -> str:
    sign = "+" if signed else ""
    if field.endswith(("recall", "precision", "f1")):
        return f"{value:{sign}.2f}"
    if field.endswith("tokens"):
        return f"{value:{sign},.0f}"
    return f"{value:{sign}.1f}"
