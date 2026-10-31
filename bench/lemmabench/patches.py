"""Reading unified diffs: which files a fix touches and which old lines it changes."""

from __future__ import annotations

import re
from dataclasses import dataclass, field

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


@dataclass
class FileChange:
    """One file in a diff, by its path before the change."""

    path: str
    is_new: bool = False
    is_deleted: bool = False
    changed_lines: set[int] = field(default_factory=set)
    """Old-side lines (from 1) that were removed or replaced."""
    insertions: set[int] = field(default_factory=set)
    """Old-side lines (from 1) after which new lines were inserted; 0 means
    at the top. Code inserted between two definitions belongs to neither."""


def parse_diff(diff: str) -> list[FileChange]:
    """The files a unified diff changes, in diff order."""
    changes: list[FileChange] = []
    current: FileChange | None = None
    in_hunk = False
    old_line = 0
    for line in diff.splitlines():
        if line.startswith("diff --git "):
            current = FileChange(path=line.split(" b/", 1)[-1])
            changes.append(current)
            in_hunk = False
            continue
        if current is None:
            continue
        if not in_hunk:
            read_header(current, line)
        if match := HUNK.match(line):
            in_hunk = True
            start, count = int(match.group(1)), match.group(2)
            # `-10,0` means nothing is removed and new lines follow line 10.
            old_line = start + 1 if count == "0" else start
        elif not in_hunk:
            continue
        elif line.startswith("-"):
            current.changed_lines.add(old_line)
            old_line += 1
        elif line.startswith("+"):
            current.insertions.add(old_line - 1)
        elif line.startswith(" "):
            old_line += 1
    return changes


def read_header(change: FileChange, line: str) -> None:
    if line.startswith("new file mode"):
        change.is_new = True
    elif line.startswith("deleted file mode"):
        change.is_deleted = True
    elif line.startswith("--- a/"):
        change.path = line[len("--- a/") :]
