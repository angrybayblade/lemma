"""Answer keys for impact tasks, checked by independent language servers.

For a symbol, every whole-word occurrence of its name is a candidate. Each
oracle server is asked for the definition at each candidate; a candidate is
a reference when every oracle points at the symbol's name. Python and
TypeScript use two servers that share no code (pyright and jedi; tsserver
and the TypeScript 7 native server); Go has only gopls. Symbols with any
unanswered candidate are dropped rather than half-checked.

The oracles are deliberately not the servers Lemma prefers (ty, and the
native TypeScript server comes second for Lemma), and Lemma is not used.
"""

from __future__ import annotations

import json
import random
import sqlite3
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

from .gold import is_test_path
from .prepare import run

ORACLES = {
    "python": {
        "lsp_id": "python",
        "rg_type": "py",
        "servers": [
            {
                "command": ["pyright-langserver", "--stdio"],
                "ready": ["Found * source file*", "No source files found*"],
                "settle": 10,
            },
            {"command": ["jedi-language-server"], "ready": [], "settle": 10},
        ],
    },
    "typescript": {
        "lsp_id": "typescript",
        "rg_type": "ts",
        "servers": [
            {
                "command": ["typescript-language-server", "--stdio"],
                "ready": ["Initializing JS/TS language features*"],
                "settle": 10,
            },
            {
                "command": [str(Path.home() / "ts7/node_modules/.bin/tsc"), "--lsp", "--stdio"],
                "ready": [],
                "settle": 15,
            },
        ],
    },
    "go": {"lsp_id": "go", "rg_type": "go", "servers": [{"command": ["gopls"], "ready": [], "settle": 10}]},
}
KINDS = ("function", "method", "class", "struct", "interface")
MIN_NAME_LENGTH = 4


@dataclass(frozen=True)
class Criteria:
    """Which symbols make useful impact tasks."""

    count: int = 3
    min_occurrences: int = 51
    """Whole-word occurrences of the name; common names are where text search goes wrong."""
    max_occurrences: int = 500
    min_references: int = 5
    max_references: int = 150
    attempts: int = 24
    """Symbols to check before giving up on finding `count`."""


@dataclass
class Checked:
    symbol: dict
    references: list[tuple[str, int]]
    """(path, line from 1), the definition excluded."""
    occurrences: int
    unknown: list[tuple[str, int]] = field(default_factory=list)
    """Lines no oracle rejected but some could not resolve past an import;
    answering them is neither required nor penalized."""


def candidate_symbols(db: Path, language: str, seed: int) -> list[dict]:
    """Definitions in the index, outside tests, in a seeded random order."""
    languages = ("typescript", "tsx") if language == "typescript" else (language,)
    connection = sqlite3.connect(db)
    # Interface methods are left out: whether a call through an
    # implementation "refers to" the interface method is a matter of
    # definition, and the oracles and servers disagree on it.
    rows = connection.execute(
        "SELECT s.id, s.name, s.qualified_name, s.kind, f.path, s.name_start_line, s.name_start_column "
        "FROM symbols s JOIN files f ON f.id = s.file_id "
        f"WHERE f.language IN ({','.join('?' * len(languages))}) "
        f"AND s.kind IN ({','.join('?' * len(KINDS))}) "
        "AND NOT EXISTS (SELECT 1 FROM symbols c WHERE c.file_id = s.file_id AND c.kind = 'interface' "
        "AND c.id != s.id AND (c.start_line, c.start_column) <= (s.start_line, s.start_column) "
        "AND (c.end_line, c.end_column) >= (s.end_line, s.end_column)) "
        "ORDER BY s.id",
        (*languages, *KINDS),
    ).fetchall()
    connection.close()
    keys = ("id", "name", "qualified_name", "kind", "path", "line", "column")
    symbols = [dict(zip(keys, row, strict=True)) for row in rows]
    symbols = [s for s in symbols if len(s["name"]) >= MIN_NAME_LENGTH and not is_test_path(s["path"])]
    random.Random(seed).shuffle(symbols)
    return symbols


