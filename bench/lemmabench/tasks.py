"""Benchmark tasks: what an agent is asked, and the answer it is scored against."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path

LOCALIZATION = "localization"
IMPACT = "impact"


@dataclass(frozen=True)
class Source:
    """Where a task's code comes from: a Git repository at one commit."""

    repo: str
    """`owner/name` on GitHub, or a local corpus name for impact tasks."""
    commit: str
    url: str
    """Something `git fetch` accepts: a URL or a local path."""


@dataclass
class Task:
    """One question for an agent about one repository snapshot."""

    id: str
    track: str
    language: str
    source: Source
    brief: str
    """What the agent is told, beyond the shared instructions for the track."""
    gold: dict = field(default_factory=dict)
    """The answer key. Localization: `files` and `symbols` (`path::Qualified.name`).
    Impact: `locations` (`path:line`, lines from 1)."""
    origin: dict = field(default_factory=dict)
    """Provenance: the dataset or experiment the task came from."""

    def to_json(self) -> str:
        return json.dumps(asdict(self), sort_keys=True)

    @staticmethod
    def from_json(line: str) -> Task:
        raw = json.loads(line)
        raw["source"] = Source(**raw["source"])
        return Task(**raw)


def load_tasks(path: Path) -> list[Task]:
    """Tasks from a JSON Lines file, in file order."""
    lines = path.read_text().splitlines()
    return [Task.from_json(line) for line in lines if line.strip()]


def save_tasks(path: Path, tasks: list[Task]) -> None:
    """Writes tasks sorted by id, so regenerating them gives a stable diff."""
    path.parent.mkdir(parents=True, exist_ok=True)
    ordered = sorted(tasks, key=lambda task: task.id)
    path.write_text("".join(task.to_json() + "\n" for task in ordered))
