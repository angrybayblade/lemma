"""An MCP server with the tools a restricted arm may use instead of a shell.

Every restricted arm gets the same way to read code (`read_file`,
`list_files`) and at most one way to search it, so the arms differ only in
the search tool:

    python3 -m lemmabench.toolserver --search grep|ripgrep|ast-grep|none

Output is capped the same way for every tool, so no tool wins by flooding
or loses by truncation.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

MAX_LINES = 300
MAX_LINE_CHARS = 300
READ_DEFAULT_LINES = 200
AST_GREP_LANGUAGES = ["python", "typescript", "tsx", "go", "javascript"]

PATH_PROPERTY = {"type": "string", "description": "File or directory relative to the repository root; default `.`."}

READ_TOOLS = [
    {
        "name": "read_file",
        "description": f"Read lines of a file, numbered. At most {MAX_LINES} lines per call.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File relative to the repository root."},
                "start_line": {"type": "integer", "minimum": 1, "description": "First line, from 1; default 1."},
                "end_line": {"type": "integer", "minimum": 1, "description": "Last line, inclusive."},
            },
            "required": ["path"],
        },
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "list_files",
        "description": "List files tracked in the repository under a directory, optionally only those whose "
        "path contains a text.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": PATH_PROPERTY,
                "contains": {"type": "string", "description": "Only paths containing this text."},
            },
        },
        "annotations": {"readOnlyHint": True},
    },
]

TEXT_SEARCH_PROPERTIES = {
    "pattern": {"type": "string", "description": "Regular expression to search for."},
    "path": PATH_PROPERTY,
    "ignore_case": {"type": "boolean"},
    "fixed_strings": {"type": "boolean", "description": "Treat the pattern as plain text."},
    "word": {"type": "boolean", "description": "Match whole words only."},
}

SEARCH_TOOLS = {
    "grep": {
        "name": "grep",
        "description": "Search file contents with GNU grep (extended regular expressions, recursive). "
        "Returns `path:line:text`.",
        "inputSchema": {"type": "object", "properties": TEXT_SEARCH_PROPERTIES, "required": ["pattern"]},
        "annotations": {"readOnlyHint": True},
    },
    "ripgrep": {
        "name": "ripgrep",
        "description": "Search file contents with ripgrep (Rust regular expressions, respects .gitignore). "
        "Returns `path:line:text`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                **TEXT_SEARCH_PROPERTIES,
                "glob": {"type": "string", "description": "Only files matching this glob, such as `*.py`."},
            },
            "required": ["pattern"],
        },
        "annotations": {"readOnlyHint": True},
    },
    "ast-grep": {
        "name": "ast_grep",
        "description": "Search code by syntax with ast-grep: the pattern is code with metavariables, such as "
        "`$OBJ.delete($$$ARGS)` or `def $NAME($$$):`. Matches syntax nodes, not text, so comments and "
        "strings are not matched. Returns `path:line:text`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "An ast-grep pattern."},
                "language": {"type": "string", "enum": AST_GREP_LANGUAGES},
                "path": PATH_PROPERTY,
            },
            "required": ["pattern", "language"],
        },
        "annotations": {"readOnlyHint": True},
    },
}


class ToolError(Exception):
    """A mistake in a tool call, reported to the agent as a tool error."""


def cap(lines: list[str]) -> str:
    shown = [line if len(line) <= MAX_LINE_CHARS else line[:MAX_LINE_CHARS] + "…" for line in lines[:MAX_LINES]]
    if len(lines) > MAX_LINES:
        shown.append(f"… {len(lines) - MAX_LINES} more lines not shown; narrow the request")
    return "\n".join(shown) if shown else "(no matches)"


def inside(root: Path, path: str) -> Path:
    resolved = (root / (path or ".")).resolve()
    if resolved != root and root not in resolved.parents:
        raise ToolError(f"{path} is outside the repository")
    return resolved


def read_file(root: Path, arguments: dict) -> str:
    file = inside(root, arguments["path"])
    if not file.is_file():
        raise ToolError(f"{arguments['path']} is not a file")
    lines = file.read_text(errors="replace").splitlines()
    start = max(1, int(arguments.get("start_line") or 1))
    end = int(arguments.get("end_line") or start + READ_DEFAULT_LINES - 1)
    end = min(end, start + MAX_LINES - 1, len(lines))
    body = [f"{number}: {lines[number - 1]}" for number in range(start, end + 1)]
    footer = [f"(lines {start}-{end} of {len(lines)})"]
    return cap(body) + "\n" + footer[0]


def list_files(root: Path, arguments: dict) -> str:
    directory = inside(root, arguments.get("path") or ".")
    tracked = run(["git", "ls-files", "--", str(directory)], root)
    contains = arguments.get("contains") or ""
    return cap([path for path in tracked if contains in path])


def search(root: Path, kind: str, arguments: dict) -> str:
    path = str(inside(root, arguments.get("path") or "."))
    pattern = arguments["pattern"]
    if kind == "grep":
        flags = ["-rnI", "--exclude-dir=.git", "--exclude-dir=.lemma", "-E"]
        command = ["grep", *flags, *text_flags(arguments), "--", pattern, path]
    elif kind == "ripgrep":
        glob = ["--glob", arguments["glob"]] if arguments.get("glob") else []
        command = ["rg", "-n", "--no-heading", "--color=never", *glob, *text_flags(arguments), "--", pattern, path]
    else:
        command = ["ast-grep", "run", "--pattern", pattern, "--lang", arguments["language"], "--json=stream", path]
        return cap(ast_grep_lines(run(command, root), root))
    return cap([relative(line, root) for line in run(command, root)])


def text_flags(arguments: dict) -> list[str]:
    flags = []
    if arguments.get("ignore_case"):
        flags.append("-i")
    if arguments.get("fixed_strings"):
        flags.append("-F")
    if arguments.get("word"):
        flags.append("-w")
    return flags


def ast_grep_lines(output: list[str], root: Path) -> list[str]:
    lines = []
    for line in output:
        try:
            match = json.loads(line)
        except json.JSONDecodeError:
            continue
        path = Path(match["file"])
        path = path.relative_to(root) if path.is_absolute() else path
        first_line = match["lines"].splitlines()[0] if match.get("lines") else match.get("text", "")
        lines.append(f"{path}:{match['range']['start']['line'] + 1}:{first_line}")
    return lines


def relative(line: str, root: Path) -> str:
    prefix = str(root) + "/"
    return line[len(prefix) :] if line.startswith(prefix) else line


def run(command: list[str], root: Path) -> list[str]:
    try:
        completed = subprocess.run(
            command, cwd=root, capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL
        )
    except subprocess.TimeoutExpired as error:
        raise ToolError("the search took longer than 60 s; narrow it") from error
    # grep and rg exit with 1 for "no matches", which is an answer.
    if completed.returncode not in (0, 1):
        raise ToolError(completed.stderr.strip()[:500] or f"exit code {completed.returncode}")
    return completed.stdout.splitlines()


def call(root: Path, search_kind: str, name: str, arguments: dict) -> str:
    if name == "read_file":
        return read_file(root, arguments)
    if name == "list_files":
        return list_files(root, arguments)
    if search_kind != "none" and name == SEARCH_TOOLS[search_kind]["name"]:
        return search(root, search_kind, arguments)
    raise ToolError(f"unknown tool {name}")


def handle(message: dict, root: Path, search_kind: str) -> dict | None:
    if "id" not in message:
        return None
    method, params = message.get("method"), message.get("params") or {}
    if method == "initialize":
        result = {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "bench-tools", "version": "1"},
        }
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        tools = READ_TOOLS + ([SEARCH_TOOLS[search_kind]] if search_kind != "none" else [])
        result = {"tools": tools}
    elif method == "tools/call":
        try:
            text, is_error = call(root, search_kind, params.get("name"), params.get("arguments") or {}), False
        except (ToolError, KeyError, ValueError) as error:
            text, is_error = f"error: {error}", True
        result = {"content": [{"type": "text", "text": text}], "isError": is_error}
    else:
        return {"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": f"unknown {method}"}}
    return {"jsonrpc": "2.0", "id": message["id"], "result": result}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--search", choices=[*SEARCH_TOOLS, "none"], required=True)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            reply = handle(json.loads(line), root, args.search)
        except json.JSONDecodeError:
            continue
        if reply is not None:
            sys.stdout.write(json.dumps(reply) + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
