# Tutorials

Complete the [quickstart](../quickstart.md) first. Each tutorial starts from its end state: a `shop` directory with two repositories, `billing` and `web`, and a change to billing's API that breaks web. Take the tutorials in any order; each builds on the same `shop`.

| Tutorial | What you do | Use it when |
| --- | --- | --- |
| [1. Declare a link](1-links.md) | Say which operation calls which, and check the caller's types against the provider's schemas. | Inference cannot see a call, or you need field-by-field type checks. |
| [2. Write your own requirement](2-requirements.md) | State facts, write a rule over them, and choose what runs with `check main`. | The standard library does not cover a property of your system. |
| [3. Allow a known finding](3-allowances.md) | Accept a finding with a reason, and make it count again after a date. | A finding is known and accepted for a limited time. |
| [4. Test a behavior](4-behaviors.md) (beta) | Run code through a checker, find a bug, fix it, and reuse the evidence. | A property depends on what code does, not on a schema. |
| [5. Add a feature](5-features.md) | Add an operation, see what it lacks, wire it up, and mark operations called from outside. | A change adds to an API and must be served and called. |
| [6. Set up your editor](6-editors.md) | Get highlighting, errors as you type, go-to-definition and formatting, and add `lm` and `lemma` to CI. | You write `.lm` modules. |

The first five are tested: Lemma's test suite runs every command they show, with the files they show, and compares the output, so what you see is what Lemma prints.

To find references and definitions with language servers, follow [Getting started](../getting-started.md).
