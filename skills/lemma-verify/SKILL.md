---
name: lemma-verify
description: Prove a change keeps repositories in agreement with `lemma check` - callers against the operations they call and their schemas, clients against the specs they were generated from, and what the change adds wired up - then fix what it reports at the root. Use at the end of each unit of a change to an API operation, route, schema or generated client, before calling the change done, or when reviewing a branch.
---

# Verify a change

`lemma check --change <repository>` reads the repository at the commit its branch left the default branch and as it is now, uncommitted edits included, reads the other repositories as they are, and reports what the change breaks in them and what it adds without wiring up. Only what the change introduced counts; what was already wrong is listed as informational.

## When

- At the end of each unit: one contract changed together with its callers. Not after every edit: a half-made change reports breaks you are about to fix.
- Before calling the change done, for every repository you edited.
- When reviewing a branch someone else wrote.

## Steps

1. From the workspace directory (whose `mod.lm` composes the repositories), run `lemma check --change <repository>` for each repository you edited, or the `check` tool with `change`. For committed work on a branch use `--base <commit>`; the default is where the branch left the default branch.
2. Read every introduced finding:
   - `error`: a request fails, such as a caller sending a parameter the operation now rejects, or a client still declaring a removed one. The command exits with 4.
   - `warning`: behavior changes without failing, such as a response field a caller reads is gone, or an operation was added that no route serves or no code calls.
   - Each names the caller or client and its `file:line`, and the requirement in brackets.
3. Fix each at its root: the caller, the client, or the contract itself if the break was not meant. A generated client is fixed where it is generated from when that is in reach, otherwise by hand to match.
4. Check again. Repeat until nothing you cannot explain is introduced.
5. Before handing over, `lemma check --change <repository> --level strict` fails when an adapter could not read the code, so a pass is not resting on documents alone.
6. Report each run and its result. A warning you leave in place needs a reason, such as an operation meant for callers outside the workspace (state it in a graph of `lemma.http.public_operation` so the check expects no caller).

## With sentry (experimental)

When the daemon's sentry is attached to the workspace (`lemma daemon sentry status` says so; `lemma daemon sentry start` attaches it), a check is answered from what earlier checks read: a check after an edit takes about a second, and one with nothing changed is instant. Checking at the end of each unit costs little then, but the rule stands: a half-made change still reports breaks you are about to fix.

Each answer ends with `sentry token: <token>`. After fixing what a check reported, `lemma check --change <repository> --since <token>` (or `since` on the `check` tool) lists only the findings that came and went, so you can see a fix land without reading the whole report again. The same token on a new answer means nothing it reads has changed.

## When a finding looks wrong

Run with `--all` to see informational findings, and `lemma verify --links` to see which caller Lemma matched to which operation. A wrong finding is usually one wrong match: a path built at run time matched to the wrong operation, or a common field name counted as a use. Say so in the report rather than working around it in code.

## The stop gate (experimental)

The stop gate is opt-in. `hooks/stop-check.py` beside this file, a Claude Code Stop hook, runs `lemma check --change` for each repository the agent changed when it tries to stop, and sends it back to work once when the change introduced errors or warnings. It is off unless a project turns it on in `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": ".claude/skills/lemma-verify/hooks/stop-check.py", "timeout": 300 }] }
    ]
  }
}
```

The path above is for skills copied into the project. Skills installed with `LEMMA_SKILLS=1` are in `~/.claude/skills`: name `stop-check.py` there instead.

### What sends the agent back

`LEMMA_GATE_LEVEL` in the hook's environment says which introduced findings send the agent back:

| `LEMMA_GATE_LEVEL` | Sends the agent back for | Use it when |
| --- | --- | --- |
| `warning` (the default) | Introduced errors and introduced warnings | An agent is at work: it can fix a warning or say why it is a false alarm, and that costs one turn |
| `error` | Introduced errors only | Warnings in this system are mostly noise, and the extra turn is not worth it |

Warnings count by default because one of them is often a real break: `consumer_uses_removed_response_field`, a response field the change removed that the calling code still reads, is a warning, since a common name such as `id` can be the caller's own field. Read off the answer and passed on, it fails at run time. The gate asks the agent to fix each finding or say why it is expected.

Set the level in the command, or in the `env` of `settings.json`:

```json
{ "type": "command", "command": "LEMMA_GATE_LEVEL=error .claude/skills/lemma-verify/hooks/stop-check.py", "timeout": 300 }
```

A level the gate does not know is not guessed at: the gate uses the default and tells the agent to have the setting corrected. Informational findings never send an agent back, and neither does a finding that was already there before the change.

### Where it looks, and what it says when it cannot

- It finds the workspace from the directory the agent runs in: the nearest directory at or above it whose `mod.lm` declares a `workspace`. An agent started inside one of its repositories is checked from there too.
- An agent working in a repository that sits beside the workspace (`local("../billing")`) cannot find it by looking up. Name the workspace with `LEMMA_WORKSPACE` in the hook's environment: `LEMMA_WORKSPACE=/path/to/workspace .claude/skills/lemma-verify/hooks/stop-check.py`.
- It reads the repositories from that module, wherever their directories are, Git worktrees included.
- It checks each repository the agent changed: one with uncommitted edits, or with commits that are not on its default branch, so work an orchestrator commits before the agent stops is checked.
- A repository with a module of its own and no workspace above it is checked alone. A directory with no Lemma module anywhere above it is left alone: the gate has nothing to hold the agent to.
- When `lemma check` cannot run or does not finish, the gate says so and sends the agent back to run it by hand, rather than letting it pass.
- It blocks at most once per stop, so an error the agent cannot fix is reported rather than looped on.
