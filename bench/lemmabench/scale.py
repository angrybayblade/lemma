"""How Lemma's checks scale: a generated system of any size, and timings of
`lemma check` on it.

python3 -m lemmabench.scale generate --out DIR --consumer-files 100000 [--services 4] [--seed 7]
python3 -m lemmabench.scale run --workspace DIR --lemma BIN [--repeats 2]

`generate` writes services, each an OpenAPI document of operations with
request bodies, and consumer repositories in TypeScript and Python whose
files import their neighbours and call those operations. Every repository
is a Git repository; the first service's second commit makes one operation
require a new field. The same seed writes the same system.

`run` checks that change with `lemma check --change`, cold (an empty cache)
and warm, and reports wall time, peak memory and the cache's size. It
fails if Lemma does not flag exactly the functions that call the changed
operation, which the generator records: a fast answer that misses callers
is not a result.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

FILES_PER_DIRECTORY = 50
FILES_PER_CONSUMER = 5_000
FUNCTIONS_PER_FILE = 3
OPERATIONS_PER_SERVICE = 60
FIELDS = [f"field_{index}" for index in range(5)]
ADDED_FIELD = "added_field"
METHODS = ["get", "post", "patch", "delete"]


@dataclass(frozen=True)
class Operation:
    service: str
    method: str
    path: str
    operation_id: str

    @property
    def has_body(self) -> bool:
        return self.method in ("post", "patch")


def operations_of(service: str) -> list[Operation]:
    """A service's operations; the first is a POST, the one the change
    makes require a new field."""
    found = []
    for index in range(OPERATIONS_PER_SERVICE):
        method = "post" if index == 0 else METHODS[index % len(METHODS)]
        path = f"/api/{service}/resource{index % 10}/{{id}}/action{index}"
        found.append(Operation(service, method, path, f"{service}_action{index}"))
    return found


def document(operations: list[Operation], with_added_field: bool) -> dict:
    """An OpenAPI document for `operations`; the first operation also
    requires `added_field` when `with_added_field`."""
    paths: dict = {}
    for position, operation in enumerate(operations):
        entry: dict = {
            "operationId": operation.operation_id,
            "parameters": [{"name": "id", "in": "path", "required": True, "schema": {"type": "string"}}],
            "responses": {"200": {"description": "ok"}},
        }
        if operation.has_body:
            required = ["field_0"] + ([ADDED_FIELD] if with_added_field and position == 0 else [])
            properties = {field: {"type": "string"} for field in FIELDS}
            if with_added_field and position == 0:
                properties[ADDED_FIELD] = {"type": "string"}
            entry["requestBody"] = {
                "required": True,
                "content": {
                    "application/json": {
                        "schema": {"type": "object", "properties": properties, "required": required}
                    }
                },
            }
        paths.setdefault(operation.path, {})[operation.method] = entry
    return {"openapi": "3.0.0", "info": {"title": "generated", "version": "1"}, "paths": paths}


def concrete(path: str) -> str:
    """A path template as a TypeScript template literal fills it."""
    return path.replace("{id}", "${id}")


def typescript_file(
    name: str, neighbours: list[str], calls: list[Operation], rng: random.Random
) -> tuple[str, list[str]]:
    """A TypeScript module: imports of its neighbours, and functions each
    calling one operation. The names of functions calling the first
    service's first operation are returned with it."""
    lines = [f"import {{ helper_{neighbour} }} from './{neighbour}'" for neighbour in neighbours]
    callers = []
    for index, operation in enumerate(calls):
        function = f"{name}_call{index}"
        body = ""
        if operation.has_body:
            fields = ", ".join(f"{field}: value" for field in FIELDS[: rng.randint(1, 3)])
            body = f", body: JSON.stringify({{ {fields} }})"
        path = concrete(operation.path)
        lines += [
            f"export async function {function}(id: string, value: string) {{",
            f"  const response = await fetch(`{path}`, {{ method: '{operation.method.upper()}'{body} }})",
            "  return response.json()",
            "}",
        ]
        if operation.operation_id.endswith("_action0") and operation.service == "svc0":
            callers.append(function)
    lines += [f"export function helper_{name}(value: string) {{", "  return value.trim()", "}"]
    return "\n".join(lines) + "\n", callers


