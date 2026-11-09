---
name: lemma-mode
description: Verification-driven engineering with Lemma for a change other code depends on - an API operation's parameters or fields, a route, a schema, a shared function, a rename. Restates the goal, sizes the work, maps what the change reaches, works in units that each end in a check, and reports what was proven and how. Use for /lemma-mode, or when asked to change a contract carefully across repositories.
disable-model-invocation: true
---

# Lemma mode

A change is done when it is proven, not when it compiles or reads right. This skill is the workflow. The other skills hold the how: **lemma-usage** for finding code and reading findings, **lemma-impact** for mapping what a change reaches, **lemma-verify** for checking it, **lemma-behavior** for running it.

## Every change

1. **Say the goal first.** In your first message, in your own words: what the user wants, the contract that changes (operation, field, symbol), and the smallest change that meets it. Don't wait for confirmation.
2. **Size it, and say which path you took.** Choose below. If the change turns out larger than the path you chose, move up.
3. **Find code with Lemma.** Text search is for strings, configuration and documentation, and for what Lemma says it could not see.
4. **Know what the change reaches before the first edit.** Each caller and declaration, with `file:line`, from Lemma's answers.
5. **Check at the end of each unit, not after every edit.** A unit is one contract changed together with its callers. A half-made change reports breaks you are about to fix.
6. **Report what is proven and how**, using the ladder below, and name what you could not prove.

## Choose a path

**Light.** One field, parameter or symbol, callers in at most one other repository, and no change in what callers must send or may read. Skip the written plan:

1. `references` on what you are changing, or `graph` with `direction: callers` from a handler.
2. Edit it and its callers.
3. **lemma-verify** for each repository you edited.
4. Report in a few lines.

**Full.** Anything else, or when unsure. Copy the steps of the playbook that fits into your todo list and work them in order.

## Playbooks

### Change a contract

An operation's parameters, request or response fields, path, or required fields.

1. Map the impact with **lemma-impact**. That list is your todo list for the edit.
2. Change the provider: the route's schema and the committed spec, together.
3. Update each caller on the list, and each client that declares the contract. Tick each off.
4. Verify with **lemma-verify** for every repository you edited. Fix, then check again.
5. Report.

### Add a feature

A new operation, field or route that callers will use.

1. Name the data shape: the request and response the feature adds.
2. Find where it attaches: the route module, the spec, the client, the first caller.
3. Add the route, its schema and the spec entry together.
4. Add the caller.
5. Verify with **lemma-verify**: no warning about what the change adds that you cannot explain.
6. Report.

### Rename or refactor a shared symbol

1. `references` for the symbol, and `implementations` for an interface member. Write the list.
2. Change the definition and every use on the list.
3. `references` on the new name finds them all, and on the old name finds none.
4. If the symbol is an operation's handler or schema, verify with **lemma-verify**.
5. Report.

### Map the impact (read-only)

Use **lemma-impact**. Answer with `file:line` and the rung each fact reached.

### Review a diff

1. **lemma-verify** on the branch: read every introduced finding.
2. For each changed operation or shared symbol, `graph` with `direction: callers` to see who it reaches.
3. Report what breaks, what is proven safe and how, and what is unproven.

## How sure are you

Get each fact the change's safety rests on as far down this list as is cheap, and say where it stopped.

1. **You said so.** Worth nothing on its own.
2. **You pointed at it.** A `file:line` from `references` or `definition`: complete for what the language server sees, unlike a text search.
3. **You traced it.** `graph` with `direction: callers`, run from the workspace, followed the operation to its callers in the other repositories.
4. **You checked it.** `lemma check --change <repository>` reports nothing introduced.
5. **You ran it.** A behavior whose checker runs the real handler passed (**lemma-behavior**).

## Writing the reply

Lead with the goal and whether it is met. Then what changed, each item with its `file:line`. Then the proof: each `lemma check` run and its result, and any behavior run. End with what is unproven, if anything.
