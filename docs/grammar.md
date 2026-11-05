# Grammars

A grammar says what `lemma verify` checks. It declares the relations facts arrive in, rules that derive new relations from them, and requirements whose matches are findings. Requirements live in grammars, not in Lemma's code, so a team can add a check, tighten one, or turn one off without changing Lemma.

The built-in grammar is [`grammars/http-contracts.yaml`](../grammars/http-contracts.yaml). A manifest uses it unless it names another:

```yaml
grammar: contracts.yaml        # relative to the manifest
change: { ... }
repositories: { ... }
```

Start a custom grammar by copying the built-in one: Lemma's extractors write facts into the relations it declares (see [Relations the extractors fill](#relations-the-extractors-fill)), so keeping those declarations keeps the facts.

Grammars are YAML for now. A `.ls` syntax with the same meaning is planned.

## A grammar file

```yaml
grammar: http-contracts          # a name, used in messages

entities:                        # optional: named kinds of things
  Operation: { method: Text, path: Text }

relations:                       # input relations: where facts arrive
  returns:
    args: [Operation]            # argument columns, by entity or type
    attributes: { field: Text }  # further named columns, in order
    sources: [syntax, spec]      # who may produce its facts
    description: An operation's response has this field.

rules:                           # derived relations, in Datalog
  - removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).

requirements:                    # conditions whose matches are findings
  consumer_reads_removed_field:
    when: reads(c, op, f), removed_field(op, f)
    severity: error              # error, warning or info
    explain: "{c} reads `{f}` from {op}, which no longer returns it"

facts:                           # facts declared by hand
  - { relation: rejects_unknown_fields, values: [billing] }
  - { relation: returns, values: [get_account, status], snapshot: base }
```

| Section | Holds |
| --- | --- |
| `grammar` | The grammar's name. Required. |
| `entities` | Kinds of things and their attributes. Optional; an entity used as a column type is stored as text. |
| `relations` | Input relations. Each column is typed `Text`, `Number`, `Symbol` or an entity. |
| `rules` | Rules deriving relations. A derived relation needs no declaration: its columns come from the rule's head. |
| `requirements` | Named conditions. Every match is a finding with the requirement's severity and message. |
| `facts` | Facts written by hand: strings for text, integers for numbers. `snapshot: base` puts a fact in the base snapshot; the head is the default. |

Unknown keys are errors, so a misspelled section fails loudly.

## Snapshots

A change has two snapshots: `base`, before it, and `head`, with it. Every input relation exists in both. `relation@base(...)` reads the base snapshot and `relation@head(...)` or plain `relation(...)` the head.

Facts from the changed repository differ between the two; facts from every other repository are the same in both. That is what lets a rule ask what changed:

```text
removed_operation(op) :- operation@base(s, op, m, p), !operation@head(s, op, m, p).
```

Derived relations are computed once, from both snapshots' inputs, so `@base` and `@head` apply only to input relations. A rule that needs "derived in the base" derives it from `@base` inputs under its own name.

## Rules

Lemma's rule language is Datalog: a rule says its head holds whenever every condition in its body holds.

```text
head(x, y) :- a(x, z), b@base(z, y), !c(y), y != "skip", n > 120.
```

| Written | Means |
| --- | --- |
| `name(args)` | The relation holds for these values. |
| `name@base(args)`, `name@head(args)` | The same, in one snapshot (input relations only). |
| `!name(args)` | The relation does not hold. |
| `x`, `op`, `leaf` | Variables: identifiers bind to values. |
| `_` | Matches anything, binding nothing. |
| `"text"` | A text constant. |
| `120`, `-1` | A number constant. |
| `=`, `!=` | Compare any two values. |
| `<`, `<=`, `>`, `>=` | Compare numbers. |

The final period is optional. Several rules with the same head are alternatives: the head holds if any of them does.

```text
calls(c, op, f, l) :- calls_path(c, m, p, f, l), operation@base(_, op, m, p).
calls(c, op, f, l) :- calls_client(c, ch, f, l), client_operation(_, ch, m, p, _, _), operation@base(_, op, m, p).
```

Rules can be recursive:

```text
reaches(a, b) :- imports(a, b).
reaches(a, c) :- reaches(a, b), imports(b, c).
```

### What makes a rule valid

Lemma checks every rule before running it and says which rule is wrong and why:

- **Safe.** Every variable in the head, in a negated atom and in a comparison appears in a positive atom of the body.
- **No wildcards in negation.** `!parameter@head(op, _, loc)` is rejected; derive the projection first: `has_parameter(op, loc) :- parameter@head(op, _, loc).` then `!has_parameter(op, loc)`.
- **Stratified.** A relation cannot depend on its own negation, directly or through other rules. `a(x) :- b(x), !a(x).` is rejected.
- **Typed.** A column holds text or numbers, never both; ordering comparisons need numbers.
- **Declared.** Every relation in a body is an input relation or derived by some rule; misspellings are errors.
- **Inputs are inputs.** A rule cannot derive into an input relation.

## Requirements

```yaml
requirements:
  consumer_sends_rejected_field:
    when: calls(c, op, f, l), removed_request_field(op, fld, leaf), uses(c, leaf), rejects_unknown(op)
    severity: error
    explain: "{c} calls {op} and names `{leaf}`; request field `{fld}` was removed and {op} now rejects requests that send it ({f}:{l})"
```

`when` is a rule body. Its variables, in the order they first appear in a positive atom, become the finding's values; `explain` refers to them as `{name}`. Findings are sorted by severity, requirement and message, so output is the same every run.

