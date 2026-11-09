# Lemma mode

Lemma mode is the workflow a coding agent follows to make a change other code depends on, and to show that the change is safe. It is a skill: type `/lemma-mode` and then the task.

```text
/lemma-mode remove the memo field from POST /v1/invoices
```

Use it for changes to an API operation's parameters or fields, a route, a schema, a shared function, or a rename. For a change nothing else depends on, you don't need it.

## Install

Lemma mode needs the `lemma` binary and the agent skills. Install both, and connect the MCP server so the agent can ask Lemma directly:

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | LEMMA_SKILLS=1 sh
claude mcp add lemma -- lemma mcp
```

The agent also needs a description of the system to check against. [Onboarding](onboarding.md) covers writing one.

The skills are written for Claude Code, which reads `~/.claude/skills` and a repository's `.claude/skills`. Other agents can follow the same text, but the `/lemma-mode` command and the Stop hook below are specific to Claude Code.

## What the agent does

1. **States the goal**: what you asked for, the contract that changes, and the smallest change that meets it.
2. **Sizes the work** and says which path it took.
3. **Maps what the change reaches** with Lemma's answers before the first edit: each caller and declaration, with `file:line`.
4. **Works in units**, one contract together with its callers, and runs `lemma check --change <repository>` at the end of each.
5. **Reports** what is proven, how, and what is not.

The agent runs a check at the end of a unit and not after every edit, because a half-made change reports breaks the agent is about to fix.

## Two paths

| Path | For | The agent |
| --- | --- | --- |
| Light | One field, parameter or symbol, callers in at most one other repository, and no change in what callers must send or may read | Finds the uses, edits them, checks, and reports in a few lines |
| Full | Anything else, or when unsure | Copies the playbook that fits into its todo list and works it in order |

The playbooks are: change a contract, add a feature, rename or refactor a shared symbol, map the impact (read-only), and review a diff.

## Reading the report

The report says how far each claim was taken. Lower on this list is stronger:

| Rung | Means |
| --- | --- |
| Said so | The agent asserted it. Worth nothing on its own |
| Pointed at it | A `file:line` from a language server: complete for what the server sees |
| Traced it | Callers followed from the operation into the other repositories |
| Checked it | `lemma check --change <repository>` reports nothing introduced |
| Ran it | A behavior that runs the real handler passed. See [Test a behavior](tutorials/4-behaviors.md) |

A claim the agent could not prove is named in the report. Read that part first.

## Enforce the check

Lemma mode asks the agent to check its work, and an agent can skip what it is asked. The opt-in Stop hook in `skills/lemma-verify/hooks/stop-check.py` runs `lemma check --change` for each repository with uncommitted edits when the agent tries to finish, and sends it back once if the change introduced errors. The `lemma-verify` skill shows how to turn it on.

## The skills it uses

Lemma mode holds the workflow and leaves the details to the other [agent skills](../skills/README.md):

| Skill | Part of the work |
| --- | --- |
| `lemma-usage` | Finding code and reading findings |
| `lemma-impact` | Mapping what a change reaches |
| `lemma-verify` | Checking the change and fixing what it reports |
| `lemma-behavior` | Proving a runtime claim by running the code |

`lemma-impact` and `lemma-verify` can also start on their own when a task calls for them. Lemma mode starts only when you ask for it.
