"""Running one agent on one task, in its own cgroup.

Each run gets a fresh copy of the task's snapshot, its own `CODEX_HOME`,
and its own temporary directory, so a Lemma daemon and its language
servers belong to that run alone. The run is a transient systemd user
service with CPU, memory and process limits; systemd reports what it used
and kills everything it started when it ends.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

from .prepare import snapshot_path
from .prompts import prompt_for, schema_for
from .tasks import Task

BASELINE = "baseline"


@dataclass(frozen=True)
class Arm:
    """The tools an agent gets."""

    shell: bool
    """Codex's own shell. Arms without it are read-only and cannot run code."""
    lemma: bool
    """Lemma's MCP server, with the project indexed before the agent starts."""
    search: str | None = None
    """The bench tool server: `read_file` and `list_files`, plus this search
    tool (`grep`, `ripgrep`, `ast-grep`, or `none`)."""
    guided: bool = False
    """One line of guidance to prefer Lemma, as a user would put in AGENTS.md."""


ARMS = {
    BASELINE: Arm(shell=True, lemma=False),
    "lemma": Arm(shell=True, lemma=True),
    "lemma-guided": Arm(shell=True, lemma=True, guided=True),
    "grep": Arm(shell=False, lemma=False, search="grep"),
    "ripgrep": Arm(shell=False, lemma=False, search="ripgrep"),
    "ast-grep": Arm(shell=False, lemma=False, search="ast-grep"),
    "lemma-only": Arm(shell=False, lemma=True, search="none"),
}

NO_SHELL_FEATURES = ("shell_tool", "unified_exec", "code_mode_host", "multi_agent")
"""Codex features that can run commands; arms without a shell disable all of them."""
BENCH_ROOT = Path(__file__).resolve().parent.parent

GUIDANCE = (
    "\n\nThis repository is indexed by Lemma. To find where a symbol is defined, used or implemented, "
    "prefer the lemma MCP tools (search_symbols, references, definition, implementations, outline, "
    "signature) over text search: they resolve symbols with a language server, so other symbols with "
    "the same name and mentions in comments or strings are left out."
)
"""What a user who installs Lemma would put in AGENTS.md; only the guided arm gets it."""


@dataclass(frozen=True)
class Limits:
    """Resource limits for one run's cgroup."""

    cpu_quota: str = "200%"
    memory_max: str = "8G"
    tasks_max: int = 1024
    timeout_seconds: int = 900


@dataclass(frozen=True)
class Setup:
    """What every run in a round shares."""

    cache: Path
    round_dir: Path
    lemma: Path
    """The `lemma` binary under test."""
    limits: Limits = Limits()
    tool_path: tuple[str, ...] = ()
    """Directories with language servers and search tools, put on `PATH`."""
    codex_args: tuple[str, ...] = ()
    """Extra arguments for `codex exec`, such as `-m <model>`."""
    codex: str = "codex"
    """The Codex executable; a stand-in can replace it to test the harness."""


@dataclass
class RunResult:
    task_id: str
    arm: str
    repeat: int
    exit_code: int | None
    wall_seconds: float
    index_seconds: float | None
    answer: dict | None
    resources: dict = field(default_factory=dict)
    error: str | None = None


def run_dir(setup: Setup, task: Task, arm: str, repeat: int) -> Path:
    return setup.round_dir / "runs" / task.id / f"{arm}-{repeat}"


def execute(setup: Setup, task: Task, arm: str, repeat: int) -> RunResult:
    """Runs the agent and records what happened. Never raises for a failed run."""
    directory = run_dir(setup, task, arm, repeat)
    if (directory / "result.json").is_file():
        return RunResult(**json.loads((directory / "result.json").read_text()))
    shutil.rmtree(directory, ignore_errors=True)
    workspace = lay_out(setup, task, arm, directory)
    unit = f"lemmabench-{os.getpid()}-{abs(hash((task.id, arm, repeat))) % 10**8}"
    start = time.monotonic()
    completed = subprocess.run(
        systemd_command(setup.limits, unit, directory),
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        env={**os.environ, "XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"},
    )
    wall = time.monotonic() - start
    (directory / "systemd.log").write_text(completed.stdout + completed.stderr)
    result = RunResult(
        task_id=task.id,
        arm=arm,
        repeat=repeat,
        exit_code=read_exit_code(directory),
        wall_seconds=wall,
        index_seconds=read_float(directory / "index.seconds"),
        answer=read_answer(directory / "answer.json"),
        resources={**parse_resources(completed.stderr), **read_cgroup_stats(directory)},
    )
    if result.answer is None:
        result.error = (completed.stderr[-500:] or "no answer").strip()
    shutil.rmtree(workspace, ignore_errors=True)
    (directory / "result.json").write_text(json.dumps(asdict(result), indent=2))
    return result


