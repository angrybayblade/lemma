"""What an agent did, from `codex exec --json` events: tools used and tokens spent."""

from __future__ import annotations

import json
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class Activity:
    input_tokens: int = 0
    cached_input_tokens: int = 0
    output_tokens: int = 0
    reasoning_tokens: int = 0
    turns: int = 0
    commands: int = 0
    mcp_calls: int = 0
    tools: Counter = field(default_factory=Counter)
    """Calls by tool: `mcp:<server>.<tool>`, or the first word of a shell command."""
    failed_mcp_calls: int = 0

    @property
    def tool_calls(self) -> int:
        return self.commands + self.mcp_calls

    @property
    def uncached_input_tokens(self) -> int:
        return self.input_tokens - self.cached_input_tokens


def read_activity(path: Path) -> Activity:
    """Sums a run's events. Unknown event types are ignored, so newer Codex
    versions that add events still parse."""
    activity = Activity()
    try:
        lines = path.read_text().splitlines()
    except OSError:
        return activity
    for line in lines:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        record(activity, event)
    return activity


def record(activity: Activity, event: dict) -> None:
    kind = event.get("type", "")
    if kind == "turn.completed":
        activity.turns += 1
        usage = event.get("usage") or {}
        activity.input_tokens += usage.get("input_tokens", 0)
        activity.cached_input_tokens += usage.get("cached_input_tokens", 0)
        activity.output_tokens += usage.get("output_tokens", 0)
        activity.reasoning_tokens += usage.get("reasoning_output_tokens", 0)
        return
    if kind != "item.completed":
        return
    item = event.get("item") or {}
    item_type = item.get("type") or item.get("item_type")
    if item_type == "command_execution":
        activity.commands += 1
        activity.tools[command_name(item.get("command", ""))] += 1
    elif item_type == "mcp_tool_call":
        activity.mcp_calls += 1
        activity.tools[f"mcp:{item.get('server', '?')}.{item.get('tool', '?')}"] += 1
        if item.get("status") == "failed" or item.get("error"):
            activity.failed_mcp_calls += 1


WRAPPERS = {"bash", "sh", "zsh", "-lc", "-c", "env", "&&", ";"}


def command_name(command: str | list) -> str:
    """The program a shell command runs, looking past `bash -lc '...'` and
    a leading `cd <dir> &&`."""
    if isinstance(command, list):
        command = " ".join(command)
    words = [word.rsplit("/", 1)[-1] for word in command.replace("'", " ").replace('"', " ").split()]
    while words:
        if words[0] == "cd":
            words = words[2:]
        elif words[0] in WRAPPERS:
            words = words[1:]
        else:
            return words[0]
    return "?"
