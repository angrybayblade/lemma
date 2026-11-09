---
name: lemma-grammar
description: Write, change and debug Lemma's `.lm` requirements - the relations, Datalog rules, `require`, `check main` and `allow` declarations that decide what `lemma check` reports. Use when asked to add, remove, tune or explain a Lemma check, change a finding's severity, allow a known finding, or understand why `lemma check` reported (or missed) something.
---

# Working with Lemma requirements

A module of relations, rules and requirements decides what `lemma check` reports. Extractors fill input relations with facts from documents and code; rules derive new relations; every match of a requirement is a finding. The built-in requirements are the standard-library module `lemma.http`, which is always loaded.

## Workflow

1. **Write it in a module `lemma check` runs**: the workspace's root `mod.lm`, or a module the root module imports. A module imports `lemma.http`'s relations and rules by name.
2. **Look at the facts first.** Before writing a rule, print what Lemma concludes, and explain a finding on a real change:
   ```sh
   lemma verify --relation calls --relation uses --json
   lemma check --change <repo> --base <commit>~1 --head <commit> --explain
   ```
   `--relation` takes any derived relation, at one snapshot, so relations that compare snapshots are empty there. Input relations show up in `--explain` output.
3. **Write the smallest change.** A new requirement over existing derived relations is usually enough. Add a rule only when the condition is reused or needs alternatives.
4. **Run `lm check`** after every edit (and `lm fmt` to lay it out). It catches a misspelled relation or a wrong column count with its line before anything runs.
5. **Run it on known cases.** Replay the changes whose right answer you know (past breaks and non-breaks) with `lemma check --change <repo> --base <commit>^ --head <commit>`. Compare findings before and after.
6. **Explain a surprising finding** with `--explain`. Read from the bottom: the input facts. Fix the requirement if the logic is wrong; fix the description if a fact is missing or wrong.

## Syntax in one screen

```lm
module contracts

import { calls, removed_operation } from lemma.http

relation Deprecated(operation: Text)            // input: facts stated in a graph

graph deprecated : Deprecated {
    "DELETE /v1/legacy/things/{}"
}

rule still_called(op: Text) {                   // derived; each line is an alternative
    (op) :- calls(_, op, _, _), removed_operation(op)
}

export require consumer_calls_deprecated {
    severity: error                             // error | warning | info
    when: calls(c, op, f, l), Deprecated(op), !still_called(op)
    message: "{c} calls {op}, which is deprecated ({f}:{l})"
}

check main {
    run lemma.http.*
    run consumer_calls_deprecated { severity: warning }
}
```

- In rules and `when:` bodies, a lowercase bare name is a variable; constants are literals, uppercase names or dotted paths. `_` matches anything, `"text"` and `123` are constants. `x == "*"` compares.
- `name@base(..)` / `name@head(..)` read one snapshot of an **input** relation; plain `name(..)` is the head.
- `!name(..)` is negation; comparisons are `== != < <= > >=` (ordering on numbers only).
- Several `(head) :- body` lines are alternatives. Recursion is allowed.
- Without a `check main`, every requirement of every loaded module runs, `lemma.http`'s included. A `check main` selects what runs; one that names only behaviors runs no HTTP check, so add `run lemma.http.*`. `lemma check --select <name>` runs another check.
- Keep `lemma.http`'s variable names (`c`, `op`, `f`, `l`) in requirements `lemma check` reports on: it looks findings' values up by those names.

### Allowing a known finding

An `allow` keeps the finding visible as allowed, says why, can expire, and is reported when it no longer covers anything.

```lm
module contracts

import lemma.http

allow lemma.http.consumer_calls_removed_operation {
    when: f == "src/legacy/sessions.ts"
    reason: "the legacy client is removed with the next major version"
    until: "2027-01-31"
}
```

- `when` reads the requirement's message variables (for a behavior, its parameters and loop variables, such as `flag == false`). Values are compared by type: `l < 100` reads a line as an integer.
- A covered finding is informational and ends with `(allowed: <reason>)`. `until` is compared with the committer date of the newest commit the check reads (a change's head); past it the findings count again and the allowance is reported as expired.
- `lemma check` reports an allowance for a behavior that ran and covered none of its cases; remove it. Allowances for requirements are not reported as unused, since a check evaluates requirements only over what a change touches.

### Behaviors

A property a query over facts cannot settle (what a handler returns when its upstream fails) is a `behavior`: statements that `expect` conditions, with `need` asking a `checker` command for evidence. See the worked example in [The `.lm` language](https://github.com/angrybayblade/lemma/blob/main/docs/language.md#behaviors-and-checkers).

- Keep relational properties in `require`, where change-driven evaluation narrows what is read; loop in a behavior only over cases the checker must run.
- Start a behavior with `expect link.exists` (or similar) so deleting what it tests makes it violated rather than passing through an empty loop.
- A behavior's outcome is `violated`, `unknown` (no evidence, a missing condition, no assertion ran, or a loop over facts read from code), `stale` (under `--no-run`, when kept evidence no longer matches) or `satisfied`. At level `check`, only violated error-severity behaviors fail; at `strict`, unknown and stale ones fail too.
- A checker reads `$LEMMA_INPUT` (its arguments, operations with their file and symbol, and `roots`, each repository's checkout) and writes `{"observed": ...}` or `{"unavailable": "why"}` to `$LEMMA_OUTPUT`. Its evidence is kept and reused while its code and arguments are unchanged; have Git ignore what it writes into the repository, or every run changes the working directory's state.

The full language is in [The `.lm` language](https://github.com/angrybayblade/lemma/blob/main/docs/language.md).

## Rules Lemma enforces

Every one fails with a message naming the rule. `lm check` reports most of them with a line; the `_` in a negation and stratification fail when `lemma check` compiles the rules, with exit 1:

- Every variable in the head, a negation or a comparison appears in a positive atom.
- No `_` inside a negation: derive a projection first (a rule `has_x(a)` over `x(a, _)`, then `!has_x(a)`).
- No relation depends on its own negation (stratification).
- No `@base`/`@head` on derived relations.
- Every relation used exists (declared or derived somewhere).
- A column holds one type.

## Choosing severity

`lemma check` exits 4 on any `error` at level `check`, so CI gates on errors only.

| Severity | Use when a match means |
| --- | --- |
| `error` | A request will fail |
| `warning` | Behaviour changes without failing |
| `info` | Worth a look, not a problem by itself |

Findings that rest on a name being **present** should use near evidence (`uses(c, name)`); findings that rest on a name being **absent** should look as far as the request can be built and no further (`!sends(c, name)`: the caller, declarations its file sees, and functions of its file it calls). Searching the whole file lets an unrelated function hide a real break.

## Recipes

- **Turn a check off**: `skip lemma.http.<name>` in a `check main`, or `run lemma.http.<name> { severity: info }`.
- **Allow a known finding**: an `allow` (above).
- **Make every removed response field an error**:
  ```lm
  import { removed_response_field } from lemma.http

  export require response_field_removed {
      severity: error
      when: removed_response_field(op, fld, leaf)
      message: "{op} no longer returns `{fld}`"
  }
  ```
- **Restrict a check to one service**: join on `operation@base(s, op, _, _), s == "#billing:api"`.

The input relations, their columns and the built-in derived relations are listed in [reference.md](reference.md).
