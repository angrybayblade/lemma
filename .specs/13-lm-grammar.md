# The `.lm` grammar and verification semantics

Captured: 2026-11-05. Status: **proposal**, version 0.1 design draft. This records the language discussion, including the Functions and Worker examples. It does not describe an implemented parser, standard library, replay adapter, or verification capability. No implementation changes accompany this document.

The intended consumer is an AI coding agent. Lemma should give the agent task-specific context, discover technical obligations affected by a change, and return evidence tied to particular input snapshots. The `.lm` surface combines configuration, reusable modules, software declarations, typed relationships, derived graphs, behaviours, and verification entry points.

This builds on [relational grammar and verification](08-relational-grammar-and-verification.md) and [composable manifests](09-composable-manifests.md). `.lm` is the intended extension for this proposal; `.ls` in older notes is historical. The draft carries forward the existing separation between service definitions, graph links, and requirements. It does not replace the current `lemma.yaml` implementation yet.

## Model and execution layers

```text
workspace -> repositories -> modules -> services -> operations
                                            |
                                      typed graphs
                                            |
                                 derived graphs + behaviours
                                            |
                                    verification evidence
```

**`->` constructs a directed relationship. Its meaning comes from the graph's relation type.** Words such as `invokes`, `owns`, and `deploys` remain ordinary identifiers.

| Layer | Purpose | Execution |
| --- | --- | --- |
| Declarations | Config, services, operations, graph edges | Describe inputs and relationships |
| Rules | Derive relationships from existing facts | Pure, finite relational evaluation |
| Behaviours | Check obligations and consume verifier evidence | Controlled execution with recorded dependencies |

## 1. Lexical conventions

Use UTF-8 source files with the `.lm` extension.

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

Proposed conventions:

- Newlines separate declarations, statements, and record fields. Semicolons are optional explicit separators.
- Newlines inside parentheses and brackets are whitespace. A newline following an operator or comma continues the expression.
- Indentation has no meaning.
- Strings use double quotes and JSON-style escapes.
- Identifiers use letters, digits, and underscores, beginning with a letter or underscore.
- Backticks escape identifiers when necessary.
- Structural keywords are contextual. Member names such as `Worker.invokes` or `config.service` remain legal.
- No user-defined operators in v0.1. Organisations extend the vocabulary through types, relations, functions, and modules.

Use `:` consistently for named arguments, field values, parameter types, and return types. Use `=` for bindings.

```lm
let result = resolve(symbol: target)

fn identity(value: Bool): Bool {
    return value
}
```

## 2. Workspaces, repositories, and imports

A workspace establishes repository aliases and execution configuration.

```lm
// workspace.lm
module platform

workspace platform {
    language: "0.1"

    repo functions = local("../functions")
    repo platform = local("../platform")

    config: {
        verification: {
            timeout_ms: 30000
        }
    }
}

import {
    forwards_functions_retriable
} from "@platform/services/worker.lm"

check main {
    run forwards_functions_retriable
}
```

Repository sources can be local or remote.

```lm
repo functions = git(
    url: "https://github.com/example/functions",
    rev: "a-specific-commit"
)
```

The remote example is a placeholder source declaration, not a real repository pin. Every evaluation records the resolved repository contents. A local working tree needs both its commit and the content identities of relevant working-tree changes.

Imports support named exports, namespace imports, and re-exports.

```lm
import { Functions } from "@functions/lemma.lm"

import {
    replay_setup_error as replay
} from "./checks/setup_replay.lm"

import * as python from "lemma/python"

export { Functions } from "./services/serverless.lm"
```

Resolution rules:

- `./` and `../` resolve relative to the importing file.
- `@name` resolves through workspace repository aliases.
- `lemma/` identifies standard-library modules.
- Declarations are private unless exported.
- Imports expose names. They do not create service relationships.
- Cyclic module imports are rejected in v0.1.

## 3. Services, operations, and symbol bindings

