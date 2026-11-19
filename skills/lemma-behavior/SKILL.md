---
name: lemma-behavior
description: Prove a runtime claim by running the real code, and keep the proof - scaffold a Lemma behavior with `lm new behavior`, whose checker runs an operation's handler (optionally replacing one outbound call), fill in the case, and run it with `lemma check`. Use when a change's safety rests on what code does rather than on its shape, such as passing an error or flag through, retrying, or a default value, and a type or contract check cannot show it.
---

# Prove it by running it

A contract check shows that shapes agree. Whether a handler passes an upstream error's `retriable` flag on, retries, or keeps a default needs the code to run. A behavior is that run kept as a requirement: its checker runs the handler, and every later `lemma check` runs it again when what its run rests on changes: a file its run read changes where Lemma can watch runs (Linux with `strace`), otherwise any file in a repository it reads. Behaviors are beta: their syntax and the checker protocol may change.

## Steps

1. **State the claim as one observable fact.** For example: "when the functions service answers with an error, the worker's response carries the same `retriable` flag". If you cannot say what you would observe, you cannot prove it.
2. **Scaffold it** from the module that declares the operation (a `mod.lm`):

   ```sh
   lm new behavior passes_on_retriable --handler worker.setup_monitor \
     --through "worker.call_setup_webhook -> functions.setup_webhook"
   ```

   `--handler` is the operation to run, as the module names it. `--through` is a link the handler calls through, written as the module's graph writes it; for Python the script replaces that call, so a case can choose what the other side answers. It writes a checker and a behavior into the module, and a script under `tools/` in the handler's language: complete for Python; for TypeScript (run with Bun) one that runs the handler and leaves replacing the call to you; for Go a placeholder that says which test to write, and prints what to fill in.
3. **Fill in the TODOs**: the input each case sends, what the replaced call answers, and the `expect` lines stating the fact from step 1. Keep one fact per behavior.
4. **Run it**: `lm check` for the module, then `lemma check --select passes_on_retriable` from the workspace. A case's outcome is satisfied, violated, unknown (the checker could not run) or stale.
5. **Make it fail once.** Break the code the claim is about, or flip an `expect`, and see the case turn violated, then put it back. A behavior that cannot fail proves nothing.
6. **Keep it.** Commit the behavior with the change. Later checks reuse its evidence until what its run rests on changes, then run it again: a file the run read where Lemma can watch runs (Linux with `strace`), otherwise any file in a repository it reads.

## What to hand back

The claim, the behavior's name and file, each case's outcome, and the run where it failed when it should have.