Each finding also says whether the change introduced it. A requirement that compares the snapshots (it reads `@base`, directly or through a rule) only matches what changed, so its findings are introduced. A requirement over one snapshot, such as a route its spec does not declare, may already have matched before the change: Lemma evaluates it at the base too, and a finding that matches there as well is reported as not introduced. Findings are matched by their text values, not their numbers, so a route that only moved down its file is the same route. For a relation only such a base evaluation reads, facts the grammar declares without a snapshot hold in both snapshots, unless it declares base facts of its own.

Severity decides what a finding does: `lemma verify` exits with 4 when any finding is an error, so CI can gate on errors while warnings and informational findings only report. Choose severity by what a match proves:

| Severity | Use when a match means | Built-in examples |
| --- | --- | --- |
| `error` | A request will fail | calling a removed operation; omitting a newly required field |
| `warning` | Behaviour changes, but nothing fails | reading a removed response field; sending a field now ignored |
| `info` | Worth a look | a changed operation the consumer never names the field of; a route missing from the spec |

A requirement that rests on a name being present (`uses(c, leaf)`) should look near the call. One that rests on a name being absent (`!sends(c, leaf)`) should look as far as the request can be built, and no further: the caller's function, declarations its file sees, and the functions of its file it calls. Searching the whole file lets an unrelated function naming the field hide a real break. The built-in grammar's comments say what each relation covers.

`--explain` attaches each finding's derivation: the requirement's fact, then the facts and rules supporting it, indented, down to input facts and `not` lines for negations.

## Relations the extractors fill

`lemma verify` writes these input relations, in both snapshots unless noted. A grammar receives the ones it declares and ignores the rest. Operations are named `METHOD /path` with path parameters as `{}`; consumers are `repository:file::Function` (or `repository:file` outside functions); files are `repository:file`.

| Relation | Columns | From |
| --- | --- | --- |
| `operation` | service, operation, method, path | service specs |
| `parameter` | operation, name, location | service specs |
| `required_parameter` | operation, name, location | service specs |
| `request_field` | operation, field path (`hooks[].handler.action`), last name | service specs |
| `required_request_field` | operation, field path, last name | service specs: required in every enclosing object and every union branch |
| `response_field` | operation, field path, last name | service specs, 2xx responses |
| `strict_request` | operation | service specs: `additionalProperties: false` |
| `route` | service, method, path, file, line | service route code |
| `route_schema` | service, method, path, part (`body`/`query`), file, schema | service route code |
| `inline_strict_route` | service, method, path, part | a route's own schema is `.strict()` |
| `strict_schema` | file, name | `const name = z.object(..).strict()` |
| `union_member` | file, union, member | `z.union`/`z.discriminatedUnion` members; `*` for a non-strict inline member |
| `client_operation` | client, function, method, path, file, line | `generated_from` specs, functions named by `call_names` |
| `client_parameter` | client, method, path, name, location | client specs |
| `calls_path` | consumer, method (`*` if unknown), path, file, line | consumer code: path strings and request calls |
| `calls_client` | consumer, chain, file, line | consumer code: calls through client receivers and imported client functions |
| `mentions` | scope, name | names used as fields: object keys, members, keyword arguments, dictionary keys, struct fields, JSON tags |
| `in_file` | scope, file | which file a consumer or function is in |
| `imports` | file, imported file | relative and alias imports; for Go, files of one package |
| `addressed_to` | path, service | a path called in code that is under a prefix the service owns: derived from its specs, or its `path_prefixes` |
| `part_of_service` | file, service | files of a consumer entry that names its `service` |
| `rejects_unknown_fields` | service | manifest `rejects_unknown_fields: true` |

## Recipes

**Turn a check off.** Delete the requirement from your copy, or lower its severity to `info` to keep seeing it without failing CI.

**Ignore a known exception.** Declare it and exclude it:

```yaml
relations:
  allowed_removal: { args: [Text], sources: [declared] }    # operation
facts:
  - { relation: allowed_removal, values: ["DELETE /v1/legacy/things/{}"] }
requirements:
  consumer_calls_removed_operation:
    when: calls(c, op, f, l), removed_operation(op), !allowed_removal(op)
    severity: error
    explain: "{c} calls {op}, which no longer exists ({f}:{l})"
```

**Add a check.** Say a team never removes a response field without a deprecation period. Make every removed response field an error, whoever reads it:

```yaml
requirements:
  response_field_removed:
    when: removed_response_field(op, fld, leaf)
    severity: error
    explain: "{op} no longer returns `{fld}`"
```

**Look before writing a requirement.** Print a relation to see what a rule concludes on a real change:

```sh
lemma verify manifest.yaml --relation removed_response_field --relation uses --json
```

**Test a grammar.** Keep manifests for changes whose right answer you know, from replays of your history ([Onboarding](onboarding.md#6-calibrate-on-history)), and run them after every grammar change. Facts declared in the grammar make small self-contained cases:

```yaml
facts:
  - { relation: operation, values: [billing, "GET /v1/invoices", GET, /v1/invoices], snapshot: base }
  - { relation: calls_path, values: ["web:src/a.ts::load", GET, /v1/invoices, "web:src/a.ts", 3] }
```

## Engines

Rules run on [Soufflé](https://souffle-lang.github.io) when it is installed and on Lemma's built-in engine otherwise; `--engine souffle` or `--engine reference` picks one. Both give the same results; Soufflé is about six times faster on large manifests. Soufflé's numbers are 32-bit: a fact with a larger number is reported, not truncated.