A service is a named grouping of operations and nested services. Service containment is a namespace and ownership relationship; it does not imply a runtime call.

A regular `operation` describes provided behaviour. A `client operation` describes an outbound interaction. Both have the common base type `Operation`; the latter also has type `ClientOperation`.

`export` controls visibility separately from the operation's role.

### Functions module

```lm
// functions/lemma.lm
module functions

import * as python from "lemma/python"

let tools = python.module("functions.serverless.task_functions")

export service Functions {
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

Symbol and type references are **resolution requests**. The standard-library providers resolve them against a specific source snapshot. The `python.module`, `symbol`, and `type` APIs shown here are proposed library interfaces.

An operation has a typed property schema:

| Property | Meaning |
| --- | --- |
| `implementation` | Source symbol implementing the operation |
| `dispatch` | Protocol-level operation identifier |
| `request` | Provider request schema source |
| `response` | Provider response schema source |
| `sends` | Client request schema source |
| `accepts` | Client response schema source |

An unresolved or ambiguous symbol remains unresolved. It must not silently bind to a similar name.

An operation's logical identity comes from its exported declaration address, for example:

```text
@functions::Functions.serverless.setup_webhook
```

Its source symbol, file location, and resolved revision are separate bindings. Moving implementation code should not inherently change the operation's identity.

## 4. Relation types and graph instances

A relation declaration defines the meaning and types of its columns.

```lm
// lemma/rpc — proposed standard-library declaration
export relation Call(
    caller: ClientOperation,
    provider: Operation
)
```

A graph is a named collection of facts of one relation type.

```lm
graph serverless_calls : Call {
    Worker.call_setup_webhook -> Functions.serverless.setup_webhook
}
```

For a binary relation, `a -> b` is equivalent to the ordered tuple `(a, b)`.

The arrow establishes neither execution order nor value forwarding. Those properties need their own evidence.

Organisation-specific vocabulary uses the same mechanism:

```lm
export entity Deployment {
    environment: Text
    region: Text
}

export relation Deployed(
    service: Service,
    deployment: Deployment
)

let production = Deployment(
    environment: "production",
    region: "us-east-1"
)

graph deployments : Deployed {
    Worker -> production
}
```

Relations with more than two columns use tuples:

```lm
relation Routes(
    gateway: Service,
    path: Text,
    destination: Operation
)

graph routes : Routes {
    (Gateway, "/monitors/setup", Worker.setup_monitor)
}
```

`Gateway` and the route in this last fragment are illustrative. They are not declarations of Platform's actual route topology.

Graph semantics:

- Facts have set semantics. Duplicate tuples share one logical fact.
- Each supporting declaration or observation retains its own provenance.
- Entity references use logical identities, not display names.
- Source order does not affect graph meaning.
- Declared edges carry `declared` provenance.
- A declared edge alone does not establish that runtime routing matches it.

Graph sources may also be supplied by providers:

```lm
graph discovered_calls : Call = discovery.calls(
    services: [Worker, Functions.serverless]
)
```

This expression builds a discovery request. Its resulting graph carries coverage information, including what remains unresolved. `discovery` represents an imported provider namespace in this fragment.

## 5. Derived graphs

Use `rule` for pure relational derivation. A rule produces a graph with a declared relation type.

```lm
relation DependsOn(
    source: Service,
    target: Service
)

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

Here:

- `:-` means "derive the tuple on the left when the conditions on the right hold."
- Commas mean conjunction.
- Multiple clauses mean alternative derivations.
- Variables are scoped to one clause.
- `!predicate(...)` expresses negation.

For v0.1, impose Datalog-style restrictions:

- Every output variable must be bound by a positive input predicate.
- Variables used in negation must already be bound.
- Recursive rules may derive new tuples over existing values, but cannot create new entities or unbounded values.
- Negation must be stratified.
- Rules cannot execute commands, access the network, or invoke effectful adapters.

