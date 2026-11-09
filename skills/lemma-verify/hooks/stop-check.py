#!/usr/bin/env python3
"""Claude Code Stop hook: before the agent finishes, check each repository
with uncommitted edits with `lemma check --change`, and send the agent
back once when the change introduced errors. Opt-in; see ../SKILL.md."""
import json
import os
import subprocess
import sys

hook = json.load(sys.stdin)
# Already sent back once for this stop: report, don't loop.
if hook.get("stop_hook_active"):
    sys.exit(0)
root = hook.get("cwd") or os.getcwd()
if not os.path.exists(os.path.join(root, "mod.lm")):
    sys.exit(0)


def edited(directory):
    status = subprocess.run(["git", "-C", directory, "status", "--porcelain"], capture_output=True, text=True)
    return status.returncode == 0 and status.stdout.strip() != ""


repositories = sorted(
    name for name in os.listdir(root)
    if os.path.isdir(os.path.join(root, name, ".git")) and edited(os.path.join(root, name))
)
errors = []
for name in repositories:
    try:
        run = subprocess.run(["lemma", "check", "--change", name, "--json"], cwd=root,
                             capture_output=True, text=True, timeout=240)
    except subprocess.TimeoutExpired:
        continue
    try:
        report = json.loads(run.stdout)
    except json.JSONDecodeError:
        continue
    for finding in report.get("findings", []):
        if finding.get("severity") == "error" and finding.get("introduced"):
            errors.append((name, finding.get("check", ""), finding.get("message", "")))
if not errors:
    sys.exit(0)
lines = [f"- [{check}] {message}" for _, check, message in errors[:10]]
if len(errors) > 10:
    lines.append(f"- and {len(errors) - 10} more")
checked = ", ".join(sorted({name for name, _, _ in errors}))
reason = (
    f"`lemma check --change` found errors your edits introduced in {checked}:\n"
    + "\n".join(lines)
    + "\nFix them, or say why each is expected, before finishing. "
    "Run `lemma check --change <repository>` to see them again."
)
print(json.dumps({"decision": "block", "reason": reason}))
