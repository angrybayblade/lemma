#!/usr/bin/env python3
"""Claude Code Stop hook: before the agent finishes, check each repository
of its workspace that it changed with `lemma check --change`, and send the
agent back once when the change introduced errors or warnings, or when a
check could not run. Opt-in; see ../SKILL.md.

A repository counts as changed when it has uncommitted edits or commits
that are not on its default branch, so work an orchestrator commits before
the agent stops is checked too.

LEMMA_GATE_LEVEL says what sends the agent back: `warning` (the default)
for introduced errors and warnings, `error` for errors only.
LEMMA_WORKSPACE names the workspace's directory when the agent runs
somewhere the gate cannot find it from, such as a repository that sits
beside the workspace rather than inside it."""
import json
import os
import re
import subprocess
import sys

MODULE = "mod.lm"
# A repository of the workspace, as its module names it: `repo web = local("../web")`.
REPOSITORY = re.compile(r'\brepo\s+([A-Za-z_]\w*)\s*=\s*local\(\s*"([^"]+)"\s*\)')
DECLARES_WORKSPACE = re.compile(r"^\s*workspace\s*\{", re.MULTILINE)
# The branches Lemma takes for a repository's default branch, in its order
# (`default_branches` in crates/lemma-graph/src/repos.rs): keep them in step.
DEFAULT_BRANCHES = ["main", "master", "develop", "trunk"]
TIMEOUT_SECONDS = 240
# What sends the agent back at each level of LEMMA_GATE_LEVEL.
LEVELS = {"warning": {"error", "warning"}, "error": {"error"}}
DEFAULT_LEVEL = "warning"


def git(directory, *arguments):
    return subprocess.run(["git", "-C", directory, *arguments], capture_output=True, text=True)


def lines_of(directory, *arguments):
    done = git(directory, *arguments)
    return [line.strip() for line in done.stdout.splitlines() if line.strip()] if done.returncode == 0 else []


def read_module(directory):
    try:
        with open(os.path.join(directory, MODULE), encoding="utf-8") as module:
            return module.read()
    except OSError:
        return None


def is_checkout(directory):
    """Whether `directory` is a repository's own checkout, not a directory
    inside another repository's."""
    top = git(directory, "rev-parse", "--show-toplevel")
    return top.returncode == 0 and os.path.realpath(top.stdout.strip()) == os.path.realpath(directory)


def workspace_of(place):
    """Where to check from, and the repositories to check: (root, [(name,
    directory)]). The workspace is the one LEMMA_WORKSPACE names, else the
    nearest directory at or above `place` whose module declares a
    workspace, else the nearest repository with a module of its own, which
    is checked alone. `None` where no Lemma module is anywhere above."""
    named = os.environ.get("LEMMA_WORKSPACE")
    candidates = [os.path.abspath(named)] if named else [place, *parents(place)]
    for directory in candidates:
        text = read_module(directory)
        if text is not None and DECLARES_WORKSPACE.search(text):
            members = [(name, os.path.normpath(os.path.join(directory, path)))
                       for name, path in REPOSITORY.findall(text)]
            own = [("self", directory)] if is_checkout(directory) else []
            return directory, own + members
    for directory in [place, *parents(place)]:
        if read_module(directory) is not None and is_checkout(directory):
            return directory, [("self", directory)]
    return None


def parents(place):
    found = []
    directory = os.path.abspath(place)
    while os.path.dirname(directory) != directory:
        directory = os.path.dirname(directory)
        found.append(directory)
    return found


def fork_point(directory):
    """Where the checked-out commit left the default branch, as Lemma finds
    it (`fork_point` in crates/lemma-graph/src/repos.rs): the merge base
    with the first default branch that exists."""
    remotes = sorted(lines_of(directory, "remote"), key=lambda remote: remote != "origin")
    configured = lines_of(directory, "config", "--get", "init.defaultBranch")
    names = DEFAULT_BRANCHES + configured
    candidates = lines_of(directory, "for-each-ref", "--format=%(symref)", "refs/remotes/*/HEAD")
    candidates += [f"{remote}/{name}" for remote in remotes for name in names] + names
    for candidate in candidates:
        if git(directory, "rev-parse", "--verify", "--quiet", f"{candidate}^{{commit}}").returncode != 0:
            continue
        base = git(directory, "merge-base", "HEAD", candidate)
        if base.returncode == 0:
            return base.stdout.strip()
    return None