def python_file(
    name: str, neighbours: list[str], calls: list[Operation], rng: random.Random
) -> tuple[str, list[str]]:
    """A Python module, as `typescript_file` writes a TypeScript one."""
    lines = ["import requests", ""]
    lines += [f"from .{neighbour} import helper_{neighbour}" for neighbour in neighbours]
    callers = []
    for index, operation in enumerate(calls):
        function = f"{name}_call{index}"
        keyword = ""
        if operation.has_body:
            fields = ", ".join(f'"{field}": value' for field in FIELDS[: rng.randint(1, 3)])
            keyword = f", json={{{fields}}}"
        path = operation.path.replace("{id}", "{identifier}")
        lines += [
            "",
            f"def {function}(identifier, value):",
            f'    response = requests.{operation.method}(f"https://api.example.com{path}"{keyword})',
            "    return response.json()",
        ]
        if operation.operation_id.endswith("_action0") and operation.service == "svc0":
            callers.append(function)
    lines += ["", f"def helper_{name}(value):", "    return value.strip()", ""]
    return "\n".join(lines), callers


def git(directory: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-c", "user.email=bench@lemma", "-c", "user.name=bench", *args],
        cwd=directory,
        check=True,
        capture_output=True,
    )


def generate(out: Path, consumer_files: int, services: int, seed: int) -> dict:
    """Writes the system under `out`; what it wrote, including the callers
    of the changed operation as `consumer:file::function`."""
    rng = random.Random(seed)
    names = [f"svc{index}" for index in range(services)]
    every_operation = [operation for name in names for operation in operations_of(name)]
    changed = every_operation[0]
    for name in names:
        repository = out / "services" / name
        repository.mkdir(parents=True)
        (repository / "mod.lm").write_text(
            f'module {name}\n\nimport * as http from lemma.http\n\n'
            'export service api {\n    operations: http.openapi("openapi.json")\n}\n'
        )
        (repository / "openapi.json").write_text(json.dumps(document(operations_of(name), False), indent=1))
        git(repository, "init", "-q")
        git(repository, "add", "-A")
        git(repository, "commit", "-qm", "operations")
    consumers = max(1, -(-consumer_files // FILES_PER_CONSUMER))
    callers: list[str] = []
    written = 0
    for consumer in range(consumers):
        repository = out / "consumers" / f"app{consumer}"
        count = min(FILES_PER_CONSUMER, consumer_files - written)
        for position in range(count):
            language = "ts" if position % 5 < 3 else "py"
            directory = repository / "src" / f"module{position // FILES_PER_DIRECTORY}"
            directory.mkdir(parents=True, exist_ok=True)
            first = (position // FILES_PER_DIRECTORY) * FILES_PER_DIRECTORY
            last = min(first + FILES_PER_DIRECTORY, count) - 1
            siblings = [f"file{other}" for other in range(first, last + 1) if other != position]
            siblings = [sibling for sibling in siblings if (int(sibling[4:]) % 5 < 3) == (language == "ts")]
            neighbours = rng.sample(siblings, min(2, len(siblings)))
            calls = [rng.choice(every_operation) for _ in range(FUNCTIONS_PER_FILE)]
            if rng.random() < 0.02:
                calls[0] = changed
            name = f"file{position}"
            writer = typescript_file if language == "ts" else python_file
            text, found = writer(name, neighbours, calls, rng)
            relative = f"src/module{position // FILES_PER_DIRECTORY}/{name}.{language}"
            (repository / relative).write_text(text)
            callers += [f"app{consumer}:{relative}::{function}" for function in found]
        if (repository / "src").exists():
            for directory in (repository / "src").iterdir():
                (directory / "__init__.py").write_text("")
        (repository / "mod.lm").write_text(
            f'module app{consumer}\n\nexport service web {{\n    index: {{ include: ["src"] }}\n}}\n'
        )
        git(repository, "init", "-q")
        git(repository, "add", "-A")
        git(repository, "commit", "-qm", "code")
        written += count
    changed_repository = out / "services" / "svc0"
    (changed_repository / "openapi.json").write_text(
        json.dumps(document(operations_of("svc0"), True), indent=1)
    )
    git(changed_repository, "commit", "-qam", "require added_field")
    lines = ["module scale", "", "workspace {", '    language: "0.2"']
    lines += [f'    repo {name} = local("services/{name}")' for name in names]
    lines += [f'    repo app{index} = local("consumers/app{index}")' for index in range(consumers)]
    lines += ["    config: { level: check }", "}", ""]
    (out / "mod.lm").write_text("\n".join(lines))
    summary = {
        "consumer_files": consumer_files,
        "services": services,
        "operations": len(every_operation),
        "consumers": consumers,
        "changed": f"{changed.method.upper()} {changed.path}",
        "callers": sorted(callers),
    }
    (out / "generated.json").write_text(json.dumps(summary, indent=1))
    return summary


def peak_memory_megabytes(stderr: str) -> float | None:
    """Peak memory from `/usr/bin/time -l` (bytes on macOS) or `-v`
    (kilobytes on Linux)."""
    found = re.search(r"(\d+)\s+maximum resident set size", stderr)
    if found:
        return int(found.group(1)) / 1_048_576
    found = re.search(r"Maximum resident set size \(kbytes\): (\d+)", stderr)
    return int(found.group(1)) / 1024 if found else None


def flagged_callers(report: dict) -> list[str]:
    """`consumer:file::function` of each caller flagged for missing the new
    field, from links like `#app0:web:src/a.ts::f -> POST /x`."""
    found = set()
    for finding in report["findings"]:
        if finding["check"] != "consumer_misses_new_required_field":
            continue
        caller = finding["link"].split(" -> ")[0]
        consumer, _, rest = caller.lstrip("#").partition(":")
        found.add(f"{consumer}:{rest.partition(':')[2]}")
    return sorted(found)


def check_once(workspace: Path, lemma: Path, cache: Path) -> tuple[float, float | None, dict]:
    timer = ["/usr/bin/time", "-l"] if sys.platform == "darwin" else ["/usr/bin/time", "-v"]
    command = [str(lemma), "check", str(workspace / "mod.lm"), "--no-daemon", "--json",
               "--change", "#svc0", "--base", "HEAD~1", "--head", "HEAD"]
    started = time.monotonic()
    result = subprocess.run(timer + command, capture_output=True, text=True,
                            env={**os.environ, "XDG_CACHE_HOME": str(cache), "CI": "true"})
    elapsed = time.monotonic() - started
    if not result.stdout.startswith("{"):
        raise SystemExit(f"lemma check failed:\n{result.stderr[-2000:]}")
    return elapsed, peak_memory_megabytes(result.stderr), json.loads(result.stdout)


def directory_megabytes(path: Path) -> float:
    return sum(file.stat().st_size for file in path.rglob("*") if file.is_file()) / 1_048_576


def run(workspace: Path, lemma: Path, repeats: int) -> int:
    expected = json.loads((workspace / "generated.json").read_text())
    cache = Path(tempfile.mkdtemp(prefix="lemma-scale-cache-"))
    try:
        for attempt in range(repeats):
            elapsed, memory, report = check_once(workspace, lemma, cache)
            flagged = flagged_callers(report)
            if flagged != expected["callers"]:
                missing = sorted(set(expected["callers"]) - set(flagged))
                extra = sorted(set(flagged) - set(expected["callers"]))
                print(f"WRONG: {len(missing)} callers missed, {len(extra)} flagged wrongly", file=sys.stderr)
                print(f"  missed: {missing[:5]}\n  extra: {extra[:5]}", file=sys.stderr)
                return 1
            label = "cold" if attempt == 0 else "warm"
            print(f"{label}: {elapsed:.1f} s, peak {memory or 0:.0f} MB, {len(flagged)} callers flagged (all correct)")
        print(f"files {expected['consumer_files']}, cache {directory_megabytes(cache):.0f} MB")
    finally:
        shutil.rmtree(cache, ignore_errors=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(prog="lemmabench.scale")
    commands = parser.add_subparsers(dest="command", required=True)
    generating = commands.add_parser("generate", help="write a system of a given size")
    generating.add_argument("--out", type=Path, required=True)
    generating.add_argument("--consumer-files", type=int, required=True)
    generating.add_argument("--services", type=int, default=4)
    generating.add_argument("--seed", type=int, default=7)
    running = commands.add_parser("run", help="time lemma check on a generated system")
    running.add_argument("--workspace", type=Path, required=True)
    running.add_argument("--lemma", type=Path, required=True)
    running.add_argument("--repeats", type=int, default=2)
    arguments = parser.parse_args()
    if arguments.command == "generate":
        if arguments.out.exists():
            raise SystemExit(f"{arguments.out} exists; choose a new directory")
        summary = generate(arguments.out, arguments.consumer_files, arguments.services, arguments.seed)
        print(f"wrote {summary['consumer_files']} files in {summary['consumers']} consumers, "
              f"{summary['operations']} operations; {len(summary['callers'])} call {summary['changed']}")
        return 0
    return run(arguments.workspace, arguments.lemma, arguments.repeats)


if __name__ == "__main__":
    sys.exit(main())