def lay_out(setup: Setup, task: Task, arm: str, directory: Path) -> Path:
    """Creates the run's workspace, Codex home, prompt and script."""
    workspace = directory / "workspace"
    directory.mkdir(parents=True)
    subprocess.run(["cp", "-a", "--reflink=auto", str(snapshot_path(setup.cache, task)), str(workspace)], check=True)
    codex_home = directory / "codex"
    codex_home.mkdir()
    auth = Path.home() / ".codex" / "auth.json"
    if auth.is_file():  # without it Codex fails, and the run records why
        shutil.copy2(auth, codex_home / "auth.json")
        (codex_home / "auth.json").chmod(0o600)
    (directory / "tmp").mkdir()
    path = ":".join([*setup.tool_path, "/usr/local/bin", "/usr/bin", "/bin"])
    (codex_home / "config.toml").write_text(codex_config(setup, arm, directory, path))
    guidance = GUIDANCE if ARMS[arm].guided else ""
    (directory / "prompt.txt").write_text(prompt_for(task) + guidance)
    (directory / "schema.json").write_text(json.dumps(schema_for(task)))
    script = directory / "run.sh"
    script.write_text(run_script(setup, arm, directory, path))
    script.chmod(0o755)
    return workspace


def codex_config(setup: Setup, arm: str, directory: Path, path: str) -> str:
    """Codex settings for the run: sandbox and the arm's MCP servers."""
    spec = ARMS[arm]
    sandbox = "workspace-write" if spec.shell else "read-only"
    lines = ['approval_policy = "never"', f'sandbox_mode = "{sandbox}"']
    workspace = str(directory / "workspace")
    env = {"PATH": path, "TMPDIR": str(directory / "tmp"), "HOME": str(Path.home())}
    if spec.lemma:
        lines += mcp_server("lemma", str(setup.lemma), ["mcp"], workspace, env)
    if spec.search:
        args = ["-m", "lemmabench.toolserver", "--search", spec.search, "--root", workspace]
        tool_env = {**env, "PYTHONPATH": str(BENCH_ROOT)}
        lines += mcp_server("tools", sys.executable, args, workspace, tool_env)
    return "\n".join(lines) + "\n"


def mcp_server(name: str, command: str, args: list[str], cwd: str, env: dict[str, str]) -> list[str]:
    return [
        "",
        f"[mcp_servers.{name}]",
        f"command = {json.dumps(command)}",
        f"args = {json.dumps(args)}",
        f"cwd = {json.dumps(cwd)}",
        "startup_timeout_sec = 60",
        # Runs are unattended: an approval prompt would reject the call.
        'default_tools_approval_mode = "approve"',
        "tool_timeout_sec = 300",
        "env = { " + ", ".join(f"{key} = {json.dumps(value)}" for key, value in env.items()) + " }",
    ]


