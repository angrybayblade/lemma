"""What agents are told, and the shape their final answer must take.

Every arm gets the same prompt. Tools differ, instructions do not.
"""

from __future__ import annotations

from .tasks import LOCALIZATION, Task

NAMING = (
    "Name definitions by qualified name: enclosing definitions joined with dots, such as "
    "`Router.delete` for the method `delete` of class `Router`, `Server.Start` for a Go method "
    "`Start` with receiver `*Server`, and plain `helper` for a top-level function. Paths are "
    "relative to the repository root."
)

LOCALIZATION_INSTRUCTIONS = f"""You are in a checkout of {{repo}}. Read the issue below and find \
where the code must change to resolve it. Do not edit files and do not write a fix: only locate.

Answer with the source files that must change (not tests) and the functions, methods or classes \
in them whose code must change. Prefer a short, precise answer over a long list of guesses. \
{NAMING}

<issue>
{{brief}}
</issue>"""

IMPACT_INSTRUCTIONS = """You are in a checkout of {repo}. Do not edit files.

{brief}

Answer with each location as a path relative to the repository root and a line number counting \
from 1."""

LOCALIZATION_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["files", "symbols"],
    "properties": {
        "files": {"type": "array", "items": {"type": "string"}},
        "symbols": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "name"],
                "properties": {"path": {"type": "string"}, "name": {"type": "string"}},
            },
        },
    },
}

IMPACT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["locations"],
    "properties": {
        "locations": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "line"],
                "properties": {"path": {"type": "string"}, "line": {"type": "integer"}},
            },
        }
    },
}


def prompt_for(task: Task) -> str:
    template = LOCALIZATION_INSTRUCTIONS if task.track == LOCALIZATION else IMPACT_INSTRUCTIONS
    return template.format(repo=task.source.repo, brief=task.brief)


def schema_for(task: Task) -> dict:
    return LOCALIZATION_SCHEMA if task.track == LOCALIZATION else IMPACT_SCHEMA
