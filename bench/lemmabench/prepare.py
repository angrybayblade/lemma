"""Snapshots of each task's code, and the localization answer keys.

A snapshot is the repository at the task's commit with its history cut
away: one commit, so an agent can use `git diff` but cannot look up the
real fix in later commits.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

from .gold import changed_definitions, definitions_from_outline, is_test_path
from .patches import parse_diff
from .tasks import LOCALIZATION, Task

GIT_IDENTITY = ["-c", "user.name=lemmabench", "-c", "user.email=lemmabench@localhost"]


def run(command: list[str], cwd: Path | None = None, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(
        command, cwd=cwd, check=True, text=True, capture_output=True, stdin=subprocess.DEVNULL, **kwargs
    )


def snapshot_path(cache: Path, task: Task) -> Path:
    return cache / "snapshots" / task.id


def mirror_path(cache: Path, task: Task) -> Path:
    return cache / "mirrors" / (task.source.repo.replace("/", "__") + ".git")


def make_snapshot(cache: Path, task: Task) -> Path:
    """The task's snapshot, created on first use."""
    snapshot = snapshot_path(cache, task)
    if (snapshot / ".git").is_dir():
        return snapshot
    mirror = mirror_path(cache, task)
    if not mirror.is_dir():
        run(["git", "init", "-q", "--bare", str(mirror)])
    commit = task.source.commit
    has_commit = (
        subprocess.run(
            ["git", "-C", str(mirror), "cat-file", "-e", f"{commit}^{{commit}}"], capture_output=True
        ).returncode
        == 0
    )
    if not has_commit:
        run(["git", "-C", str(mirror), "fetch", "-q", "--depth", "1", task.source.url, commit])
    partial = snapshot.with_name(snapshot.name + ".partial")
    shutil.rmtree(partial, ignore_errors=True)
    partial.mkdir(parents=True)
    archive = subprocess.Popen(["git", "-C", str(mirror), "archive", commit], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(partial)], stdin=archive.stdout, check=True)
    if archive.wait() != 0:
        raise RuntimeError(f"git archive failed for {task.id}")
    run(["git", "init", "-q"], cwd=partial)
    run(["git", "add", "-A"], cwd=partial)
    run(["git", *GIT_IDENTITY, "commit", "-q", "--no-verify", "-m", "base"], cwd=partial)
    partial.rename(snapshot)
    return snapshot


def fill_localization_gold(cache: Path, task: Task, lemma: Path) -> None:
    """Names the definitions the fix changes, from the snapshot's syntax index."""
    snapshot = snapshot_path(cache, task)
    db = cache / "gold" / f"{task.id}.db"
    db.parent.mkdir(parents=True, exist_ok=True)
    run([str(lemma), "--no-daemon", "--db", str(db), "index", str(snapshot)])
    symbols: set[str] = set()
    for change in parse_diff(task.gold["patch"]):
        if is_test_path(change.path):
            continue
        outline = json.loads(
            run([str(lemma), "--no-daemon", "--db", str(db), "--json", "outline", str(snapshot / change.path)]).stdout
        )
        names = changed_definitions(change, definitions_from_outline(outline))
        symbols.update(f"{change.path}::{name}" for name in names)
    task.gold["symbols"] = sorted(symbols)


def prepare(cache: Path, tasks: list[Task], lemma: Path) -> list[str]:
    """Snapshots every task and fills in missing answer keys. Returns the ids
    of tasks that could not be prepared, with why."""
    failures = []
    for task in tasks:
        try:
            make_snapshot(cache, task)
            if task.track == LOCALIZATION and "symbols" not in task.gold:
                fill_localization_gold(cache, task, lemma)
        except (subprocess.CalledProcessError, RuntimeError) as error:
            detail = getattr(error, "stderr", "") or str(error)
            failures.append(f"{task.id}: {detail.strip()[:300]}")
    return failures
