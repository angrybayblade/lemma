"""The answer key for localization: the files and definitions a fix changes.

It follows SWE-PolyBench's node-level key: functions, methods and classes
(Go structs and interfaces) whose code the fix changes or adds. Where the
dataset ships that key (`modified_nodes`), it is used as is; otherwise it
is computed here the same way.

Definitions are named the way `lemma outline` names them (qualified by
their containers: `Router.delete`; Go methods by receiver: `Server.Start`).
Agents in every arm are told this convention, so the key favours no arm.
"""

from __future__ import annotations

from dataclasses import dataclass

from .patches import FileChange

TEST_MARKERS = ("test", "tests", "__tests__", "spec", "testdata", "e2e")
NODE_KINDS = {"function", "method", "class", "struct", "interface"}
"""Kinds of definition the key counts; constants, variables and type aliases are not nodes."""


@dataclass(frozen=True)
class Definition:
    """A definition's qualified name and the lines it spans (from 1, inclusive)."""

    qualified_name: str
    first_line: int
    last_line: int
    kind: str = "function"

    def contains(self, line: int) -> bool:
        return self.first_line <= line <= self.last_line


def is_test_path(path: str) -> bool:
    """Test files are not part of the answer: fixes are scored on source."""
    parts = path.lower().replace("\\", "/").split("/")
    name = parts[-1]
    if any(part in TEST_MARKERS for part in parts[:-1]):
        return True
    return name.startswith("test_") or any(marker in name for marker in ("_test.", ".test.", ".spec.", "_spec."))


def changed_definitions(change: FileChange, definitions: list[Definition]) -> set[str]:
    """Qualified names of the innermost definitions each changed line falls in.

    Inserted code counts only when it lands inside a definition, not between
    two of them: adding a new function next to an old one does not change
    the old one.
    """
    names: set[str] = set()
    for line in change.changed_lines:
        if innermost := innermost_containing(definitions, line, line):
            names.add(innermost.qualified_name)
    for after in change.insertions:
        if innermost := innermost_containing(definitions, after, after + 1):
            names.add(innermost.qualified_name)
    return names


def innermost_containing(definitions: list[Definition], first: int, last: int) -> Definition | None:
    """The smallest definition spanning both lines, if any."""
    containing = [d for d in definitions if d.contains(first) and d.contains(last)]
    if not containing:
        return None
    return min(containing, key=lambda d: d.last_line - d.first_line)


def definitions_from_outline(outline: dict) -> list[Definition]:
    """Node definitions from `lemma --json outline` (zero-based positions)."""
    return [
        Definition(
            qualified_name=symbol["qualified_name"],
            first_line=symbol["span"]["start"]["line"] + 1,
            last_line=symbol["span"]["end"]["line"] + 1,
            kind=symbol["kind"],
        )
        for symbol in outline["symbols"]
        if symbol["kind"] in NODE_KINDS
    ]


def added_definitions(before: list[Definition], after: list[Definition]) -> set[str]:
    """Definitions the fix adds: in the patched file but not the original."""
    return {d.qualified_name for d in after} - {d.qualified_name for d in before}


def from_polybench(modified_nodes: list[str]) -> set[str]:
    """SWE-PolyBench nodes (`path->program->class_declaration:A->method_definition:b`)
    as `path::A.b`."""
    symbols = set()
    for node in modified_nodes:
        path, *parts = node.split("->")
        names = [part.split(":", 1)[1] for part in parts if ":" in part]
        if names:
            symbols.add(f"{path}::{'.'.join(names)}")
    return symbols