This gives Lemma a predictable fixed-point computation and a tractable dependency graph for incremental evaluation. See also [change-driven evaluation](11-change-driven-evaluation.md) for the current engine's approach to selecting relevant facts.

An absent fact is usable as negative evidence only when the relevant query scope is complete.

## 6. Worker module and the cross-service graph

The Worker module imports Functions' exported operation, binds its own client and public handler, and declares the relationship.

```lm
// platform/services/worker.lm
module platform.worker

import * as go from "lemma/go"

import { Call } from "lemma/rpc"
import { Functions } from "@functions/lemma.lm"

import {
    replay_setup_error
} from "./checks/setup_replay.lm"

let client = go.package(
    "apps/worker/core/serverless/generated"
)

let monitors = go.package(
    "apps/worker/handlers/monitors"
)

export service Worker {
    client operation call_setup_webhook {
        implementation: client.method("SetupWebhook", "Invoke")

        sends: client.type("SetupWebhookRequest")
        accepts: client.type("SetupWebhookResponse")
    }

    export operation setup_monitor {
        implementation: monitors.method(
            "SetupMonitorHandler",
            "ServeHTTP"
        )

        response: monitors.type("SetupMonitorResponse")
    }
}

export graph serverless_calls : Call {
    Worker.call_setup_webhook -> Functions.serverless.setup_webhook
}
```

The graph connects the client operation to the provider. The public handler is a separate operation because verifying propagation requires observing what Worker returns to its caller. The `go.package`, `method`, and `type` APIs are proposed library interfaces.

## 7. Behaviours and assertions

A `behavior` defines an obligation. It can query graphs, iterate over cases, call pure functions, and request evidence.

The following declaration belongs in the same Worker module as the service and graph above.

```lm
export behavior forwards_functions_retriable {
    let edge = serverless_calls[
        Worker.call_setup_webhook,
        Functions.serverless.setup_webhook
    ]

    expect edge.exists

    for flag in [true, false] {
        let trace = need replay_setup_error(
            handler: Worker.setup_monitor,
            through: edge,
            functions_retriable: flag
        )

        expect trace.upstream.dispatch == "setup_webhook"

        expect trace.upstream.body.result.has("error")
        expect trace.upstream.body.result.error != null

        expect trace.upstream.body.result.has("retriable")
        expect trace.upstream.body.result.retriable == flag

        expect trace.http.status == 200

        expect trace.http.body.has("error")
        expect trace.http.body.error != null

        expect trace.http.body.has("retriable")
        expect trace.http.body.retriable ==
            trace.upstream.body.result.retriable
    }
}
```

The replay adapter's contract is to exercise the bound Worker handler, intercept the selected outbound invocation, inject a Functions-shaped application-error response, and capture the corresponding public response. Evidence must correlate the intercepted invocation and public response to the same replay case. An adapter cannot establish this by merely echoing its requested inputs.

The assertions verify the actual raw payloads. Missing `retriable` is distinct from `false`. The injected application error is in `result.error`, rather than a transport-level failure. Any transport exercised by the runner must be reported in its evidence; this example does not claim coverage of all client transports.

Two language constructs matter here:

| Construct | Meaning |
| --- | --- |
| `expect condition` | Assert an obligation against available evidence |
| `need expression` | Obtain the value from an evidence-producing computation |

A false assertion ends the current case with a violation. Missing evidence stops dependent work with an unresolved outcome. Other independent cases may continue. A failure of the outer `expect edge.exists` stops the dependent replay cases.

The initial `expect edge.exists` prevents deleting the link from making this behaviour pass through an empty iteration. Graph lookup must preserve the requested endpoints even when the edge is absent, so the assertion can report that absence. An incomplete discovered graph may yield an unknown existence result.

Behaviours can also be parameterised for reuse:

```lm
behavior forwards_boolean(
    handler: Operation,
    edge: Edge<Call>,
    field: Text
) {
    // Shared verification procedure.
}
```

