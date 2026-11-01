"""Choosing tasks: public issues for localization, common-named symbols for impact.

Selection is seeded, so the same inputs always give the same tasks.
"""

from __future__ import annotations

import json
import random
import urllib.parse
import urllib.request
from pathlib import Path

from .gold import is_test_path
from .patches import parse_diff
from .prepare import make_snapshot
from .tasks import IMPACT, LOCALIZATION, Source, Task
from .truth import ORACLES, Checked, Criteria, check_symbols, index_snapshot

ROWS_API = "https://datasets-server.huggingface.co/rows"
POLYBENCH = "AmazonScience/SWE-PolyBench_500"
MULTILINGUAL = "SWE-bench/SWE-bench_Multilingual"
EXTENSIONS = {
    "python": (".py",),
    "typescript": (".ts", ".tsx", ".mts", ".cts"),
    "go": (".go",),
}
MAX_FILES = 3
MAX_BRIEF_CHARS = 6000


def fetch_rows(dataset: str) -> list[dict]:
    """Every row of a Hugging Face dataset's test split."""
    rows: list[dict] = []
    while True:
        query = urllib.parse.urlencode(
            {"dataset": dataset, "config": "default", "split": "test", "offset": len(rows), "length": 100}
        )
        with urllib.request.urlopen(f"{ROWS_API}?{query}", timeout=60) as response:
            page = json.load(response)
        rows.extend(item["row"] for item in page["rows"])
        if len(rows) >= page["num_rows_total"] or not page["rows"]:
            return rows


def language_of_patch(patch: str) -> str | None:
    """The one supported language every changed source file is in, if any."""
    languages = set()
    for change in parse_diff(patch):
        if is_test_path(change.path):
            continue
        matching = [lang for lang, exts in EXTENSIONS.items() if change.path.endswith(exts)]
        if not matching:
            return None
        languages.add(matching[0])
    return languages.pop() if len(languages) == 1 else None


def is_usable_issue(row: dict) -> bool:
    """Small, self-contained fixes to existing source files."""
    changes = [c for c in parse_diff(row["patch"]) if not is_test_path(c.path)]
    if not 1 <= len(changes) <= MAX_FILES:
        return False
    if any(c.is_new or c.is_deleted for c in changes):
        return False
    return 0 < len(row["problem_statement"]) <= MAX_BRIEF_CHARS


def localization_task(row: dict, dataset: str, language: str) -> Task:
    repo = row["repo"]
    files = sorted({c.path for c in parse_diff(row["patch"]) if not is_test_path(c.path)})
    return Task(
        id=f"loc-{row['instance_id']}",
        track=LOCALIZATION,
        language=language,
        source=Source(repo=repo, commit=row["base_commit"], url=f"https://github.com/{repo}.git"),
        brief=row["problem_statement"].strip(),
        # Symbols are filled in by `prepare`, which has the code to read.
        gold={"files": files, "patch": row["patch"]},
        origin={"dataset": dataset, "instance_id": row["instance_id"], "modified_nodes": row.get("modified_nodes")},
    )


def localization_candidates(seed: int) -> dict[str, list[Task]]:
    """Usable issues by language, in a seeded random order with one issue
    per repository first: Python and TypeScript from SWE-PolyBench, Go from
    SWE-bench Multilingual. Take tasks from the front until enough prepare
    cleanly."""
    pools: dict[str, list[tuple[str, dict]]] = {lang: [] for lang in EXTENSIONS}
    for dataset in (POLYBENCH, MULTILINGUAL):
        wanted = {"python", "typescript"} if dataset == POLYBENCH else {"go"}
        for row in fetch_rows(dataset):
            language = language_of_patch(row["patch"])
            if language in wanted and is_usable_issue(row):
                pools[language].append((dataset, row))
    random.seed(seed)
    candidates: dict[str, list[Task]] = {}
    for language, pool in pools.items():
        pool.sort(key=lambda item: item[1]["instance_id"])
        random.shuffle(pool)
        seen_repos: set[str] = set()
        first, rest = [], []
        for dataset, row in pool:
            target = rest if row["repo"] in seen_repos else first
            seen_repos.add(row["repo"])
            target.append(localization_task(row, dataset, language))
        candidates[language] = first + rest
    return candidates


def impact_tasks(
    cache: Path, lemma: Path, corpora: dict[str, tuple[Source, str]], criteria: Criteria, seed: int
) -> list[Task]:
    """Impact tasks for each corpus (source and language), with answer keys
    checked by the oracle servers."""
    tasks: list[Task] = []
    for corpus, (source, language) in sorted(corpora.items()):
        placeholder = Task(id=f"imp-{corpus}", track=IMPACT, language=language, source=source, brief="")
        root = make_snapshot(cache, placeholder)
        db = cache / "truth" / f"{corpus}.db"
        index_snapshot(lemma, db, root)
        for checked in check_symbols(root, db, language, criteria, seed):
            tasks.append(impact_task(corpus, source, language, checked))
    return tasks


def impact_task(corpus: str, source: Source, language: str, checked: Checked) -> Task:
    symbol = checked.symbol
    line = symbol["line"] + 1
    brief = (
        f"The {symbol['kind']} `{symbol['qualified_name']}` is defined at "
        f"{symbol['path']}:{line}. Its signature is about to change in a way that breaks "
        "every caller. Find every place in this repository's code that refers to it: calls, "
        "imports, and references to it as a value or type. Leave out the definition itself, "
        "other symbols that share its name, and mentions in comments or strings."
    )
    return Task(
        id=f"imp-{corpus}-{symbol['name']}-{line}",
        track=IMPACT,
        language=language,
        source=source,
        brief=brief,
        gold={
            "locations": [f"{path}:{number}" for path, number in checked.references],
            "unknown": [f"{path}:{number}" for path, number in checked.unknown],
        },
        origin={
            "oracles": [server["command"][0] for server in ORACLES[language]["servers"]],
            "name_occurrences": checked.occurrences,
            "symbol": symbol,
        },
    )
