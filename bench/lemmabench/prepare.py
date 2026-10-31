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

from .gold import added_definitions, changed_definitions, definitions_from_outline, from_polybench, is_test_path
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
    """Names the definitions the fix changes or adds: SWE-PolyBench's own key
    when the dataset has one, otherwise computed from the patch."""
    shipped = parse_nodes(task.origin.get("modified_nodes"))
    if shipped:
        task.gold["symbols"] = sorted(from_polybench(shipped))
        task.gold["symbols_from"] = "swe-polybench"
        return
    task.gold["symbols"] = sorted(computed_symbols(cache, task, lemma))
    task.gold["symbols_from"] = "computed"


def parse_nodes(raw: object) -> list[str]:
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except json.JSONDecodeError:
            return []
    return [node for node in raw or [] if isinstance(node, str)]


def computed_symbols(cache: Path, task: Task, lemma: Path) -> set[str]:
    snapshot = snapshot_path(cache, task)
    changes = [change for change in parse_diff(task.gold["patch"]) if not is_test_path(change.path)]
    patched = patched_copy(cache, task, [change.path for change in changes])
    before_db = index(lemma, cache / "gold" / f"{task.id}.db", snapshot)
    after_db = index(lemma, cache / "gold" / f"{task.id}-patched.db", patched)
    symbols: set[str] = set()
    for change in changes:
        before = outline(lemma, before_db, snapshot, change.path)
        after = outline(lemma, after_db, patched, change.path)
        names = changed_definitions(change, before) | added_definitions(before, after)
        symbols.update(f"{change.path}::{name}" for name in names)
    return symbols


def patched_copy(cache: Path, task: Task, paths: list[str]) -> Path:
    """The changed source files, with the fix applied, in a directory of their own."""
    directory = cache / "gold" / f"{task.id}-patched"
    shutil.rmtree(directory, ignore_errors=True)
    for path in paths:
        target = directory / path
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(snapshot_path(cache, task) / path, target)
    patch = directory / "fix.patch"
    patch.write_text(task.gold["patch"])
    includes = [f"--include={path}" for path in paths]
    run(["git", "apply", *includes, str(patch)], cwd=directory)
    patch.unlink()
    return directory


def index(lemma: Path, db: Path, root: Path) -> Path:
    db.parent.mkdir(parents=True, exist_ok=True)
    run([str(lemma), "--no-daemon", "--db", str(db), "index", str(root)])
    return db


def outline(lemma: Path, db: Path, root: Path, path: str) -> list:
    result = run([str(lemma), "--no-daemon", "--db", str(db), "--json", "outline", str(root / path)])
    return definitions_from_outline(json.loads(result.stdout))


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
