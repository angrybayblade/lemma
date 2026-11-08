---
name: lemma-behavior
description: Prove a runtime claim by running the real code, and keep the proof - scaffold a Lemma behavior with `lm new behavior`, whose checker runs an operation's handler (optionally replacing one outbound call), fill in the case, and run it with `lemma check`. Use when a change's safety rests on what code does rather than on its shape, such as passing an error or flag through, retrying, or a default value, and a type or contract check cannot show it.
---

# Prove it by running it

A contract check shows that shapes agree. Whether a handler passes an upstream error's `retriable` flag on, retries, or keeps a default needs the code to run. A behavior is that run kept as a requirement: its checker runs the handler, and every later `lemma check` runs it again when the code it reads changes.

## Steps

1. **State the claim as one observable fact.** For example: "when the functions service answers with an error, the worker's response carries the same `retriable` flag". If you cannot say what you would observe, you cannot prove it.
2. **Scaffold it** from the module that declares the operation (a `mod.lm`):

   ```sh
   lm new behavior passes_on_retriable --handler worker.setup_monitor \
     --through "worker.call_setup_webhook -> functions.setup_webhook"
   ```

   `--handler` is the operation to run, as the module names it. `--through` is a link the handler calls through, written as the module's graph writes it; the script replaces that call so a case can choose what the other side answers. It writes a checker and a behavior into the module, and a script under `tools/` in the handler's language (Python, or TypeScript run with Bun; a stub for Go), and prints what to fill in.
3. **Fill in the TODOs**: the input each case sends, what the replaced call answers, and the `expect` lines stating the fact from step 1. Keep one fact per behavior.
4. **Run it**: `lm check` for the module, then `lemma check --select passes_on_retriable` from the workspace. A case's outcome is satisfied, violated, unknown (the checker could not run) or stale.
5. **Make it fail once.** Break the code the claim is about, or flip an `expect`, and see the case turn violated, then put it back. A behavior that cannot fail proves nothing.
6. **Keep it.** Commit the behavior with the change. Later checks reuse its evidence until the code it reads changes, then run it again.

## What to hand back

The claim, the behavior's name and file, each case's outcome, and the run where it failed when it should have.