def occurrences(root: Path, language: str, name: str) -> list[dict]:
    """Every whole-word occurrence of a name, zero-based, columns in bytes."""
    output = subprocess.run(
        ["rg", "--vimgrep", "-w", "-F", "-t", ORACLES[language]["rg_type"], "--", name, "."],
        cwd=root,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
    ).stdout
    found = []
    for line in output.splitlines():
        path, line_number, column, _ = line.split(":", 3)
        path = path[2:] if path.startswith("./") else path
        found.append({"path": path, "line": int(line_number) - 1, "column": int(column) - 1})
    return found


def ask_oracles(root: Path, language: str, jobs: list[dict]) -> list[list[list[bool | None]]]:
    """Verdicts per server, per job, per candidate."""
    results = []
    oracle = Path(__file__).with_name("oracle.py")
    for server in ORACLES[language]["servers"]:
        spec = {
            "root": str(root),
            "command": server["command"],
            "language_id": ORACLES[language]["lsp_id"],
            "ready": server["ready"],
            "settle_seconds": server["settle"],
            "jobs": jobs,
        }
        completed = subprocess.run(
            [sys.executable, str(oracle)], input=json.dumps(spec), capture_output=True, text=True, check=True
        )
        results.append(json.loads(completed.stdout)["verdicts"])
    return results


def check_symbols(root: Path, db: Path, language: str, criteria: Criteria, seed: int) -> list[Checked]:
    """Symbols with common names and a fully checked set of references."""
    picked = []
    for symbol in candidate_symbols(db, language, seed):
        if len(picked) == criteria.attempts:
            break
        found = occurrences(root, language, symbol["name"])
        if criteria.min_occurrences <= len(found) <= criteria.max_occurrences:
            picked.append((symbol, found))
    if not picked:
        return []
    jobs = [
        {"target": {"path": s["path"], "line": s["line"], "column": s["column"]}, "candidates": found}
        for s, found in picked
    ]
    verdicts = ask_oracles(root, language, jobs)
    checked = []
    for index, (symbol, found) in enumerate(picked):
        per_server = [server[index] for server in verdicts]
        result = combine(symbol, found, per_server)
        if result and criteria.min_references <= len(result.references) <= criteria.max_references:
            checked.append(result)
    return checked[: criteria.count]


def combine(symbol: dict, found: list[dict], per_server: list[list]) -> Checked | None:
    """Lines every server confirms are references; lines a server could
    only follow to an import are unknown. `None` if a server failed to
    answer for any candidate."""
    if any(verdict is None for server in per_server for verdict in server):
        return None
    definition = (symbol["path"], symbol["line"])
    references, unknown = set(), set()
    for position, candidate in enumerate(found):
        if (candidate["path"], candidate["line"]) == definition:
            continue
        verdicts = [server[position] for server in per_server]
        line = (candidate["path"], candidate["line"] + 1)
        if all(verdict is True for verdict in verdicts):
            references.add(line)
        elif False not in verdicts:
            unknown.add(line)
    # A line with a confirmed occurrence is a reference, whatever else is on it.
    return Checked(symbol, sorted(references), len(found), sorted(unknown - references))


def recheck(root: Path, language: str, symbol: dict) -> Checked | None:
    """A fresh answer key for one symbol, as `check_symbols` would build it."""
    found = occurrences(root, language, symbol["name"])
    job = {"target": {"path": symbol["path"], "line": symbol["line"], "column": symbol["column"]}, "candidates": found}
    verdicts = ask_oracles(root, language, [job])
    return combine(symbol, found, [server[0] for server in verdicts])


def index_snapshot(lemma: Path, db: Path, root: Path) -> None:
    db.parent.mkdir(parents=True, exist_ok=True)
    run([str(lemma), "--no-daemon", "--db", str(db), "index", str(root)])
