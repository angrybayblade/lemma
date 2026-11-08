---
name: lemma-mode
description: Verification-driven engineering with Lemma for a change other code depends on - an API operation's parameters or fields, a route, a schema, a shared function, a rename. Restates the goal, maps every caller with Lemma before editing, works in units that each end in a check, and proves the change with `lemma check` before calling it done. Use for /lemma-mode, or when asked to change a contract carefully across repositories.
disable-model-invocation: true
---

# Lemma mode

A change is done when it is proven, not when it compiles or reads right. Lemma gives the proofs a text search cannot: every use of a symbol through the language's own server, every caller of an operation in every repository, and a check that the repositories still agree after the edit.

## Non-negotiables

1. **Say the goal first.** In your first message, in your own words: what the user wants, the contract that changes (operation, field, symbol), and the smallest change that meets it. Don't wait for confirmation.
2. **Pick a playbook below and copy its steps into your todo list word for word.** Work them in order.
3. **Find code with Lemma.** `search_symbols`, `outline`, `references`, `definition`, `implementations` and `graph` (or `lemma symbols`, `refs`, `graph` on the command line). Use text search only for strings, configuration and documentation, and for what Lemma says it could not see.
4. **List the impact before the first edit.** Every caller and every place the contract is declared, each with `file:line`, from Lemma's answers. That list is your todo list for the edit.
5. **Check at the end of each unit, not after every edit.** A unit is one contract changed together with its callers. Run `lemma check --change <repository>` for each repository you edited (the **lemma-verify** skill). Fix what it reports at the root and check again.
6. **Report what is proven and how.** Your final reply says, for each claim, which rung of the ladder below it reached and the command that showed it, and names anything you could not prove.

## How sure are you

Get each fact the change's safety rests on as far down this list as is cheap, and say where it stopped.

1. **You said so.** Worth nothing on its own.
2. **You pointed at it.** A `file:line` from `references` or `definition`: complete for what the language server sees, unlike a text search.
3. **You traced it.** `graph` with `direction: callers` from the handler, run from the workspace, followed the operation to its callers in the other repositories.
4. **You checked it.** `lemma check --change <repository>` reports nothing introduced: callers agree with the operations they call, and what you added is wired up.
5. **You ran it.** A behavior whose checker runs the real handler passed (the **lemma-behavior** skill).

## Playbooks

### Change a contract

An operation's parameters, request or response fields, path, or required fields.

1. Find the operation: its route handler and schema (`search_symbols`, `outline` of the route file) and its entry in each committed spec.
2. Find every caller: `graph` with `direction: callers` and `workspace` from the handler, `references` on generated client methods for the operation, and `lemma check --links` for the links Lemma infers. Write the impact list.
3. Change the provider: the route's schema and the committed spec, together.
4. Update each caller on the impact list, and each client that declares the contract. Tick each off.
5. Verify with **lemma-verify** for every repository you edited. Fix, then check again.
6. Report, with the ladder.

### Add a feature

A new operation, field or route that callers will use.

1. Name the data shape: the request and response the feature adds.
2. Find where it attaches: the route module, the spec, the client, the first caller (`search_symbols`, `graph`).
3. Add the route, its schema and the spec entry together.
4. Add the caller.
5. Verify with **lemma-verify**: no `operation_without_route`, `route_without_operation` or `operation_without_consumer` warning you cannot explain.
6. Report.

### Rename or refactor a shared symbol

1. `references` for the symbol (and `implementations` for an interface member). Write the list.
2. Change the definition and every use on the list.
3. `references` on the new name finds them all, and on the old name finds none.
4. If the symbol is an operation's handler or schema, verify with **lemma-verify**.
5. Report.

### Map the impact of a change (read-only)

Use the **lemma-impact** skill. Answer with `file:line` and the rung each fact reached.

### Review a diff

1. `lemma check --change <repository>` on the branch: read every introduced finding (**lemma-verify**).
2. For each changed operation or shared symbol, `graph` with `direction: callers` to see who it reaches.
3. Report what breaks, what is proven safe and how, and what is unproven.

## Writing the reply

Lead with the goal and whether it is met. Then the impact list as changed, each item with its `file:line`. Then the proof: each `lemma check` run and its result, and any behavior run. End with what is unproven, if anything.