This is a signature sketch, not a complete verifier. Empty or placeholder bodies must not be treated as evidence that a forwarding property holds.

## 8. Procedural functions and adapters

Keep procedural authoring small. Pure functions support immutable bindings, conditionals, finite loops, and returns.

```lm
fn same_boolean(left: Bool, right: Bool): Bool {
    return left == right
}
```

For v0.1:

- No mutable global state.
- No unrestricted `while`.
- No recursive procedural functions.
- No implicit filesystem, clock, environment, or network access.
- Relational recursion remains available through `rule`.

External execution goes through typed adapters.

```lm
// platform/services/checks/setup_replay.lm
module platform.checks.setup_replay

import { Call } from "lemma/rpc"

export adapter replay_setup_error(
    handler: Operation,
    through: Edge<Call>,
    functions_retriable: Bool
): Evidence<Json> =
    runner("platform.setup-replay/v1")
```

`runner(...)` names a registered, versioned implementation. It could be implemented in Go, Python, TypeScript, Rust, or a command using Lemma's adapter protocol. This runner name is a proposed interface, not an existing implementation created by this specification.

The adapter boundary must record:

- Input snapshots and configuration.
- Runner implementation and toolchain identity.
- Files, queries, and external inputs read.
- Produced artifacts and trace correlation.
- Coverage and any unresolved dependencies.

Repository commands should be represented as argument arrays, without implicit shell interpolation.

Parsing or importing a module must not execute an adapter. Execution occurs when verification demands its evidence. Declaration-time provider calls construct resolution or discovery requests rather than executing repository code as an import side effect.

## 9. Verification outcomes

A behaviour's report needs more information than a Boolean.

| Outcome | Meaning |
| --- | --- |
| `satisfied` | Its required assertions hold within the stated evidence scope |
| `violated` | Current evidence contains a counterexample |
| `unknown` | Required evidence, resolution, or coverage is unavailable |
| `stale` | Previously obtained evidence does not meet current validity requirements |

"Not run" is an execution status, not a successful verification result.

Distinguish missing data from missing evidence:

```text
A captured response lacks "retriable"
    -> a concrete violation

The replay runner could not capture the response
    -> unknown
```

For the Functions/Worker example, a successful result establishes forwarding for the two executed application-error cases. It does not establish that every possible execution forwards the flag. Functions' classification of upstream failures as retriable is a separate behaviour. Compatibility with a Functions response that omits the flag is also a separate obligation.

A stronger verifier could supply static value-flow or symbolic-execution evidence under the same behaviour interface. Its scope and assumptions must remain visible.

A check passes only when all required applicable obligations are satisfied and applicability discovery is complete. Changed or removed obligations must appear in the change report rather than disappear silently.

Every successful verification remains tied to a specific obligation, input snapshot, evidence scope, and verifier version. Evidence reuse must respect those dependencies, including negative query dependencies and coverage.

## 10. Core grammar

The following EBNF captures declaration and statement structure. Conventional literal and expression productions are abbreviated. This is not a complete parser grammar: `name`, `path`, `string`, `separator`, `expression`, `type_expr`, `arguments`, `term`, and `comparison` still require full lexical or expression productions before implementation.

