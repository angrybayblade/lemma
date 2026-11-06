# Tutorials

Each tutorial starts where the [quickstart](../quickstart.md) ends: a `shop` directory with two repositories, `billing` and `web`, and a change to billing's API that breaks web. They work in any order, though they read best in this one.

| Tutorial | You will |
| --- | --- |
| [1. Declare a link](1-links.md) | Say which operation calls which, and check the caller's types against the provider's schemas. |
| [2. Write your own requirement](2-requirements.md) | State facts, write a rule over them, and choose what runs with `check main`. |
| [3. Allow a known finding](3-allowances.md) | Accept a finding with a reason, and see it count again after a date. |
| [4. Test a behavior](4-behaviors.md) | Run code through a checker, find a bug, fix it, and see evidence reused. |
| [5. Set up your editor](5-editors.md) | Get highlighting, errors as you type, go-to-definition and formatting, and add `lm` and `lemma` to CI. |

The first four are tested: Lemma's test suite runs every command they show, with the files they show, and compares the output, so what you see is what Lemma prints.

For Lemma's other side, finding references and definitions with language servers, see [Getting started](../getting-started.md).
