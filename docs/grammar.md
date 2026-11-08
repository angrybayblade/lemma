# Requirements and rules

A requirement says what must hold. It is written in `.lm`, over relations the extractors fill with facts from documents and code, and rules that derive new relations from them. Every match of a requirement is a finding, so a team can add a check, tighten one, or turn one off without changing Lemma.

The built-in requirements are the standard-library module `lemma.http` ([reference](standard-library.md#lemmahttp)). `lemma.http` is always loaded, and a module of your own adds to it:

```lm
module contracts

import { removed_response_field } from lemma.http

rule dropped(op: Text, fld: Text) {
    (op, fld) :- removed_response_field(op, fld, _)
}

export require response_field_removed {
    severity: error
    when: dropped(op, fld)
    message: "{op} no longer returns `{fld}`"
}
```

`lemma check` runs a module's requirements when the module is the workspace's root module or one the root module imports. Without a `check main`, every requirement of every loaded module runs, `lemma.http`'s included; a `check main` selects what runs. See [The `.lm` language](language.md#requirements-of-your-own).

Keep `lemma.http`'s variable names (`c`, `op`, `f`, `l`) in requirements `lemma check` reports on: findings' values are looked up by those names.

## Snapshots

A change has two snapshots: `base`, before it, and `head`, with it. Every input relation exists in both. `relation@base(...)` reads the base snapshot and `relation@head(...)` or plain `relation(...)` the head.

Facts from the changed repository differ between the two; facts from every other repository are the same in both. That is what lets a rule ask what changed:

```lm
import { operation } from lemma.http

rule gone(op: Text) {
    (op) :-
        operation@base(s, op, m, p),
        !operation@head(s, op, m, p)
}
```

Derived relations are computed once, from both snapshots' inputs, so `@base` and `@head` apply only to input relations. A rule that needs "derived in the base" derives it from `@base` inputs under its own name.

## Rules

Lemma's rule language is Datalog: a rule says its head holds whenever every condition in its body holds. A rule declares its columns in place, and each `(head) :- body` inside it is an alternative: the head holds if any of them does.

```lm
relation a(x: Text, z: Text)
relation b(z: Text, y: Text, n: Int)
relation c(y: Text)

rule head(x: Text, y: Text) {
    (x, y) :-
        a(x, z),
        b@base(z, y, n),
        !c(y),
        y != "skip",
        n > 120
}
```

| Written | Means |
| --- | --- |
| `name(args)` | The relation holds for these values. |
| `name@base(args)`, `name@head(args)` | The same, in one snapshot (input relations only). |
| `!name(args)` | The relation does not hold. |
| `x`, `op`, `leaf` | Variables: lowercase names bind to values. |
| `_` | Matches anything, binding nothing. |
| `"text"`, uppercase names, dotted paths | Text and named constants. |
| `120`, `-1` | A number constant. |
| `==`, `!=` | Compare any two values. |
| `<`, `<=`, `>`, `>=` | Compare numbers. |

Several `(head) :- body` lines are alternatives, and rules can be recursive:

```lm
relation imports_file(file: Text, imported: Text)

rule reaches(a: Text, c: Text) {
    (a, b) :- imports_file(a, b)
    (a, c) :- reaches(a, b), imports_file(b, c)
}
```

### What makes a rule valid

Lemma checks every rule before running it and says which rule is wrong and why. `lm check` reports most of these with their line; a wildcard in a negation and a relation depending on its own negation are reported when `lemma check` compiles the rules, and stop it with exit 1:

- **Safe.** Every variable in the head, in a negated atom and in a comparison appears in a positive atom of the body.
- **No wildcards in negation.** `!parameter@head(op, _, loc)` is rejected; derive the projection first with a rule `has_parameter(op, loc)` over `parameter@head(op, _, loc)`, then write `!has_parameter(op, loc)`.
- **Stratified.** A relation cannot depend on its own negation, directly or through other rules.
- **Typed.** A column holds one type; ordering comparisons need numbers.
- **Declared.** Every relation in a body is a declared relation, or derived by some rule; misspellings are errors.
- **Inputs are inputs.** A rule cannot derive into a declared relation.

## Requirements

```lm
import { calls, removed_request_field, uses, rejects_unknown } from lemma.http

export require consumer_sends_rejected_field {
    severity: error
    when: calls(c, op, f, l), removed_request_field(op, fld, leaf), uses(c, leaf), rejects_unknown(op)
    message: "{c} calls {op} and names `{leaf}`; request field `{fld}` was removed and {op} now rejects requests that send it ({f}:{l})"
}
```

`when` is a rule body. Its variables, in the order they first appear in a positive atom, become the finding's values; `message` refers to them as `{name}`. Findings are sorted by severity, requirement and message, so output is the same every run.

Each finding also says whether the change introduced it. A requirement that compares the snapshots (it reads `@base`, directly or through a rule) only matches what changed, so its findings are introduced. A requirement over one snapshot, such as a route its spec does not declare, may already have matched before the change: Lemma evaluates it at the base too, and a finding that matches there as well is reported as not introduced. Findings are matched by their text values, not their numbers, so a route that only moved down its file is the same route.

Severity decides what a finding does: at level `check`, `lemma check` exits with 4 when any finding is an error, so CI can gate on errors while warnings and informational findings only report. Choose severity by what a match proves:

| Severity | Use when a match means | Built-in examples |
| --- | --- | --- |
| `error` | A request will fail | calling a removed operation; omitting a newly required field |
| `warning` | Behaviour changes, but nothing fails | reading a removed response field; sending a field now ignored |
| `info` | Worth a look | a changed operation the code never names the field of; a route missing from the spec |

A requirement that rests on a name being present (`uses(c, leaf)`) should look near the call. One that rests on a name being absent (`!sends(c, leaf)`) should look as far as the request can be built, and no further: the caller's function, declarations its file sees, and the functions of its file it calls. Searching the whole file lets an unrelated function naming the field hide a real break. `lemma.http`'s comments say what each relation covers.

`lemma check --explain` attaches each inferred finding's derivation: the requirement's fact, then the facts and rules supporting it, indented, down to input facts and `not` lines for negations.

### Reading only what a change touches

By default `lemma check` reads only the consumer code a finding can rest on (`--evaluation demand`). Lemma decides from the rules' structure whether a module's requirements allow that. Each requirement must be one of these:

- **Complete**: it reads nothing from consumer code, such as a route its spec does not declare.
- **Anchored**: every call it rests on is a call of an operation held by a relation computed without consumer code (`removed_operation(op)`), and its other consumer facts are about the calling code: its function, its file, and the files that file imports. Imports followed through a recursive rule are not bounded, so such a requirement is not anchored.
- **Change-local**: each consumer fact it rests on is guarded by the negation of the same row at the base, through a rule that reads only that input at the base, with no conditions, and keeps its calling code (`!called_before(c, m, p)`), so it can hold only for code the change edits. Its other negations may only be such guards, or the service membership of the guarded call's file.

When any requirement is none of these, every consumer file is read and the report's `evaluation` names the requirement.

## Relations the extractors fill

`lemma check` fills these input relations of `lemma.http`, in both snapshots unless noted. Operations are named `METHOD /path` with path parameters as `{}`; consumers are `repository:file::Function` (or `repository:file` outside functions); files are `repository:file`.

| Relation | Columns | From |
| --- | --- | --- |
| `operation` | service, operation, method, path | service documents |
| `parameter` | operation, name, location | service documents |
| `required_parameter` | operation, name, location | service documents |
| `request_field` | operation, field path (`hooks[].handler.action`), last name | service documents |
| `required_request_field` | operation, field path, last name | service documents: required in every enclosing object and every union branch |
| `response_field` | operation, field path, last name | service documents, 2xx responses |
| `strict_request` | operation | service documents: `additionalProperties: false` |
| `route` | service, method, path, file, line | service route code |
| `route_schema` | service, method, path, part (`body`/`query`), file, schema | service route code |
| `inline_strict_route` | service, method, path, part | a route's own schema is `.strict()` |
| `strict_schema` | file, name | `const name = z.object(..).strict()` |
| `union_member` | file, union, member | `z.union`/`z.discriminatedUnion` members; `*` for a non-strict inline member |
| `client_operation` | client, function, method, path, file, line | `generated_from` documents of `http.generated_client`, functions named by `call_names` |
| `client_parameter` | client, method, path, name, location, file, line | a client's documents; the file and line of the client method sending the operation, or the document with line 0 |
| `calls_path` | consumer, method (`*` if unknown), path, file, line | service code: path strings and request calls |
| `calls_client` | consumer, chain, file, line | service code: calls through client receivers and imported client functions |
| `mentions` | scope, name | names used as fields: object keys, members, keyword arguments, dictionary keys, struct fields, JSON tags |
| `calls_function` | scope, name | functions a function calls by name |
| `in_file` | scope, file | which file a consumer or function is in |
| `named` | function, simple name | `update` for `Accounts.update` |
| `imports` | file, imported file | relative and alias imports; for Go, files of one package |
| `addressed_to` | path, service | a path called in code that is under a prefix the service owns, derived from its documents |
| `part_of_service` | file, service | a service's own files |
| `rejects_unknown_fields` | service | `rejects_unknown_fields: true` on the service |
| `public_operation`, `public_service` | operation; service | stated in a graph, see [Checking links](check.md#is-it-wired-up) |

## Recipes

**Turn a check off.** In a `check main`, run the rest and skip it, or lower its severity to keep seeing it without failing CI:

```lm
import lemma.http

check main {
    run lemma.http.*
    run lemma.http.consumer_calls_changed_operation { severity: warning }
    skip lemma.http.consumer_sends_ignored_parameter
}
```

**Allow a known finding.** An `allow` keeps it visible, with a reason and an expiry date. See [Allowances](language.md#allowances).

**Add a check.** Say a team never removes a response field without a deprecation period. The example at the top of this page makes every removed response field an error, whoever reads it.

**Look before writing a requirement.** Print a relation to see what Lemma concludes at the working directory (`calls`, `uses`), and explain a finding on a real change to see the facts behind it:

```sh
lemma verify --relation calls --relation uses --json
lemma check --change billing --base main~1 --head main --explain
```

`lemma verify` reads one snapshot, so a relation that compares snapshots, such as `removed_response_field`, is empty there.

**Test a requirement.** Keep past changes whose right answer you know, from replays of your history ([Onboarding](onboarding.md#6-calibrate-on-history)), and replay them after every change to a requirement.

## Engines

Rules run on [Soufflé](https://souffle-lang.github.io) when it is installed and on Lemma's built-in engine otherwise; `--engine souffle` or `--engine reference` picks one. Both give the same results; Soufflé is about six times faster on large systems. Soufflé's numbers are 32-bit: a fact with a larger number is reported, not truncated.
