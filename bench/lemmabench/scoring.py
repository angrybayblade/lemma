"""Scoring answers against the key: precision, recall and F1 per run."""

from __future__ import annotations

from dataclasses import dataclass

from .tasks import LOCALIZATION, Task


@dataclass(frozen=True)
class Match:
    found: int
    wanted: int
    correct: int

    @property
    def precision(self) -> float:
        return self.correct / self.found if self.found else 0.0

    @property
    def recall(self) -> float:
        return self.correct / self.wanted if self.wanted else 1.0

    @property
    def f1(self) -> float:
        total = self.precision + self.recall
        return 2 * self.precision * self.recall / total if total else 0.0


def compare(found: set[str], wanted: set[str]) -> Match:
    return Match(found=len(found), wanted=len(wanted), correct=len(found & wanted))


def normalize_path(path: str) -> str:
    path = path.strip().replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path.lstrip("/")


def score(task: Task, answer: dict | None) -> dict[str, Match]:
    """Matches by level: `file` and `symbol` for localization, `location`
    (and `file`) for impact. A missing answer scores zero everywhere."""
    answer = answer or {}
    if task.track == LOCALIZATION:
        return score_localization(task, answer)
    return score_impact(task, answer)


def score_localization(task: Task, answer: dict) -> dict[str, Match]:
    files = {normalize_path(path) for path in answer.get("files", []) if isinstance(path, str)}
    symbols = set()
    for symbol in answer.get("symbols", []):
        if isinstance(symbol, dict) and symbol.get("path") and symbol.get("name"):
            symbols.add(f"{normalize_path(symbol['path'])}::{symbol['name'].strip()}")
    # A file is answered if it is listed or holds an answered symbol.
    files |= {symbol.split("::", 1)[0] for symbol in symbols}
    return {
        "file": compare(files, set(task.gold["files"])),
        "symbol": compare(symbols, set(task.gold.get("symbols", []))),
    }


def score_impact(task: Task, answer: dict) -> dict[str, Match]:
    locations = set()
    for location in answer.get("locations", []):
        if isinstance(location, dict) and location.get("path") and isinstance(location.get("line"), int):
            locations.add(f"{normalize_path(location['path'])}:{location['line']}")
    wanted = set(task.gold["locations"])
    return {
        "location": compare(locations, wanted),
        "file": compare({loc.rsplit(":", 1)[0] for loc in locations}, {loc.rsplit(":", 1)[0] for loc in wanted}),
    }
