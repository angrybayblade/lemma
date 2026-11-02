---
name: lemma-grammar
description: Write, change and debug Lemma grammars - the YAML files of relations, Datalog rules and requirements that decide what `lemma verify` reports. Use when asked to add, remove, tune or explain a Lemma check, change a finding's severity, allow a known exception, or understand why `lemma verify` reported (or missed) something.
---

# Working with Lemma grammars

A grammar decides what `lemma verify` checks. Extractors fill input relations with facts from specs and code; rules derive new relations; every match of a requirement is a finding. The built-in grammar is `grammars/http-contracts.yaml` in Lemma's repository.

## Workflow

1. **Start from the built-in grammar.** Copy it next to the manifest and point the manifest at it: `grammar: contracts.yaml`. Keep its relation declarations: extractors only fill relations the grammar declares.
2. **Look at the facts first.** Before writing a rule, print what Lemma already concludes on a real change:
   ```sh
   lemma verify manifest.yaml --relation calls --relation removed_request_field --json
   ```
   `--relation` takes any derived relation; input relations show up in `--explain` output.
3. **Write the smallest change.** A new requirement over existing derived relations is usually enough. Add a rule only when the condition is reused or needs alternatives.
4. **Run it on known cases.** Rerun the manifests whose right answer you know (past breaks and non-breaks). Compare findings before and after.
5. **Explain a surprising finding** with `--explain`. Read from the bottom: the input facts. Fix the grammar if the logic is wrong; fix the manifest if a fact is missing or wrong.

## Syntax in one screen

```yaml
grammar: contracts
relations:                                   # inputs: declared with column types
  allowed_removal: { args: [Text], sources: [declared] }
rules:                                       # Datalog; head :- body.
  - removed_operation(op) :- operation@base(s, op, m, p), !operation@head(s, op, m, p).
requirements:
  consumer_calls_removed_operation:
    when: calls(c, op, f, l), removed_operation(op), !allowed_removal(op)
    severity: error                          # error | warning | info
    explain: "{c} calls {op}, which no longer exists ({f}:{l})"
facts:                                       # by hand; head snapshot unless snapshot: base
  - { relation: allowed_removal, values: ["DELETE /api/v3/old/{}"] }
```

- Identifiers are variables, `_` matches anything, `"text"` and `123` are constants.
- `name@base(..)` / `name@head(..)` read one snapshot of an **input** relation; plain `name(..)` is the head.
- `!name(..)` is negation; comparisons are `= != < <= > >=` (ordering on numbers only).
- Several rules with one head are alternatives. Recursion is allowed.

## Rules Lemma enforces

Every one fails with a message naming the rule:

- Every variable in the head, a negation or a comparison appears in a positive atom.
- No `_` inside a negation: derive a projection first (`has_x(a) :- x(a, _).` then `!has_x(a)`).
- No relation depends on its own negation (stratification).
- No `@base`/`@head` on derived relations.
- Every relation used exists (declared input or derived somewhere).
- A column holds text or numbers, not both.

## Choosing severity

`lemma verify` exits 4 on any `error`, so CI gates on errors only.

| Severity | Use when a match means |
| --- | --- |
| `error` | A request will fail |
| `warning` | Behaviour changes without failing |
| `info` | Worth a look, not a problem by itself |

Findings that rest on a name being **present** should use near evidence (`uses(c, name)`); findings that rest on a name being **absent** should search broadly (`!names_anywhere(f, name)`). Mixing them up produces false findings.

## Recipes

- **Turn a check off**: delete the requirement, or set `severity: info`.
- **Allow a known exception**: declare a relation, add facts, and negate it in the requirement (above).
- **Make every removed response field an error**:
  ```yaml
  response_field_removed:
    when: removed_response_field(op, fld, leaf)
    severity: error
    explain: "{op} no longer returns `{fld}`"
  ```
- **Restrict a check to one service**: join on `operation@base(s, op, _, _), s = "gateway"`.

The input relations, their columns and the built-in derived relations are listed in [reference.md](reference.md).
