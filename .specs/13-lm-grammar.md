# The `.lm` grammar and verification semantics

Captured: 2026-11-05. Revised: 2026-11-06, draft 0.2. Status: **implemented**, both phases of [section 15](#15-implementation-boundary-and-remaining-work); that section lists what is left. This records the language design, worked through a platform example whose services are named `functions`, `worker` and `gateway`. `lemma.yaml` and grammar YAML keep working beside it. [Changes from draft 0.1](#16-changes-from-draft-01) lists what this revision tightened.

The intended consumer is an AI coding agent. Lemma should give the agent task-specific context, discover technical obligations affected by a change, and return evidence tied to particular input snapshots. The `.lm` surface combines configuration, reusable modules, software declarations, typed relationships, derived graphs, requirements, and the checkers that produce evidence for them.

This builds on [relational grammar and verification](08-relational-grammar-and-verification.md), [composable manifests](09-composable-manifests.md) and [change-driven evaluation](11-change-driven-evaluation.md). `.lm` is the extension; `.ls` in older notes is historical. Everything `lemma.yaml` and grammar YAML say today must be expressible in `.lm` and lower to the same structures (section 13), so the language can be proven on existing configurations before it adds anything new.

## Model and execution layers

```text
workspace -> repositories -> modules -> services -> operations
                                  |
                      relations, graphs and facts
                                  |
                        rules (derived graphs)
                                  |
          requirements: require (relational), behavior (evidence)
                                  |
                   checks -> outcomes, findings, evidence
```

**`->` constructs a directed relationship. Its meaning comes from the graph's relation type.** Words such as `invokes`, `owns`, and `deploys` remain ordinary identifiers.

| Layer | Declarations | Execution |
| --- | --- | --- |
| Declarations | `workspace`, `repo`, `service`, `operation`, `entity`, `relation`, `graph` | Describe inputs. Resolution and discovery requests are built, not run |
| Rules | `rule` | Pure, finite Datalog over the facts of a change's base and head snapshots |
| Requirements | `require`, `behavior`, `allow` | `require` turns a rule body into findings. `behavior` runs statements that may demand evidence |
| Evidence | `checker` | External execution, keyed by recorded inputs and reused while they hold |

Three properties hold everywhere:

1. **Loading never executes.** Parsing or importing a module runs no provider, command or checker. They run when a check demands their output.
2. **Same inputs, same outcome.** Iteration is sorted by identity. There is no clock, environment or network access except through checkers, whose inputs are recorded.
3. **Absence is evidence only under complete coverage.** Otherwise the answer is unknown, naming the missing coverage.

## 1. Lexical conventions

Source files are UTF-8 with the `.lm` extension.

```lm
// Line comment

/* Block comment */

let name = "worker"
let enabled = true
let retries = 3
let services = [Worker, Functions]
let config = {
    timeout_ms: 5000
    transport: "http"
}
```

- **Separators.** A newline or `;` ends a declaration, statement, record field or graph row; record fields may also be separated by commas, as in `{ include: ["src"], exclude: ["tests"] }`. A closing `}` or the end of the file ends the last one without a separator.
- **Continuation.** Newlines inside `(...)` and `[...]` are whitespace. A newline after a binary operator, a comma, `:`, `=`, `->`, `:-` or an opening bracket continues the expression, a line starting with `because` continues the `expect` before it, and a line starting with `else` continues the `if` before it. No other line can start by continuing the previous one, so a line cannot start with a binary operator.
- Indentation has no meaning.
- **Identifiers** are letters, digits and underscores, starting with a letter or underscore. Backticks escape any other name, such as a field called `in`.
- **Case carries meaning in one place only**, patterns (section 7): a lowercase bare name is a variable there. By convention, services, entities, relations and types are `UpperCamel`, and everything else is `snake_case`.
- **Strings** use double quotes and JSON escapes. Interpolation, `{name}` or `{name.field}`, exists only in `message:` and `because` strings; `{{` and `}}` write braces.
- **Numbers** are 64-bit signed integers (`Int`). JSON numbers inside `Json` values compare by exact decimal value.
- **Reserved words:** `module import export from as let fn if else for in return expect because need true false null self`.
- **Contextual words**, which are keywords only where a declaration or member starts: `workspace repo service client operation entity relation graph rule require allow behavior checker check run skip type`. Member names such as `Worker.invokes` or `config.service` stay legal.
- `->` appears only in graph rows and `:-` only in rule clauses, which keeps building a relationship and deriving one distinct.
- There are no user-defined operators. Organisations extend the vocabulary through entities, relations, functions and modules.

`:` gives a named argument, field value, parameter type or return type. `=` binds.

```lm
let result = resolve(symbol: target)

fn identity(value: Bool): Bool {
    return value
}
```

### Layout

Every module has one canonical layout, so a diff shows only what changed. The formatter keeps the author's line breaks, since newlines separate items, and changes only indentation, the spaces within a line and the number of blank lines. It refuses a module with syntax errors, but lays out a fragment without a `module` line like a module. The examples in this document, the standard library and `docs/language.md` are in this layout.

- **Indentation** is four spaces for each bracket open at the start of the line. Brackets opened on the same line count once, so `f({` indents its contents one level. A line starting with a closing bracket lines up with the line that opened it, as in `): Evidence<Json> {`.
- **Continuation.** Where newlines separate, a line that continues the one before (after a binary operator, a comma, `:`, `=`, `->` or `:-`, or starting with `because`) goes one level further in, and every further continued line stays at that level. This is how a rule's body sits under its head and a `when:` body under its field. A line starting with `else` is not indented. After a comma, a line starting with `name:` is a new field, not a continuation. Inside `(...)`, `[...]` and an import's `{...}`, newlines are whitespace and lines take the bracket's level only.
- **Spaces.** One space goes around binary operators, `=`, `->` and `:-`, and after `,`, `;` and `:`. Braces get a space inside them when their contents share their line (`{ include: ["src"] }`, `import { Link } from lemma.links`), but an empty pair is `{}`. No space goes inside `(...)` and `[...]`; before `,`, `;`, `:` and `.`; before a call's `(`, an index's `[` or a snapshot; after a prefix `!` or `-`; or around type arguments (`Fact<Link>`). A reserved word keeps its space before a bracket (`in [true, false]`). The `:` naming a graph's or rule's relation has a space on each side (`graph links : Link`), and so does the `*` of `import * as`. The `.*` of `run` is written tight.
- **Blank lines.** There is at most one blank line in a row, and none at the start or end of the file or of a bracketed block. One follows the `module` line. A top-level declaration whose braced body spans several lines (a service, workspace, rule, requirement, allowance, behavior, function, checker, check, entity, or graph written as rows) has one before and after it. Other blank lines are the author's, so runs of `let`, `relation` and `import` lines can stay grouped.
- **Comments** are kept as written, except that trailing spaces are dropped from line comments. A comment on its own line is indented like the code line after it, or like the inside of the block when that line closes the block. A comment after code is one space from it.
- Lines have no trailing spaces, and the file ends with one newline. A `;` stays where it was written, and lines are neither wrapped nor joined.

## 2. Modules, repositories and workspaces

### Modules

A module is one `.lm` file. Its first item is `module <name>`, and the name is fixed by where the file is:

- A repository's root module is the file `lemma.lm` at its root. It names the repository, as `module functions`, and that name is one segment.
- Any other file is named by the root's name plus its path, without the extension: `checks/setup_replay.lm` in `functions` is `module functions.checks.setup_replay`.
- A name that does not match the file's place is a compile error. So a module name says where its file is, and two repositories cannot define the same module.

### Imports

```lm
import { Link } from lemma.links
import * as python from lemma.python
import { Functions } from functions
import { replay_setup_error } from platform.checks.setup_replay
import platform.services.gateway

export { Worker } from platform.services.worker
```

- An import names a **module**, not a file path or a workspace alias. Names starting `lemma.` are the standard library.
- A module resolves through the repositories the evaluation knows (below). An unknown first segment is an error naming the repository that is missing.
- `import <module>` with no names loads a module only, for the graphs and requirements it declares.
- Declarations are private unless exported. `export { ... } from` re-exports.
- Imports expose names. They do not create relationships, and they do not decide which facts count (section 6).
- Cyclic imports are rejected.

### Repositories

`repo <name> = <source>` says where the module tree named `<name>` comes from. The repository's root module must declare `module <name>`.

```lm
repo functions = local("../functions")
repo functions = git(url: "https://git.example.com/functions.git", rev: "3f2a1c9e04")
```

- `local(path)` reads a working tree, with `path` relative to the declaring file. Its identity is its commit plus the content of every changed file Lemma reads.
- `git(url:, rev:)` reads a commit. `rev` should be a commit; a branch or tag is resolved, and the commit is recorded.
- The repository holding the entry module is always known under its root module's name.

A `repo` declaration may appear in two places:

| Place | Meaning |
| --- | --- |
| Inside the entry module's `workspace` block | Authoritative for this evaluation |
| At the top level of a repository's root module | A default, used when nothing earlier names that repository. This is how a repository checked alone in CI pins what it imports, as external services do in `lemma.yaml` today |

Lemma searches the entry workspace first, then the root modules breadth-first from the entry module, and the first declaration of a name wins. As in `lemma.yaml`, a parent's pins override a repository's. Each evaluation records the source and commit every repository resolved to.

### Workspace

A `workspace` block holds the authoritative repositories and the evaluation's configuration. Only the entry module's block is read. In any other loaded module the block is ignored, and Lemma says so.

```lm
// example/lemma.lm: the workspace entry point
module example

workspace {
    language: "0.2"

    repo functions = local("../functions")
    repo platform = local("../platform")

    config: {
        level: check
        verification: { timeout_ms: 30000 }
    }
}

check main {
    run lemma.links.*
    run lemma.http.*
    run platform.services.worker.forwards_upstream_retriable
}
```

`language` pins the language and standard-library version.

### What an evaluation loads

- The entry module: the file given to `lemma check`, by default `./lemma.lm`.
- The root module of every repository the evaluation resolves.
- Every module a loaded module imports. Standard-library modules load only when imported.

Every loaded module's graphs and requirements count. A `.lm` file in a loaded repository that no loaded module imports is reported, so a graph cannot drop out of a check silently.

## 3. Identity

Every declaration has an **address**: its module name, `::`, and its path inside the module.

```text
functions::Functions.serverless.setup_webhook
platform.services.worker::Worker.call_setup_webhook
platform.services.gateway::Gateway["GET /api/v3/sessions/{}"]
```

- A discovered operation's path is its service plus its **dispatch key**, the protocol-level name a provider gives it. For HTTP that is the method and path, with path parameters written `{}` as in today's operation names. `Gateway["GET /api/v3/sessions/{id}"]` and `Gateway["GET /api/v3/sessions/{}"]` name the same operation.
- Module names are fixed by repository layout, not by a workspace's aliases, so an address is the same in every workspace. Evidence and segments keyed by addresses can be shared between machines (see [segments](12-segments-and-shared-storage.md)).
- An operation's source symbol, file location and resolved revision are resolution results, separate from its address. Moving the implementation does not change the operation.
- An entity value's identity is its type plus the canonical encoding of its field values. The `let` name bound to it is not part of its identity.

Renaming a declaration, or moving it to another module, changes its address. To keep the history, list the old addresses:

```lm
export service serverless {
    export operation setup_webhook {
        previously: ["functions::Functions.serverless.register_webhook"]
        // ...
    }
}
```

Before rules run, `previously` maps base facts about an old address onto the new one, so the change report shows a rename instead of a removal and an addition. Without it, a rename is a removal and an addition.

## 4. Services and operations

A service is a named group of operations and nested services. Containment is a namespace and an ownership relationship. It does not imply a runtime call.

Most operations are **discovered**: providers read them from documents or code. Hand-written `operation` blocks are for what discovery cannot see, such as a queue handler or a serverless function dispatched by name.

```lm
// platform/services/gateway.lm
module platform.services.gateway

import * as http from lemma.http
import * as hono from lemma.hono

export service Gateway {
    index: { include: ["apps/gateway/src"] }

    operations: [
        http.openapi("apps/gateway/openapi.json"),
        hono.app(
            directory: "apps/gateway",
            entry: ["src/index.ts#app", "src/routes"],
            setup: ["pnpm", "install", "--frozen-lockfile"],
        ),
    ]
}
```

Service fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `index` | `{ include, exclude, languages }` | The code Lemma reads for the service |
| `operations` | `Operations` or a list of them | Discovered operations. Later providers in the list replace an earlier provider's operation with the same dispatch key, as code's operations are laid over committed documents today. Each operation records the provider that found it |
| `client` | `Client` | Set when the service is itself a generated client of another, such as `http.generated_client(...)` |
| `symbols` | `List<Text>` | Files whose definitions are the service's public interface |
| `routes` | `{ register, define }` | The calls its code writes routes with |
| `runtime` | record | Interpreters and environment for the commands its providers run |

A provider that reads only part of the code reports partial coverage, and the service's operation set is then incomplete (section 6). Today that is a warning.

### Hand-written operations

A regular `operation` describes behavior a service provides. A `client operation` describes an outbound interaction. Both have type `Operation`, and a client operation also has type `ClientOperation`. `export` controls visibility, separately from the role.

```lm
// functions/lemma.lm
module functions

import * as python from lemma.python

let tools = python.module("functions.serverless.tool_functions")

export service Functions {
    index: { include: ["functions"] }

    export service serverless {
        export operation setup_webhook {
            implementation: tools.symbol("setup_webhook")
            dispatch: "setup_webhook"

            request: tools.type("setup_webhook.Request")
            response: tools.type("setup_webhook.Response")
        }
    }
}
```

| Property | Applies to | Meaning |
| --- | --- | --- |
| `implementation` | both | The source symbol implementing it, a `Resolution<Symbol>` |
| `dispatch` | both | Its dispatch key. Two operations of one service with the same key are an error |
| `request`, `response` | `operation` | The schemas it accepts and returns |
| `sends`, `accepts` | `client operation` | The schemas it sends and expects back |
| `previously` | both | Old addresses (section 3) |

Using the other role's schema properties is a compile error.

A schema source is one of:
- a type in code (`tools.type(...)`);
- a location in a document (`lemma.json.document("spec.json", path: "$.setup.request")`);
- a location in a command's output (`lemma.json.command(["python3", "scripts/spec.py"], path: ...)`), run at the snapshot and cached by the code's Git tree, as resolvers are today.

### Resolution

`tools.symbol(...)` and `tools.type(...)` build **resolution requests**. Lemma resolves them against a specific snapshot: the syntax index first, then a language server to choose between candidates, as `lemma def` does. The result is resolved, ambiguous (with every candidate) or unresolved, and never a similar name chosen silently. The language modules' `module`, `package`, `symbol`, `method` and `type` functions are proposed standard-library interfaces.

For an operation in a link, a binding that resolved at the base and does not at the head violates `lemma.links.link_persists`, today's first link check. Any other unresolved binding is a diagnostic, and the results that depend on it are unknown.

## 5. Values and types

| Type | Values |
| --- | --- |
| `Bool`, `Int`, `Text` | Booleans, 64-bit signed integers, UTF-8 strings |
| `Json` | JSON values: `null`, booleans, numbers, strings, lists, objects |
| `List<T>`, `{ name: T, ... }` | Lists, and records, which are structural |
| `Service`, `Operation`, `ClientOperation` | Declared or discovered. `ClientOperation` is a subtype of `Operation`, the only subtyping in v0.2 |
| `Symbol`, `Site` | A resolved source symbol (repository, file, name, kind, span), and a source location |
| `Resolution<T>` | A resolved `T`, ambiguous candidates, or unresolved |
| `Fact<R>` | The lookup of one tuple of relation `R` (section 6) |
| `Evidence<T>` | A checker's result: a `T` with provenance, or unavailable (section 10) |
| Entity types | Declared with `entity` |

Equality is type-strict: `false`, `0`, `"false"`, `null` and a missing field are all different, and comparing values of different types is a compile error where the types are known.

**Enumerated fields.** `severity` (`error`, `warning`, `info`) and `level` (`audit`, `check`, `strict`) take a bare name from their field's fixed set. The name is not looked up in scope.

### Missing, null and unknown

These are three different things:

- `null` is a JSON null that is present.
- **missing** is what member access (`x.f`) or indexing (`x[i]`) on a `Json` value yields when the field or element is absent. Only `Json` produces it. An absent field of a record or entity type is a compile error.
- **unknown** is a truth Lemma cannot establish, such as the existence of a fact in an incompletely covered graph, or a comparison involving an unresolved binding.

A condition evaluates to true, false, missing or unknown:

| Expression | Result |
| --- | --- |
| A comparison with a missing or unknown operand | unknown if any operand is unknown, otherwise missing |
| `!x` for missing or unknown `x` | `x` unchanged |
| `false && x`, `true \|\| x` | false and true respectively, whatever `x` is |
| Other `&&` and `\|\|` with a missing or unknown side | unknown before missing, as for comparisons |
| `x.has("f")` | Always a `Bool`: true when `f` is present, `null` included |

What statements do with each:

| Statement | true | false | missing | unknown |
| --- | --- | --- | --- | --- |
| `expect e` | holds | violated | violated, reported as "`e` is missing" | unknown |
| `if e` | then-block | else-block | ends the case: violated, condition missing | ends the case: unknown |

So `expect body.retriable == flag` fails with a message saying `retriable` is missing when the field is absent, and a separate `has` check is unnecessary. An `if` never quietly chooses a branch on a value it does not have.

## 6. Relations, facts and graphs

A relation declaration defines a relationship's meaning and the types of its columns.

```lm
// lemma.links: standard library
export relation Link(
    caller: ClientOperation,
    provider: Operation
)
```

A **link** is the relationship `graph.links` declares today: caller to provider, checked for persistence and schema agreement.

A **graph** is a named set of facts of one relation. Its facts are written as rows, supplied by a provider, or derived by a rule (section 7).

```lm
graph serverless_links : Link {
    Worker.call_setup_webhook -> Functions.serverless.setup_webhook
}

graph inferred : Link = http.inferred_links(
    services: [Worker, Functions.serverless]
)
```

Here `http` is `import * as http from lemma.http`. The provider call builds a discovery request. It runs when a check reads `Link`, and its graph carries the provider's coverage, including what it left unresolved.

For a binary relation, `a -> b` is the ordered tuple `(a, b)`. The arrow establishes neither execution order nor value forwarding; those need their own evidence. Wider relations use tuples:

```lm
relation Routes(gateway: Service, path: Text, destination: Operation)

graph routes : Routes {
    (Gateway, "/triggers/setup", Worker.setup_trigger)
}
```

Organisations declare their own entities and relations with the same mechanism:

```lm
export entity Deployment {
    environment: Text
    region: Text
}

export relation Deployed(service: Service, deployment: Deployment)

let production = Deployment(environment: "production", region: "us-east-1")

graph deployments : Deployed {
    Worker -> production
}
```

### What a relation's name means

- **A relation's name denotes the union of every stated graph of that relation in the loaded modules**, rows and providers alike. `Link(c, p)` sees the declared links and the inferred ones.
- A graph's name denotes its own facts only.
- `export` decides who may *name* a graph, not whether its facts count. A module cannot hide facts from a check by not exporting them.
- Rules do not feed a relation's union. A rule typed `: DependsOn` borrows the relation's column types. Reading its facts means naming the rule, so a transitive closure never leaks into the relation it was computed from.

### Facts and provenance

- Facts have set semantics. Duplicate tuples are one fact, and each support (a declaration, a provider's observation, a derivation) keeps its own provenance.
- Entity references are addresses, never display names. Source order does not affect meaning.
- A declared fact carries `declared` provenance, and alone it does not establish that runtime routing matches it.

### Coverage

- A graph of rows is complete for its own facts: the declaration is the source of truth for what was declared.
- A provider reports its coverage, meaning what it read completely and what it left unresolved.
- A relation's union is complete for a scope when every graph contributing to it is complete for that scope.
- Negation and failed lookups over incomplete scopes are unknown, not false.

### Lookup and iteration

`serverless_links[a, b]` looks up one tuple and returns a `Fact<Link>`:

| Member | Meaning |
| --- | --- |
| `.exists` | true; false only under complete coverage; otherwise unknown |
| `.supports` | The provenance of each support, in a deterministic order |
| the columns, `.caller` and `.provider` | The requested endpoints, kept even when the fact is absent, so a report can name what is missing |

`Link[a, b]` looks up the union, and `Link@base[a, b]` looks it up in the base snapshot. `for (caller, provider) in Link { ... }` iterates the facts sorted by address. Whether the domain is complete decides whether the behavior's applicability is complete (section 9).

## 7. Rules

`rule` derives a graph by pure relational evaluation.

```lm
relation DependsOn(source: Service, target: Service)

graph service_dependencies : DependsOn {
    Worker -> Functions.serverless
}

export rule reachable : DependsOn {
    (a, b) :- service_dependencies(a, b)

    (a, c) :-
        reachable(a, b),
        service_dependencies(b, c)
}
```

- `:-` derives the tuple on the left when the literals on the right hold. Commas are conjunction.
- A rule takes its columns from a relation (`rule reachable : DependsOn`) or declares them in place (`rule removed(operation: Text) { ... }`), which suits the many small rules a grammar derives on the way to its requirements.
- Several clauses are alternative derivations.
- Variables are scoped to one clause.
- `!literal` is negation.
- A predicate is a graph, a rule, or a relation (its union).
- Arguments are positional, or all named. Named arguments may leave columns out, and those columns are then `_`: `Routes(destination: d)`.
- Comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`) relate bound terms of one type. `x.field` in a comparison reads a field of an entity-typed variable.

### Variables and constants

In rule clauses, `when:` bodies and `for` patterns:

- A **bare name starting with a lowercase letter is a variable**. `_` is an anonymous variable.
- `x.field`, where `x` is a lowercase name, is always a field of the variable `x`. It is allowed in comparisons.
- A **constant** is a literal, or a path starting with an uppercase name (`Worker`, `Functions.serverless.setup_webhook`, `Gateway["GET /users/{}"]`) or with `self` (`self.production`, for a lowercase binding of the current module).
- A constant from a namespace import (`http.x`) is imported by name instead, so an import alias never meets a variable.
- A variable named like a binding in scope is a warning that suggests `self.<name>`.

This follows the split Haskell patterns make between variables and constructors. Whether a term is a variable depends only on how it is spelled, so adding a declaration or an import can never change what an existing clause means.

### Snapshots

Every fact belongs to a snapshot. A change has a **base** and a **head**. A check without a change has one snapshot, which serves as both.

- `name@base(...)` reads the base and `name@head(...)` the head. An unqualified literal reads the head.
- Graph lookups take the same qualifier: `Link@base[a, b]`.
- Repositories the change does not touch have the same facts in both.
- `previously` mappings (section 3) apply to base facts first.

```lm
relation Removed(operation: Operation)

rule removed_operation : Removed {
    (target) :-
        operation@base(operation: target),
        !operation@head(operation: target)
}
```

### Restrictions

The v0.2 rule language is Datalog, as defined in [spec 08](08-relational-grammar-and-verification.md#rule-semantics):

- Every variable in the head, in a negated literal or in a comparison is bound by a positive literal in the same clause.
- Rules derive tuples over existing values only. They cannot create entities or unbounded values.
- Negation is stratified.
- Rules execute nothing: no commands, network or checkers.
- No aggregates in v0.2.

This gives a predictable fixed point, and a dependency graph that change-driven evaluation can narrow.

## 8. Requirements: `require` and `allow`

A `require` turns a rule body into findings. It is the form for any property that is a query over facts.

```lm
// lemma.http: ported from grammars/http-contracts.yaml
export require consumer_misses_new_required_field {
    severity: error
    when:
        calls(consumer, operation, site),
        new_required_request_field(operation, field, leaf),
        !sends(consumer, leaf)
    at: site
    message: "{consumer} calls {operation}, which now requires `{field}`; its code never names `{leaf}`"
}
```

| Field | Meaning |
| --- | --- |
| `when` | A rule body, evaluated with unqualified literals at the head |
| `at` | A `Site` variable, the finding's location. Optional |
| `message` | Interpolates `when` variables by name |
| `severity` | `error` (default), `warning` or `info`. A check may override it |

- Each distinct binding of the variables used in `message` and `at` is one finding. Other variables are existential.
- Outcome: **satisfied** when there are no findings and the relations the body negates are complete; **violated** when there is at least one finding; **unknown** when a negated relation is incomplete for some binding. Unknown findings name the missing coverage.
- Change-driven evaluation computes a `require`'s anchors from its body as it does for grammar requirements today. That narrowing is why a property that is a query over facts belongs in `require`, not in a behavior's loop.

### Allowances

```lm
allow lemma.http.consumer_calls_removed_operation {
    when: consumer.file == "src/legacy/sessions.ts"
    reason: "the legacy client is removed with the next major version"
    until: "2027-01-31"
}
```

- `when` is a rule body over the requirement's message variables. It selects the findings the allowance covers.
- An allowance never hides a finding. A covered finding is reported as **allowed**, with the reason, and does not fail the check.
- `until` is compared with the head commit's committer date, never the clock, so the same commit always gives the same answer. Past it, the findings count again and the allowance is reported as expired.
- An allowance that covers no finding is reported, so stale allowances get removed. Only a check of the whole system (`lemma check` without a change) reports these: a change-driven run reads only what the change touches, so most allowances rightly cover nothing in it.
- An allowance's variables are its requirement's message and `at` variables, or a behavior's parameters and loop variables, so `allow worker.forwards_upstream_retriable { when: flag == false }` covers that behavior's `flag=false` cases. A grammar's findings carry their values as text; those of `Int` and `Bool` variables are read as such.
- `lemma check` applies allowances to behaviors' cases and to its grammar's findings, and `lemma verify` to the findings of a `.lm` grammar.

## 9. Behaviors

A `behavior` is a requirement that needs evidence beyond facts. It runs statements that query graphs, iterate over cases, call pure functions and demand evidence from checkers.

```lm
export behavior forwards_upstream_retriable {
    severity: error

    let link = serverless_links[
        Worker.call_setup_webhook,
        Functions.serverless.setup_webhook
    ]

    expect link.exists

    for flag in [true, false] {
        let trace = need replay_setup_error(
            handler: Worker.setup_trigger,
            through: link,
            upstream_retriable: flag
        )

        expect trace.upstream.dispatch == "setup_webhook"
        expect trace.upstream.body.result.error != null
        expect trace.upstream.body.result.retriable == flag

        expect trace.http.status == 200
        expect trace.http.body.error != null
        expect trace.http.body.retriable == trace.upstream.body.result.retriable
            because "the worker passes on the retriable flag the functions service set"
    }
}
```

| Construct | Meaning |
| --- | --- |
| `severity: ...`, `allow_empty: ...` | Fields, allowed only at the start of a behavior's block |
| `let x = e` | An immutable binding |
| `expect e [because "..."]` | An assertion, with the outcomes in section 5 |
| `need e` | Evaluates an `Evidence<T>`. Yields the `T`, or ends the case as unknown when the evidence is unavailable |
| `if`, `for` | Conditionals and finite iteration over lists and relations |

### Cases

- The body outside any `for` is the root case. Each iteration of a `for` is a case nested under the enclosing one.
- A case's identity is the behavior's address plus the pattern values of every enclosing `for`, such as `platform.services.worker::forwards_upstream_retriable[flag=true]`. Renaming unrelated declarations does not change it.
- A violated `expect` ends its case. So does a `need` without evidence, or an `if` on a missing or unknown condition. The rest of that case does not run, and neither do the cases nested in it. Sibling cases still run.
- The initial `expect link.exists` therefore guards the replay cases. Deleting the link makes the behavior violated instead of passing through an empty loop.
- **Outcome:** the worst of its cases, in the order violated, unknown, stale, satisfied. The report lists every case.
- **Vacuity:** a behavior in which no `expect` ran is unknown ("no assertion ran"), unless it sets `allow_empty: true`.
- **Applicability:** a `for` over an incompletely covered relation makes the behavior at best unknown, even when every case it ran held.

A failed `expect` reports its site, the expression's text, the value of each operand, and the `because` text.

### Parameters

```lm
behavior forwards_flag(handler: Operation, link: Fact<Link>, field: Text) {
    // Shared verification procedure.
}
```

A parameterised behavior runs from a check with arguments: `run forwards_flag(handler: ..., link: ..., field: "retriable")`. Each distinct argument set is a separate obligation, and its identity includes the arguments. Behaviors do not call behaviors in v0.2; shared logic goes in functions and parameterised behaviors. A placeholder body runs no assertion, so it is unknown, never evidence that a property holds.

### What a behavior reads

A behavior reads stated facts: graphs written in modules, the relations they make up, and rules over them, evaluated bottom-up stratum by stratum. Facts Lemma reads from code (the standard library's `lemma.http` relations) and provided graphs are not available to behaviors yet, so those relations are incompletely covered: a lookup that finds nothing is unknown rather than false, and a loop over them makes the behavior at best unknown. Iteration over facts is in the order of their values' text.

### Re-running

A behavior runs again on every check; only evidence is kept (section 10). Evaluation is pure and cheap given its facts and evidence, so when nothing a behavior read changed, its evidence keys are equal, its kept evidence is reused, and it reaches the outcome it reached before. Keeping outcomes themselves would only save the evaluation.

## 10. Functions and checkers

### Pure functions

```lm
fn same_boolean(left: Bool, right: Bool): Bool {
    return left == right
}
```

A function body has `let`, `if`, `for` over finite lists, and `return`. There is no `need`, no `expect`, no mutable state and no recursion. The call graph between functions must be acyclic, which is checked at compile time. Recursion over relations belongs in `rule`.

### Checkers

A **checker** produces evidence for a requirement. It is declared with the command that implements it, and nothing runs at import.

```lm
// platform/checks/setup_replay.lm
module platform.checks.setup_replay

import { Link } from lemma.links

export checker replay_setup_error(
    handler: Operation,
    through: Fact<Link>,
    upstream_retriable: Bool
): Evidence<Json> {
    command: ["go", "run", "./tools/setup-replay"]
    directory: "apps/worker"
    setup: ["go", "mod", "download"]
    reads: ["platform", "functions"]
    timeout_ms: 120000
}
```

| Field | Meaning |
| --- | --- |
| `command` | An argument array, run without a shell |
| `directory` | Where it runs, relative to its repository's root |
| `setup` | A command run once per Git tree before the first run |
| `reads` | The repositories whose trees its result depends on. Defaults to the checker's own |
| `env` | Environment variables, part of the evidence key |
| `timeout_ms` | After this the evidence is unavailable |
| `cache` | `false` for a command that depends on anything else, such as a network service. Its evidence is never reused |

**Protocol.** It mirrors [adapters](../docs/adapters.md).
- **Running:** Lemma runs `command` in `directory` of the checker's repository at the snapshot: the clone itself for a working directory, a worktree for a commit. `setup` runs first, once per state of that repository's code. `timeout_ms` defaults to ten minutes, `setup`'s to twenty.
- **Environment:** `env`, plus `LEMMA_INPUT`, `LEMMA_OUTPUT`, `LEMMA_ROOT` (the checkout it runs in) and `LEMMA_CASE` (the demanding case's identity).
- **Inputs:** `LEMMA_INPUT` names a JSON file (below).
- **Output:** the command writes to `LEMMA_OUTPUT` either `{ "observed": <T>, "artifacts": [...], "toolchain": {...}, "coverage": {...} }` or `{ "unavailable": "<reason>" }`.
- **Failure:** a non-zero exit, a timeout, no output, or output that is not a `T` makes the evidence unavailable, with the reason.

The input:

```json
{
  "checker": "platform.checks.setup_replay::replay_setup_error",
  "case": "platform.services.worker::forwards_upstream_retriable[flag=true]",
  "repository": "platform",
  "root": "/checkouts/platform",
  "roots": { "platform": "/checkouts/platform", "functions": "/checkouts/functions" },
  "arguments": {
    "handler": {
      "operation": "platform.services.worker::Worker.setup_trigger",
      "name": "Worker.setup_trigger",
      "client": false,
      "service": "platform.services.worker::Worker",
      "repository": "platform",
      "implementation": { "path": "apps/worker/handlers/triggers.go:SetupTriggerHandler.ServeHTTP",
                          "file": "apps/worker/handlers/triggers.go", "symbol": "SetupTriggerHandler.ServeHTTP" }
    },
    "through": {
      "relation": "lemma.links::Link",
      "values": { "caller": { "operation": "...", "...": "..." }, "provider": { "...": "..." } },
      "exists": true,
      "supports": ["stated in graph platform.services.worker::serverless_links"]
    },
    "upstream_retriable": true
  }
}
```

- An operation is its address, its name in its module, whether it is a client operation, its service, its repository and its `implementation`'s file and symbol (`null` when it has none). A discovered operation is `{ "operation": <key>, "discovered": true, "service", "repository" }`, a service `{ "service", "name", "repository" }`.
- A fact is its relation, its values by column, whether it holds (`null` when unknown) and where it was stated.
- Entities are their fields plus `"entity"`; booleans, integers, text, lists and records are themselves.
- Arguments name repositories, never paths on the machine, since they are part of the evidence's key; `roots` gives each repository's checkout. A file is relative to its repository.

**Evidence** is the value plus its provenance. Its key is a digest of the protocol version, the checker's address and declaration as written, the argument values, the state of the code in each repository of `reads` (its Git tree, plus a digest of each changed file for a working directory), and `env`. The reported toolchain, artifacts and coverage are kept with the evidence but are not part of the key, since they are only known after running. Kept evidence with an equal key is reused without running.

**Where evidence is kept.** Each result is a JSON file named by its key under `evidence/` in Lemma's cache (`$XDG_CACHE_HOME/lemma` or `~/.cache/lemma`), holding the key, checker, case, arguments, the states read and the output. `evidence/cases/` keeps the key each case (a checker and its arguments) last ran with. A checker's own `unavailable` answer is kept; failures are not, so they run again. Nothing in a key names a path, so copying the files carries evidence to another machine; sharing it as [segments](12-segments-and-shared-storage.md) is future work. Files a checker leaves in a working directory change its state, and so its keys, unless Git ignores them. Code outside Git has no state to name, and its evidence is never reused.

**Stale** appears only when running is not allowed, under `lemma check --no-run`. A case whose kept key differs from its current one is stale; one never run is unknown.

The replay checker's contract:
1. Exercise the bound worker handler.
2. Intercept the selected outbound invocation.
3. Inject a response shaped like the functions service's application error.
4. Capture the corresponding public response.

Evidence must correlate the intercepted invocation and the public response to the same case. A checker cannot establish this by echoing its inputs. Any transport the checker exercised is reported in its evidence. This example claims no coverage of other client transports.

## 11. Checks and outcomes

A `check` is a named entry point selecting the requirements to evaluate.

```lm
check main {
    run lemma.links.*
    run lemma.http.*
    skip lemma.http.consumer_sends_removed_parameter
    run platform.services.worker.forwards_upstream_retriable { severity: warning }
}
```

- `run` takes a requirement, written as its module and name joined by `.`, a module's `*` (every requirement in it), or a parameterised behavior with arguments. A trailing record overrides fields such as `severity`. A module that `run` names is loaded. `allow` names its requirement the same way.
- `skip` removes a requirement a `*` selected.
- `lemma check` runs the entry module's `check main`, and `lemma check --select <name>` runs another. Without a `check main`, it runs every requirement of every loaded module and every behavior without parameters, as `lemma verify` runs the whole grammar. `lemma check` always loads `lemma.http`, so its contract checks run unless a `check main` leaves them out: a `check main` that names only behaviors runs no HTTP contract check.
- A `.lm` grammar for `lemma verify` follows the same rules.

| Outcome | Meaning |
| --- | --- |
| `satisfied` | Its assertions hold within the stated evidence scope |
| `violated` | Current evidence contains a counterexample |
| `unknown` | Required evidence, resolution or coverage is unavailable |
| `stale` | Stored evidence no longer matches its inputs, and running was not allowed |

"Not run" is an execution status, not a verification result. Findings covered by an `allow` are reported as `allowed`.

Missing data and missing evidence are different:

```text
A captured response lacks "retriable"
    -> a concrete violation

The replay checker could not capture the response
    -> unknown
```

The workspace's `config.level`, or `lemma check --level`, decides what fails, extending today's levels:

| Level | Error-severity requirement violated | Error-severity requirement unknown |
| --- | --- | --- |
| `audit` | reported | reported |
| `check` | fails (exit 4) | reported |
| `strict` | fails | fails |

**With a change**, only findings the change introduced count. A finding counts when there is no equal finding (same requirement, same message variables) with the base evaluated as the head. Lemma evaluates the base only for requirements that produced findings at the head. It does not run checkers at the base just to compare: the base's program runs on kept evidence only, and a behavior's case that did not hold counts unless the base ended the same case the same way, with the same message. The change report also lists requirements and behaviors that were added, removed or changed between base and head, by address and body, so a deleted requirement shows up instead of disappearing.

Every outcome is tied to a requirement, the input snapshots, the evidence scope and the checker versions. For the example, a satisfied outcome establishes forwarding for the two executed application-error cases. It does not establish that every execution forwards the flag. Whether the functions service classifies upstream failures as retriable is a separate behavior, and so is the worker's handling of a response that omits the flag. A stronger checker, such as static value flow or symbolic execution, can supply evidence under the same behavior, with its scope and assumptions visible.

## 12. Worked example

The platform repository's root module loads its services, and pins the functions repository for when it is checked alone:

```lm
// platform/lemma.lm
module platform

import platform.services.gateway
import platform.services.worker

repo functions = git(url: "https://git.example.com/functions.git", rev: "3f2a1c9e04")
```

The worker module binds its client and its public handler, declares the link, and holds the behavior from section 9:

```lm
// platform/services/worker.lm
module platform.services.worker

import * as go from lemma.go
import { Link } from lemma.links
import { Functions } from functions
import { replay_setup_error } from platform.checks.setup_replay

let client = go.package("apps/worker/core/serverless/generated")
let triggers = go.package("apps/worker/handlers/triggers")

export service Worker {
    index: { include: ["apps/worker"] }

    client operation call_setup_webhook {
        implementation: client.method("SetupWebhook", "Invoke")

        sends: client.type("SetupWebhookRequest")
        accepts: client.type("SetupWebhookResponse")
    }

    export operation setup_trigger {
        implementation: triggers.method("SetupTriggerHandler", "ServeHTTP")

        response: triggers.type("SetupTriggerResponse")
    }
}

export graph serverless_links : Link {
    Worker.call_setup_webhook -> Functions.serverless.setup_webhook
}

export behavior forwards_upstream_retriable {
    // as in section 9
}
```

The public handler is a separate operation because verifying propagation requires observing what the worker returns to its caller. With `example/lemma.lm` (section 2) as the entry module, `lemma check` does the following:
- loads `example`, `functions`, `platform` and its imported services, and `platform.checks.setup_replay`;
- resolves each operation's bindings at the head, and at the base when there is a change;
- checks the link with `lemma.links` and the HTTP consumers with `lemma.http`;
- runs the replay checker for each case it cannot reuse evidence for.

## 13. Lowering and the standard library

The rule layer, links and requirements lower into the structures Lemma evaluates today. Behaviors, checkers and evidence are new, and `lemma check` runs them itself (sections 9 to 11).

| `.lm` | Today |
| --- | --- |
| `repo` in the entry workspace | the parent's `services: "#name": { path \| git, ref }` |
| `repo` at the top of a root module | a repository's external-service pins |
| `service` with `index` and `operations: http.openapi(...)` | `services.<name>.index`, `exports.openapi` |
| `operations: hono.app(...)`, `fastapi.app(...)`, `http.adapter("express-routes", ...)`, `http.adapter_command(...)` | `exports.operations_from`: the shorthands, any built-in adapter by name, or a command |
| a hand-written `operation` | `exports.operations.<name>`, with `path` and `schema` |
| `client: http.generated_client(...)` | `exports.client` |
| `graph g : Link { a -> b }` | `graph.links` |
| `lemma.http.inferred_links(...)` | inferred links |
| `relation`, `rule`, `require` | grammar YAML `relations`, `rules`, `requirements` |
| `@base`, `@head` | the same qualifiers |
| `config.level` | `graph.level` |
| `previously` | an operation's `previously` in `lemma.yaml` |
| `allow` | applied to findings by `lemma check` and `lemma verify` |
| `behavior`, `checker`, `need` | run by `lemma check` |

Standard library:

| Module | Contents |
| --- | --- |
| `lemma.http` | `operation`, parameters, request and response fields, routes, `calls_path`, `calls_client`, `client_operation`, and the syntax facts the rules read (`mentions`, `imports`, `in_file`, `named`); providers `openapi`, `generated_client`, `inferred_links`, `adapter`, `adapter_command`; the rules and requirements of `grammars/http-contracts.yaml` |
| `lemma.links` | `Link`. `lemma check` checks each link for persistence and schema agreement in code of its own, not as requirements, so allowances do not name those checks yet |
| `lemma.hono`, `lemma.fastapi` | Operation providers, today's built-in adapters |
| `lemma.python`, `lemma.typescript`, `lemma.go`, `lemma.rust` | Resolution requests: `module` or `package`, `symbol`, `method`, `type` |
| `lemma.json` | Schema sources: `document`, `command` |

[The standard library's reference](../docs/standard-library.md) lists every module's functions, methods and declarations with their signatures; it is generated from the library, and a test keeps the two equal.

`lemma.yaml` and grammar YAML keep working through the same lowering. `lemma lm convert` writes a `lemma.yaml` as a root module, so moving an existing configuration is mechanical.

How lowering for `lemma check` names things:

- A repository's name is its root module's, so it is an identifier: converting `#payments-client` gives `repo payments_client`, and its external service becomes `#payments_client`.
- A service that only groups services (no operations, providers or client of its own) is a container. Its fields pass down to the services inside it, and only those are lowered, named by their path joined with `/` (`Functions/serverless`).
- `lemma.yaml` says an operation's role only by a link's direction, so conversion writes the caller of a link as a `client operation`, and lowering writes `sends`/`accepts` back as `request`/`response`.
- A link to a discovered operation names it by its `operationId` (`Gateway.listUsers`) or by method and path (`Gateway["GET /users/{id}"]`). The latter lowers to the address `services.Gateway.GET /users/{}`, and `lemma check` finds the operation a service's `OpenAPI` documents declare for that method and path at each snapshot, path parameters compared whatever their names, with or without an `operationId`. Operations an adapter discovers are not linkable yet, by either name.
- `previously: ["functions::Functions.serverless.register_webhook"]` lowers to `previously: [Functions/serverless.register_webhook]` on the operation. When a change's base has no operation by the new name, `lemma check` compares a link with the operation at its old name, so a rename does not make old disagreements look new. It applies to links; renames do not yet carry relation facts.
- The standard library's `lemma.http` keeps the variable names of `grammars/http-contracts.yaml` (`c`, `op`, `f`, `l`), because `lemma check` reads a finding's values by those names. Declaring a finding's roles (`at:` and named roles) would remove that coupling.

## 14. Core grammar

```ebnf
(* Lexical. Comments are whitespace. *)
name          = ( letter | "_" ) { letter | digit | "_" }
              | "`" { character - "`" } "`" ;
lower_name    = ( lowercase_letter | "_" letter_or_digit ) { letter | digit | "_" } ;
upper_name    = uppercase_letter { letter | digit | "_" } ;
module_name   = name { "." name } ;
string        = '"' { character - ( '"' | "\" ) | escape } '"' ;
integer       = [ "-" ] digit { digit } ;
separator     = newline | ";" ;
(* A newline is not a separator inside ( ) or [ ], or after a binary
   operator, ",", ":", "=", "->", ":-" or an opening bracket. A closing
   "}" or the end of the file ends the last item without a separator.
   A line starting with "because" or "else" continues the statement before
   it. Record fields may also be separated by commas. *)

file          = "module" module_name separator { item separator } ;

item          = import_decl
              | reexport_decl
              | workspace_decl
              | repo_decl
              | [ "export" ] declaration ;

import_decl   = "import" module_name
              | "import" "{" import_name { "," import_name } [ "," ] "}" "from" module_name
              | "import" "*" "as" name "from" module_name ;
import_name   = name [ "as" name ] ;
reexport_decl = "export" "{" import_name { "," import_name } [ "," ] "}" "from" module_name ;

workspace_decl   = "workspace" "{" { workspace_member separator } "}" ;
workspace_member = repo_decl | field ;
repo_decl        = "repo" name "=" expression ;

declaration   = binding | type_decl | entity_decl | relation_decl
              | service_decl | graph_decl | rule_decl | require_decl
              | allow_decl | behavior_decl | function_decl
              | checker_decl | check_decl ;

binding       = "let" name [ ":" type_expr ] "=" expression ;
type_decl     = "type" name "=" type_expr ;
entity_decl   = "entity" name "{" { typed_field separator } "}" ;
relation_decl = "relation" name parameters ;

service_decl   = "service" name "{" { service_member separator } "}" ;
service_member = [ "export" ] service_decl
               | [ "export" ] operation_decl
               | binding
               | field ;
operation_decl = [ "client" ] "operation" name "{" { field separator } "}" ;

graph_decl    = "graph" name ":" type_expr
                ( "{" { graph_row separator } "}" | "=" expression ) ;
graph_row     = expression [ "->" expression ]  (* one value for a one-column relation *)
              | "(" expression "," expression { "," expression } ")" ;

rule_decl     = "rule" name ( ":" type_expr | parameters ) "{" { rule_clause separator } "}" ;
rule_clause   = "(" term { "," term } ")" ":-" body ;
body          = literal { "," literal } ;
literal       = [ "!" ] module_name [ snapshot ] "(" [ terms ] ")"
              | term comparator term ;
terms         = term { "," term } | name ":" term { "," name ":" term } ;
term          = lower_name [ "." name ]          (* a variable, or its field in comparisons *)
              | "_"
              | constant ;
constant      = string | integer | "true" | "false" | "null"
              | ( upper_name | "self" ) { "." name | "[" string "]" } ;
snapshot      = "@base" | "@head" ;
comparator    = "==" | "!=" | "<" | "<=" | ">" | ">=" ;

require_decl  = "require" name "{" { field separator } "}" ;    (* when, at, message, severity *)
allow_decl    = "allow" module_name "{" { field separator } "}" ; (* when, reason, until *)
(* In require and allow, the value of "when" is a body, not an expression. *)

behavior_decl = "behavior" name [ parameters ] block ;
function_decl = "fn" name parameters ":" type_expr block ;
checker_decl  = "checker" name parameters ":" type_expr "{" { field separator } "}" ;

check_decl    = "check" name "{" { check_member separator } "}" ;
check_member  = "run" module_name [ "." "*" ] [ call_arguments ] [ record ]
              | "skip" module_name ;

block         = "{" { statement separator } "}" ;
statement     = binding
              | field                           (* only at the start of a behavior *)
              | "expect" expression [ "because" string ]
              | "return" expression
              | if_statement
              | "for" pattern "in" expression block ;
if_statement  = "if" expression block [ "else" ( block | if_statement ) ] ;
pattern       = lower_name | "(" lower_name "," lower_name { "," lower_name } ")" ;
(* A record literal in an "if" or "for" header must be parenthesised. *)

parameters    = "(" [ typed_field { "," typed_field } [ "," ] ] ")" ;
typed_field   = name ":" type_expr ;
field         = name ":" expression ;
type_expr     = name [ "<" type_expr { "," type_expr } ">" ]
              | "{" { typed_field separator } "}" ;

(* Expressions, lowest precedence first. Equality and comparison do not chain. *)
expression     = disjunction ;
disjunction    = conjunction { "||" conjunction } ;
conjunction    = equality { "&&" equality } ;
equality       = comparison [ ( "==" | "!=" ) comparison ] ;
comparison     = additive [ ( "<" | "<=" | ">" | ">=" ) additive ] ;
additive       = multiplicative { ( "+" | "-" ) multiplicative } ;
multiplicative = unary { ( "*" | "/" | "%" ) unary } ;
unary          = ( "!" | "-" | "need" ) unary | postfix ;
postfix        = primary { "." name | snapshot
                         | "[" expression { "," expression } "]"
                         | call_arguments } ;
call_arguments = "(" [ argument { "," argument } [ "," ] ] ")" ;
argument       = [ name ":" ] expression ;  (* positional arguments come first *)
primary        = integer | string | "true" | "false" | "null" | "self"
               | name | list | record | "(" expression ")" ;
list           = "[" [ expression { "," expression } [ "," ] ] "]" ;
record         = "{" { field separator } "}" ;
```

| Precedence, highest first | Operators |
| --- | --- |
| Access, snapshots and calls | `a.b`, `a@base`, `a[b]`, `f(...)` |
| Unary | `!`, `-`, `need` |
| Multiplication | `*`, `/`, `%` |
| Addition | `+`, `-` |
| Comparison | `<`, `<=`, `>`, `>=` |
| Equality | `==`, `!=` |
| Conjunction | `&&` |
| Disjunction | `\|\|` |

The statement grammar is shared, and type and effect checks per context restrict it. A `need` in a function, a field after a behavior's first statement, and a provider call evaluated during import are compile errors, not runtime surprises.

## 15. Implementation boundary and remaining work

Implementation goes in two phases. Each is checked against something that already exists or is fixed in advance.

1. **Lower what exists.** Build the lexer, a lossless parser (comments and layout kept, for a formatter and editor diagnostics), name resolution and type checking for everything except behaviors and checkers. Lower the result into today's manifest and grammar structures. Port `grammars/http-contracts.yaml` to `lemma.http` and the link checks to `lemma.links`. Convert the fixtures' and gate workspaces' `lemma.yaml` files with the converter. *Acceptance:* `lemma check` gives the same findings from `.lm` as from YAML on every gate (verify, mutations, recall, replay, fixtures, scale).
2. **Behaviors and evidence.** Add `behavior`, `need`, `checker`, the evidence store and the four outcomes, with the replay checker as the first implementation. *Acceptance:* the worked example's behavior is violated on a worker that drops the flag, satisfied on one that forwards it, and unknown when the checker cannot run.

**Status, 2026-11-06.** Both phases are implemented.

Phase 1:

- `crates/lemma-lang` parses modules with error recovery, loads programs across repositories, resolves names and type-checks them, and holds the standard library. Its formatter writes a module in the [canonical layout](#layout), keeping every comment, and checks that the result reads as the same tokens.
- `lemma-grammar` lowers a program's relations, rules, graphs and requirements; `lemma.http` compiles to the same program as `grammars/http-contracts.yaml`, which a test requires.
- `lemma-graph` reads `lemma.lm` wherever `lemma check` reads `lemma.yaml`. On five fixture systems and a six-repository workspace, checks from converted modules match the YAML ones line for line, and replaying 54 past changes from converted modules gives the reviewed findings.
- `lemma check` runs a parent module's own requirements beside `lemma.http`'s, as its `check main` (or `--select`ed check) selects them, and a workspace naming a language version other than 0.2 is refused.
- `lemma verify` takes a `.lm` grammar module (`grammar: contracts.lm`), whose `check main` selects requirements.
- `lemma lm check`, `lemma lm fmt` and `lemma lm convert` are on the command line, and `lemma lm lsp` serves modules to editors: diagnostics as you type (one check per burst of edits), hovers, definitions across repositories, symbols, completion and formatting.
- `editors/` has a Tree-sitter grammar with highlighting, locals, folds and indents, a VS Code extension that starts the server, and a Zed extension. `editors/check.sh` parses every `.lm` file in the repository with both parsers, which agree on all of them, and checks each editor for real: VS Code runs the packaged extension against the server under xvfb, headless Neovim and Helix in tmux check captures and diagnostics, and the Zed extension is built for WASM with its queries run by the Tree-sitter CLI. Zed itself has not been run.

Phase 2:

- `lemma-lang` lowers behaviors, functions and checkers to statements with resolved names, and runs them case by case with the outcomes of sections 5 and 9, over stated facts and rules (section 9) and evidence from a caller.
- `lemma check` runs the selected behaviors, runs checkers with the protocol of section 10 and keeps their evidence; `--no-run` uses kept evidence only; `--level strict` fails on unknown error-severity behaviors; a change compares cases with the base's on kept evidence and lists requirements added, removed or changed.
- Allowances are applied by `lemma check` and `lemma verify` (section 8).
- *Acceptance:* `crates/lemma-graph/tests/behaviors.rs` runs the worked example's behavior with a replay checker: violated on a worker that drops the flag, satisfied on one that forwards it, unknown (failing only at `strict`) when the checker cannot run, and stale under `--no-run` once the worker's code changes.

Settled since draft 0.2 left them open:

- Standard-library signatures: [docs/standard-library.md](../docs/standard-library.md), generated from the library.
- The `LEMMA_INPUT` encoding, and where evidence is kept: section 10.
- Language versions: a module declares none; the entry workspace's `language` names the version of the whole program, and a Lemma that does not implement it refuses the program.
- The formatter's layout: [section 1](#layout).

Still open:

- Facts read from code and provided graphs for behaviors, so a behavior can loop over `calls` with complete coverage.
- Sharing evidence between machines as segments, and running checkers for the base when asked.
- Linking operations an adapter discovers, and `previously` for relation facts.
- Finding roles (`at:` with a `Site` from the standard library's relations) in place of the variable names `lemma check` reads.
- Allowances for `lemma check`'s built-in link checks.

## 16. Changes from draft 0.1

- **Examples:** they use a generic platform (`functions`, `worker`, `gateway`) and no organisation's names.
- **Modules and imports:** imports name modules, not files or workspace aliases. A module's name is fixed by its file's place in its repository, and a repository's name is its root module's. Addresses are therefore the same in every workspace (sections 2 and 3).
- **Repository pins:** a root module may pin repositories as defaults, and the entry workspace's pins win, matching external services today.
- **Loading:** every loaded module's facts count, and unloaded `.lm` files are reported.
- **Discovered operations:** `operations:` providers are the main way a service gets its operations. Hand-written operations cover what discovery cannot see, and their schema properties must match their role.
- **Relation names:** a relation's name is the union of its stated graphs. `export` controls naming only, and rules do not feed the union.
- **Snapshots:** `@base` and `@head` are available in rules and lookups, and `previously` keeps renames as one declaration.
- **Rule variables:** lowercase bare names are variables, and constants are uppercase, dotted or literal.
- **Requirements:** `require`, a relational requirement with severity, location and message, sits alongside `behavior`, so change-driven evaluation keeps working. `allow` adds a reason and an expiry dated by commit.
- **Missing values:** missing, `null` and unknown are defined, along with what `expect` and `if` do with each.
- **Cases:** behavior cases have identities, nesting, aggregation and a rule against vacuous passes.
- **Checkers:** `adapter` is renamed `checker`, because an adapter translates a provider's output while a checker produces evidence. A checker is declared with its command, the repositories it reads and its timeout, and runs under the `LEMMA_INPUT`/`LEMMA_OUTPUT` protocol. Evidence keys decide reuse and when `stale` appears.
- **Links:** `Call` is renamed `Link` and `Edge<Call>` is renamed `Fact<Link>`, following the vocabulary: a link connects two operations, and relationships are not called edges.
- **Checks:** `check` gains `skip`, module wildcards, severity overrides and a default, and `audit`, `check` and `strict` decide what fails.
- **Grammar:** the EBNF now covers lexical tokens, rule terms and expressions.