```ebnf
file =
    "module" path separator
    { item separator } ;

item =
      import_decl
    | reexport_decl
    | workspace_decl
    | [ "export" ] declaration ;

declaration =
      binding
    | type_decl
    | entity_decl
    | relation_decl
    | service_decl
    | graph_decl
    | rule_decl
    | function_decl
    | adapter_decl
    | behavior_decl
    | check_decl ;

import_decl =
    "import" import_selection "from" string ;

import_selection =
      "{" import_name { "," import_name } [ "," ] "}"
    | "*" "as" name ;

import_name =
    name [ "as" name ] ;

reexport_decl =
    "export" "{" import_name { "," import_name } [ "," ] "}"
    "from" string ;

workspace_decl =
    "workspace" name "{"
        { workspace_member separator }
    "}" ;

workspace_member =
      "repo" name "=" expression
    | field ;

binding =
    "let" name [ ":" type_expr ] "=" expression ;

type_decl =
    "type" name "=" type_expr ;

entity_decl =
    "entity" name "{"
        { typed_field separator }
    "}" ;

relation_decl =
    "relation" name parameters ;

service_decl =
    "service" name "{"
        { service_member separator }
    "}" ;

service_member =
      [ "export" ] service_decl
    | [ "export" ] operation_decl
    | binding
    | field ;

operation_decl =
    [ "client" ] "operation" name "{"
        { field separator }
    "}" ;

graph_decl =
    "graph" name ":" type_expr
    (
        "{" { graph_row separator } "}"
        | "=" expression
    ) ;

graph_row =
      expression "->" expression
    | "(" expression "," expression
        { "," expression } ")" ;

rule_decl =
    "rule" name ":" type_expr "{"
        { rule_clause separator }
    "}" ;

rule_clause =
    "(" term { "," term } ")"
    ":-" rule_literal { "," rule_literal } ;

rule_literal =
      [ "!" ] path "(" [ term { "," term } ] ")"
    | comparison ;

function_decl =
    "fn" name parameters ":" type_expr block ;

adapter_decl =
    "adapter" name parameters ":" type_expr
    "=" expression ;

behavior_decl =
    "behavior" name [ parameters ] block ;

check_decl =
    "check" name "{"
        { "run" path [ arguments ] separator }
    "}" ;

block =
    "{" { statement separator } "}" ;

statement =
      binding
    | "expect" expression
    | "return" expression
    | "if" expression block [ "else" block ]
    | "for" pattern "in" expression block ;

pattern =
      name
    | "(" name "," name { "," name } ")" ;

parameters =
    "(" [ typed_field { "," typed_field } [ "," ] ] ")" ;

typed_field =
    name ":" type_expr ;

field =
    name ":" expression ;
```

Expressions include literals, lists, records, references, member access, indexing, function calls, and `need`.

Operator precedence, highest first:

| Precedence | Operators |
| --- | --- |
| Access and calls | `a.b`, `a[b]`, `f(...)` |
| Unary | `!`, `-`, `need` |
| Multiplication | `*`, `/`, `%` |
| Addition | `+`, `-` |
| Comparison | `<`, `<=`, `>`, `>=` |
| Equality | `==`, `!=` |
| Conjunction | `&&` |
| Disjunction | `\|\|` |

Equality is type-strict. There is no implicit conversion between `false`, `0`, `"false"`, `null`, and a missing field.

`->` is restricted to graph rows. `:-` is restricted to rule clauses. This keeps relationship construction and logical derivation unambiguous.

The shared statement grammar is subject to context-specific type and effect checks. Parsing a statement does not grant permission to use an evidence effect in a pure function or execute a provider while importing a module.

## 11. Implementation boundary and remaining specification work

The proposed first implementation prioritises modules, operation bindings, typed graphs, behaviours, adapters, and evidence-aware outcomes. The relational rule layer can lower into Lemma's existing evaluation machinery. This document authorises no implementation work or YAML migration.

Before treating this as a parser-ready language definition, specify:

- Full lexical, expression, type, and argument productions, including separators at closing delimiters and end of file.
- The exact type of graph lookup results, tuple iteration, and coverage propagation.
- The built-in type hierarchy and property schemas for workspaces, services, operations, and provider requests.
- Stable identities for user-defined entities and behaviour cases, including declaration renames.
- How rule variables are distinguished from bound constants and how relational comparisons are typed.
- Runner registration, evidence serialization, input tracking, and freshness rules.
- Aggregation of per-case outcomes, empty-domain obligations, and reporting of removed obligations.

These are refinements of the recorded design. The Functions and Worker code blocks are proposed `.lm` modules, not executable files added to either repository, and no replay verification was run when saving this draft.