def is_changed(directory, members_inside):
    """Whether the repository at `directory` has uncommitted edits, leaving
    out the workspace's own repositories kept inside it, or commits since
    it left its default branch."""
    if not is_checkout(directory):
        return False
    status = lines_of(directory, "status", "--porcelain")
    edited = [line for line in status if not any(line[3:].rstrip("/") == member for member in members_inside)]
    if edited:
        return True
    head = git(directory, "rev-parse", "HEAD").stdout.strip()
    base = fork_point(directory)
    return base is not None and base != head


def check(root, name):
    """The findings of `lemma check --change name`, or why there are none to read."""
    try:
        run = subprocess.run(["lemma", "check", "--change", name, "--json"], cwd=root,
                             capture_output=True, text=True, timeout=TIMEOUT_SECONDS)
    except FileNotFoundError:
        return None, "`lemma` is not on PATH"
    except subprocess.TimeoutExpired:
        return None, f"the check did not finish in {TIMEOUT_SECONDS} seconds"
    try:
        return json.loads(run.stdout).get("findings", []), None
    except json.JSONDecodeError:
        said = (run.stderr.strip() or run.stdout.strip()).splitlines()
        return None, said[-1][:300] if said else f"`lemma check` exited with {run.returncode}"


def main():
    hook = json.load(sys.stdin)
    # Already sent back once for this stop: report, don't loop.
    if hook.get("stop_hook_active"):
        return
    found_workspace = workspace_of(hook.get("cwd") or os.getcwd())
    if found_workspace is None:
        return
    root, repositories = found_workspace
    inside = [os.path.relpath(directory, root) for _, directory in repositories
              if os.path.realpath(directory).startswith(os.path.realpath(root) + os.sep)]
    level = os.environ.get("LEMMA_GATE_LEVEL") or DEFAULT_LEVEL
    blocking = LEVELS.get(level, LEVELS[DEFAULT_LEVEL])
    found = []
    unchecked = []
    for name, directory in repositories:
        if not is_changed(directory, inside if name == "self" else []):
            continue
        findings, problem = check(root, name)
        if findings is None:
            unchecked.append((name, problem))
            continue
        for finding in findings:
            if finding.get("severity") in blocking and finding.get("introduced"):
                found.append((name, finding["severity"], finding.get("check", ""), finding.get("message", "")))
    parts = []
    if level not in LEVELS:
        # Said rather than guessed at: a mistyped level must not quietly loosen the gate.
        parts.append(f"LEMMA_GATE_LEVEL is `{level}`; it must be `warning` or `error`. "
                     f"The stop gate used `{DEFAULT_LEVEL}`. Tell the user to correct the hook's settings.")
    if not found and not unchecked and not parts:
        return
    if found:
        lines = [f"- {severity} [{kind}] {message}" for _, severity, kind, message in found[:10]]
        if len(found) > 10:
            lines.append(f"- and {len(found) - 10} more")
        checked = ", ".join(sorted({name for name, _, _, _ in found}))
        parts.append(f"`lemma check --change` found problems your changes introduced in {checked}:\n"
                     + "\n".join(lines)
                     + "\nFix them, or say why each is expected, before finishing: a warning can be "
                     "a false alarm, such as a common field name the code uses for something else. "
                     f"Run `lemma check --change <repository>` from {root} to see them again.")
    for name, problem in unchecked:
        parts.append(f"The stop gate could not check your changes in {name}: {problem}. "
                     f"Run `lemma check --change {name}` from {root} yourself and report what it says "
                     "before finishing.")
    print(json.dumps({"decision": "block", "reason": "\n".join(parts)}))


main()
