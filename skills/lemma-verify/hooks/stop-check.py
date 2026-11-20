#!/usr/bin/env python3
"""Claude Code Stop hook: before the agent finishes, check each of the
workspace's repositories that has uncommitted edits with `lemma check
--change`, and send the agent back once when the change introduced errors,
or when a check could not run. Opt-in; see ../SKILL.md.

Set LEMMA_GATE_LEVEL=warning to send the agent back for introduced
warnings too."""
import json
import os
import re
import subprocess
import sys

# A repository of the workspace, as its module names it: `repo web = local("../web")`.
REPOSITORY = re.compile(r'\brepo\s+([A-Za-z_]\w*)\s*=\s*local\(\s*"([^"]+)"\s*\)')
TIMEOUT_SECONDS = 240


def repositories(root):
    """The workspace's repositories with their directories, read from its
    module rather than from the directories beside it: a repository may sit
    inside the workspace, next to it, or be a Git worktree."""
    with open(os.path.join(root, "mod.lm"), encoding="utf-8") as module:
        named = REPOSITORY.findall(module.read())
    return [(name, os.path.normpath(os.path.join(root, path))) for name, path in named]


def git(directory, *arguments):
    return subprocess.run(["git", "-C", directory, *arguments], capture_output=True, text=True)


def is_edited(directory):
    """Whether `directory` is a repository's own checkout with uncommitted
    changes. A plain directory inside another repository is not one."""
    top = git(directory, "rev-parse", "--show-toplevel")
    if top.returncode != 0 or os.path.realpath(top.stdout.strip()) != os.path.realpath(directory):
        return False
    status = git(directory, "status", "--porcelain")
    return status.returncode == 0 and status.stdout.strip() != ""


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
    root = hook.get("cwd") or os.getcwd()
    if not os.path.exists(os.path.join(root, "mod.lm")):
        return
    blocking = {"error", "warning"} if os.environ.get("LEMMA_GATE_LEVEL") == "warning" else {"error"}
    found = []
    unchecked = []
    for name, directory in repositories(root):
        if not is_edited(directory):
            continue
        findings, problem = check(root, name)
        if findings is None:
            unchecked.append((name, problem))
            continue
        for finding in findings:
            if finding.get("severity") in blocking and finding.get("introduced"):
                found.append((name, finding.get("check", ""), finding.get("message", "")))
    if not found and not unchecked:
        return
    parts = []
    if found:
        lines = [f"- [{kind}] {message}" for _, kind, message in found[:10]]
        if len(found) > 10:
            lines.append(f"- and {len(found) - 10} more")
        checked = ", ".join(sorted({name for name, _, _ in found}))
        parts.append(f"`lemma check --change` found problems your edits introduced in {checked}:\n"
                     + "\n".join(lines)
                     + "\nFix them, or say why each is expected, before finishing. "
                     "Run `lemma check --change <repository>` to see them again.")
    for name, problem in unchecked:
        parts.append(f"The stop gate could not check your edits in {name}: {problem}. "
                     f"Run `lemma check --change {name}` yourself and report what it says before finishing.")
    print(json.dumps({"decision": "block", "reason": "\n".join(parts)}))


main()
