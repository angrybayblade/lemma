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
        "agent_seconds": result.agent_seconds if result.agent_seconds is not None else result.wall_seconds,
        "wall_seconds": result.wall_seconds,
        "cpu_seconds": result.resources.get("cpu_seconds", 0.0),
        "memory_peak_mb": result.resources.get("memory_peak_bytes", 0) / 2**20,
        "uncached_input_tokens": activity.uncached_input_tokens,
        "output_tokens": activity.output_tokens,
        "tool_calls": activity.tool_calls,
        "shell_commands": activity.commands,
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


PRIMARY = {"localization": "symbol_f1", "impact": "location_f1"}
SCORE_FIELDS = {
    "localization": ["symbol_f1", "symbol_recall", "symbol_precision", "file_recall"],
    "impact": ["location_f1", "location_recall", "location_precision", "file_recall"],
}
COST_FIELDS = [
    "agent_seconds",
    "wall_seconds",
    "uncached_input_tokens",
    "output_tokens",
    "tool_calls",
    "shell_commands",
    "lemma_calls",
    "cpu_seconds",
    "memory_peak_mb",
]
EFFECT_FIELDS = ["agent_seconds", "uncached_input_tokens"]


def render(rows: list[RunRow], arms: list[str]) -> str:
    """A Markdown report per track: each arm's means, each arm's paired
    effect against the baseline, and the primary score per task."""
    lines = ["# Round report", ""]
    for track in ("localization", "impact"):
        track_rows = [row for row in rows if row.task.track == track]
        if not track_rows:
            continue
        tasks = sorted({row.task.id for row in track_rows})
        primary = PRIMARY[track]
        lines += [f"## {track} ({len(tasks)} tasks, {len(track_rows)} runs)", ""]
        lines += arm_table(track_rows, arms, SCORE_FIELDS[track] + COST_FIELDS) + [""]
        if BASELINE in arms:
            lines += effect_table(track_rows, arms, [primary, *EFFECT_FIELDS]) + [""]
        lines += per_task_table(track_rows, arms, primary) + [""]
    return "\n".join(lines) + "\n"


def arm_table(rows: list[RunRow], arms: list[str], fields: list[str]) -> list[str]:
    """One row per arm: the share of runs that answered, then per-task means."""
    lines = ["| Arm | answered | " + " | ".join(fields) + " |", "|" + " --- |" * (len(fields) + 2)]
    for arm in arms:
        arm_rows = [row for row in rows if row.arm == arm]
        if not arm_rows:
            continue
        answered = sum(row.answered for row in arm_rows) / len(arm_rows)
        cells = []
        for field in fields:
            values = [value for (_, a), value in per_task_means(arm_rows, field).items() if a == arm]
            cells.append(fmt(field, statistics.fmean(values)) if values else "–")
        lines.append(f"| {arm} | {answered:.0%} | " + " | ".join(cells) + " |")
    return lines


def effect_table(rows: list[RunRow], arms: list[str], fields: list[str]) -> list[str]:
    """Each arm minus the baseline, paired by task, with 95% intervals."""
    lines = ["| Arm − baseline | " + " | ".join(fields) + " |", "|" + " --- |" * (len(fields) + 1)]
    for arm in arms:
        if arm == BASELINE:
            continue
        cells = [effect_cell(field, paired_effect(rows, field, arm)) for field in fields]
        lines.append(f"| {arm} | " + " | ".join(cells) + " |")
    return lines


def effect_cell(field: str, effect: tuple[float, float, float] | None) -> str:
    if effect is None:
        return "–"
    mean, low, high = (fmt(field, value, signed=True) for value in effect)
    return f"{mean} ({low}, {high})"


def per_task_table(rows: list[RunRow], arms: list[str], field: str) -> list[str]:
    means = per_task_means(rows, field)
    tasks = sorted({row.task.id: row.task for row in rows}.values(), key=lambda t: t.id)
    lines = [f"| Task ({field}) | Language | " + " | ".join(arms) + " |", "|" + " --- |" * (len(arms) + 2)]
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
