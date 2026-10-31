"""Choosing tasks: public issues for localization, round-3 symbols for impact.

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
from .tasks import IMPACT, LOCALIZATION, Source, Task

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


def true_references(result: dict, symbol: dict) -> set[tuple[str, int]] | None:
    """Lines both oracles confirmed as references (only one oracle for Go),
    zero-based. `None` if some candidates went unjudged or were sampled."""
    key = str(symbol["id"])
    data = result["data"][key]
    if data["sampled"]:
        return None
    position = [s["id"] for s in result["symbols"]].index(symbol["id"])
    oracles = [result["verdicts"][name]["verdicts"][position] for name in ("A", "B") if name in result["verdicts"]]
    confirmed: set[tuple[str, int]] = set()
    for index, candidate in enumerate(data["checked"]):
        if index == 0:
            continue  # the definition itself
        verdicts = [oracle[index] for oracle in oracles]
        if any(verdict is None for verdict in verdicts):
            return None
        if all(verdicts):
            confirmed.add((candidate["path"], candidate["line"]))
    return confirmed


def select_impact(
    results_dir: Path, corpora: dict[str, Source], per_corpus: int, min_references: int, max_references: int, seed: int
) -> list[Task]:
    """Symbols from round 3 whose references were fully checked, with a
    useful number of them."""
    random.seed(seed)
    tasks: list[Task] = []
    for corpus, source in sorted(corpora.items()):
        candidates = []
        for path in sorted(results_dir.glob(f"{corpus}-*.json")):
            result = json.loads(path.read_text())
            by_id = {symbol["id"]: symbol for symbol in result["symbols"]}
            for symbol in (by_id[symbol_id] for symbol_id in result["final"]):
                truth = true_references(result, symbol)
                if truth and min_references <= len(truth) <= max_references:
                    candidates.append((result["language"], symbol, truth))
        candidates.sort(key=lambda item: item[1]["id"])
        for language, symbol, truth in random.sample(candidates, min(per_corpus, len(candidates))):
            tasks.append(impact_task(corpus, source, language, symbol, truth))
    return tasks


def impact_task(corpus: str, source: Source, language: str, symbol: dict, truth: set[tuple[str, int]]) -> Task:
    line = symbol["line"] + 1
    brief = (
        f"The {symbol['kind']} `{symbol['qualified_name']}` is defined at "
        f"{symbol['path']}:{line}. Its signature is about to change in a way that breaks "
        "every caller. Find every place in this repository's code that refers to it: calls, "
        "imports, and references to it as a value or type. Leave out the definition itself, "
        "other symbols that share its name, and mentions in comments or strings."
    )
    return Task(
        id=f"imp-{corpus}-{symbol['id']}",
        track=IMPACT,
        language=language,
        source=source,
        brief=brief,
        gold={"locations": sorted(f"{path}:{line + 1}" for path, line in truth)},
        origin={"experiment": "round3", "symbol": symbol},
    )