def run_script(setup: Setup, arm: str, directory: Path, path: str) -> str:
    """The script run inside the cgroup."""
    quoted = shlex.quote
    workspace = directory / "workspace"
    lemma = quoted(str(setup.lemma))
    index = ""
    stop = ""
    if ARMS[arm].lemma:
        # Index before the agent starts, as a project an agent works in
        # would already be; the time is recorded separately.
        index = (
            f"start=$EPOCHREALTIME\n"
            f"{lemma} index . > {quoted(str(directory / 'index.json'))} 2>&1\n"
            f'awk "BEGIN {{ print $EPOCHREALTIME - $start }}" > {quoted(str(directory / "index.seconds"))}\n'
        )
        stop = f"{lemma} daemon stop >/dev/null 2>&1 || true\n"
    codex = " ".join(
        quoted(part)
        for part in [
            setup.codex,
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--ephemeral",
            "--output-schema",
            str(directory / "schema.json"),
            "-o",
            str(directory / "answer.json"),
            *(flag for feature in disabled_features(arm) for flag in ("--disable", feature)),
            *setup.codex_args,
            "-",
        ]
    )
    return f"""#!/bin/bash
export PATH={quoted(path)}
export HOME={quoted(str(Path.home()))}
export CODEX_HOME={quoted(str(directory / "codex"))}
export TMPDIR={quoted(str(directory / "tmp"))}
cd {quoted(str(workspace))}
{index}timeout {setup.limits.timeout_seconds} {codex} < {quoted(str(directory / "prompt.txt"))} \\
  > {quoted(str(directory / "events.jsonl"))} 2> {quoted(str(directory / "codex.stderr"))}
echo $? > {quoted(str(directory / "exit_code"))}
{stop}# systemd's own summary can miss the peak; read the cgroup while still in it.
cgroup=/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)
cat "$cgroup/memory.peak" > {quoted(str(directory / "memory.peak"))} 2>/dev/null
cat "$cgroup/cpu.stat" > {quoted(str(directory / "cpu.stat"))} 2>/dev/null
"""


def disabled_features(arm: str) -> tuple[str, ...]:
    return () if ARMS[arm].shell else NO_SHELL_FEATURES


def systemd_command(limits: Limits, unit: str, directory: Path) -> list[str]:
    return [
        "systemd-run",
        "--user",
        "--wait",
        "--collect",
        f"--unit={unit}",
        f"--property=CPUQuota={limits.cpu_quota}",
        f"--property=MemoryMax={limits.memory_max}",
        "--property=MemorySwapMax=0",
        f"--property=TasksMax={limits.tasks_max}",
        f"--property=RuntimeMaxSec={limits.timeout_seconds + 120}",
        "--",
        "/bin/bash",
        str(directory / "run.sh"),
    ]


def read_cgroup_stats(directory: Path) -> dict:
    """Peak memory and CPU time the run's cgroup recorded, if the script got
    to read them."""
    stats: dict = {}
    peak = read_float(directory / "memory.peak")
    if peak is not None:
        stats["memory_peak_bytes"] = int(peak)
    try:
        lines = (directory / "cpu.stat").read_text().splitlines()
    except OSError:
        return stats
    for line in lines:
        name, _, value = line.partition(" ")
        if name == "usage_usec":
            stats["cpu_seconds"] = int(value) / 1e6
    return stats


def read_answer(path: Path) -> dict | None:
    try:
        answer = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return answer if isinstance(answer, dict) else None


def read_exit_code(directory: Path) -> int | None:
    try:
        return int((directory / "exit_code").read_text().strip())
    except (OSError, ValueError):
        return None


def read_float(path: Path) -> float | None:
    try:
        return float(path.read_text().strip())
    except (OSError, ValueError):
        return None


DURATION_PART = re.compile(r"([\d.]+)(min|ms|us|h|s)")
UNIT_SECONDS = {"h": 3600, "min": 60, "s": 1, "ms": 1e-3, "us": 1e-6}
SIZE = re.compile(r"([\d.]+)([KMGT]?)")
SIZE_BYTES = {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}


def parse_resources(summary: str) -> dict:
    """CPU time and peak memory from `systemd-run --wait`'s summary."""
    resources: dict = {}
    for line in summary.splitlines():
        line = line.strip()
        if line.startswith("CPU time consumed:"):
            text = line.split(":", 1)[1]
            resources["cpu_seconds"] = sum(
                float(value) * UNIT_SECONDS[unit] for value, unit in DURATION_PART.findall(text)
            )
        elif line.startswith("Memory peak:"):
            if match := SIZE.search(line.split(":", 1)[1]):
                resources["memory_peak_bytes"] = int(float(match.group(1)) * SIZE_BYTES[match.group(2)])
        elif line.startswith("Finished with result:"):
            resources["result"] = line.split(":", 1)[1].strip()
    return resources
