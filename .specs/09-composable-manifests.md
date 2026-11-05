# Composable manifests, links and schema checks

Captured: 2026-11-02. Status: **established direction** where marked (decided in discussion, 2026-11-02); syntax shown is **proposal** unless stated.

This replaces the single cross-repository manifest of [document 8](08-relational-grammar-and-verification.md) (repositories with roles `service`, `client`, `consumer`) with composable `lemma.yaml` files. Each repository describes what it is; a parent composes repositories; relationships live in a `graph` section; schemas on both ends of a relationship are resolved to one format and compared. It came out of trying to trace a breaking change in the generic platform's Functions through Worker and Gateway to the dashboard and SDKs, which the rc0 model could not express, and out of removing rc0's generic platform-specific assumptions (PR #5).

## Principles

- **Established direction:** Lemma is a general framework. Nothing about a particular company's layout, generators or naming goes into Lemma's code; system-specific knowledge goes into that system's `lemma.yaml` files and grammar.
- **Established direction:** inference is the default. Users declare only what inference cannot see (the generic platform's Functions → Worker Lambda calls are the motivating case).
- **Established direction:** Lemma does not define CI. Users run checks themselves, typically from a workspace repository like `platform-workspace`.

## Files

**Established direction:** one file per repository, `lemma.yaml`, with two parts:

- `services`: what the repository is: its services, their operations, and where each operation's schemas come from. Definitions only.
- `graph`: relationships (links), linking patterns and requirements (the grammar), and the check level.

A parent `lemma.yaml` (for example in `platform-workspace`) has the same shape: it declares the repositories it composes as external services and holds the `graph` for what spans them. Lemma merges the `graph` sections it reads.

**Established direction:** relationships are not declared inside `services`; they go in `graph`. A separate `graph.lemma.yaml` was considered and rejected to avoid extra work for users.

**Established direction (later):** a custom `.ls` format with proper types and LSP support will subsume both definitions and relations.

## Names and external services

**Established direction:**

- `functions` names a service in the same repository.
- `#functions` names an external service. External services are declared in `services` with a leading `#`; no separate `external` section.
- External services are pinned. Several pin forms are allowed; the preferred one is a Git commit, with `HEAD` meaning the default branch's latest commit.
- Sources: a local directory or a remote Git repository.

```yaml
services:
  "#functions":
    git: git@github.com:example/functions.git
    ref: HEAD                # a commit preferred; branch, tag also allowed
  "#dashboard":
    path: ../dashboard       # local directory
```

**Proposal:** Lemma reports the commit each `#service` resolved to, so a run with `ref: HEAD` can be reproduced.

## Addresses

**Proposal:** an address names a node in a `lemma.yaml` tree, optionally in another repository:

```text
services.worker.submit_task                       # in this repository
#functions:services.serverless.submit_task           # in the external service's lemma.yaml
```

Code anchors name a symbol in a file: `path/to/file.py:submit_task` for a callable, `path/to/file.go#SubmitTaskRequest` for a type. Exact anchor syntax is open (see open questions).

## Operations and schemas

**Established direction:** an operation declares where it lives in code and where its request and response schemas come from. Schemas can come from:

- a **resolver**: a command whose output holds the schema, located by a path (JSON path), resolved lazily;
- **spec references**: an OpenAPI document and `$ref`s;
- **code references**: an anchor to a type in code.

```yaml
# functions/lemma.yaml (definitions only)
services:
  serverless:
    operations:
      submit_task:
        path: functions/serverless/task_functions.py:submit_task
        schema:
          resolver: make serverless
          request:  { format: json-schema, path: "$.submit_task.request" }
          response: { format: json-schema, path: "$.submit_task.response" }
```

```yaml
# platform/lemma.yaml (definitions only)
services:
  worker:
    operations:
      submit_task:
        path: apps/worker/core/serverless/generated/submit_task.go:SubmitTask.Invoke
        schema:
          request:  apps/worker/core/serverless/generated/submit_task.go#SubmitTaskRequest
          response: apps/worker/core/serverless/generated/submit_task.go#SubmitTaskResponse
  "#functions": { git: git@github.com:example/functions.git, ref: HEAD }

graph:
  links:
    - services.worker.submit_task -> "#functions:services.serverless.submit_task"
  level: audit
```

**Observed (2026-11-02):** Worker's generated wrapper already carries Functions' function name verbatim (`InvokeWithHttp(ctx, "submit_task", …)`, `Op: "submit_task"` in `core/serverless/generated/submit_task.go`), and Functions registers the function under its class name. No name transformation is needed to relate them.

## Links

**Established direction:** only operations are linked. A link is operation → operation. Each operation's schemas are resolved its own way; once linked, Lemma compares them. Schema-to-type links were considered and rejected: schemas belong to operations.

**Established direction:** inferred links are the default (what `lemma verify` does today: paths in code, calls through generated clients). Declared links are for what inference cannot make.

**Established direction:** Lemma does not detect "inference failed here" on its own for now. Instead, an **agent skill audits** `lemma.yaml` files against the code: it lists boundary calls that resolve to nothing (generated-client objects and modules, HTTP and RPC libraries such as Lambda `Invoke`, URLs built from environment variables, queue publishers), declarations that no longer match (dangling anchors, services without links, failing resolvers), and proposes `lemma.yaml` changes as a diff.

**Established direction:** a parent's grammar holds **requirements and linking patterns** (rules that create links by pattern, so many links need not be declared one by one). Patterns were discussed with capture syntax (Tree-sitter queries or ast-grep-style code patterns); since operation links plus per-operation schemas cover the motivating case, patterns are deferred until declared links become numerous.

## Checks

**Established direction:** incremental, cheapest first; stop at the first failure:

1. **The link persists.** Both operations exist at base and head; their anchors resolve.
2. **The schemas agree.** Both sides' request and response schemas, resolved to the common format, are compatible in the direction of travel.

**Established direction:** the user picks the **level**: `audit` reports differences as warnings; `check` fails on them.

**Established direction (inferred links):** for now, an inferred link checks persistence, the provider's schema at base versus head, and the caller's field-name usage near the call, as rc0 does. Later: infer the caller's schema at the call site (a zod `.parse`, a typed response, a TypeScript annotation, LSP hover for inferred types) and compare it like a declared link.

## Common schema format

**Established direction:** both sides resolve to **JSON Schema**, the interchange into a comparable shape (fields with wire names, required, nullable, types, nesting, unknown-field policy).

**Established direction:** resolution chain, cheapest sufficient source first:

1. **Language resolvers** (exact, use the language's own rules: aliases, alias generators, inheritance, generics):

| Language / library | Resolver |
| --- | --- |
| Pydantic | `Model.model_json_schema()` |
| `TypedDict`, dataclasses, plain types | `pydantic.TypeAdapter(T).json_schema()` |
| zod | `z.toJSONSchema()` (zod 4), `zod-to-json-schema` (zod 3) |
| TypeScript interfaces and types | `ts-json-schema-generator` / `typescript-json-schema` (TypeScript compiler, not run) |
| Go structs | Tree-sitter, by `encoding/json`'s rules (implemented; invopop dropped: it does not mark pointers, slices and maps nullable). Two views: **encode** (sender: fields without `omitempty` required; pointers, slices, maps nullable) and **decode** (receiver: nothing required, every field accepts null) |
| Rust (serde) | `schemars` where types derive `JsonSchema`; `serde-reflection` tracing otherwise |

2. **Tree-sitter**: reads the anchored declaration syntactically.
3. **LSP**: when Tree-sitter cannot complete a type (defined elsewhere, re-exported, inherited, generic, inferred), go-to-definition locates the declaration for Tree-sitter to read, or hover gives a type expression parsed with the same grammar. Results cached incrementally by the content of the files involved.

Unresolvable references stay opaque and are reported as unresolved, never guessed.

**Established direction (environments):** resolvers use **existing environments**. Users give interpreter paths per service (Python, Node); Go and Rust use the default toolchains (`go`, `cargo`) unless configured. When a resolver cannot run, the entity falls back to Tree-sitter and the report says which source answered.

```yaml
services:
  serverless:
    runtime: { python: .venv/bin/python }
```

**Established direction (base vs head):** a Git worktree per commit, imported or compiled with the configured environment. Other strategies (like-for-like fallback, cached base schemas) to be compared later on UX and performance.

## Comparison

**Established direction:** use an existing Rust tool: [`json-schema-diff`](https://github.com/getsentry/json-schema-diff) (Sentry; draft-07, best-effort, used in Sentry's Kafka-schema CI). It reports atomic changes (path, kind) with an advisory `is_breaking`.

**Implemented (corrects the earlier casting):** the tool calls a change breaking when the right-hand side is stricter, so a link is compared as `compare(sender, receiver)`: left = what is sent, right = what receives it.

- request: sender = caller, receiver = provider;
- response: sender = provider, receiver = caller, with **expected fields** on (a field the caller reads that the provider does not send is breaking).

Lemma overrides the tool's verdicts, which assume two versions of one schema: `PropertyAdd` is breaking only with expected fields on; `PropertyRemove` is never breaking; a separate walk reports sender fields a strict receiver (`additionalProperties: false`) rejects.

**Implemented (reporting):** with a change given (`--change`, `--base`, `--head`), only differences absent at base are problems (warning at `audit`, error at `check`, exit 4); differences already there are info. An anchor failure is introduced only if the anchor resolved at base. Without a change, every difference is a problem. A commit without a `lemma.yaml` falls back to the working directory's (reported), so history predating the file can be replayed.

**Measured (2026-11-02, Functions → Worker, 13 declared links, ~28 s per run):** `3ee5e0b07bd`: 0 problems, 3 additive changes as info; `679c8719ea0` at `check`: exit 0; a synthetic breaking branch: exactly 4 introduced errors (new required request field, response field type change, expected response field dropped, Worker anchor gone), exit 4. About 110 pre-existing differences, mostly Go nil slices and maps against non-nullable Python dicts.

## Entry point

**Established direction:** checks start from a parent `lemma.yaml` (the workspace), which names the change (repository and commits). Lemma finds dependents by reading every repository the parent composes.

## Worked example: generic platform

```text
platform-workspace/lemma.yaml          services: #platform, #functions, #dashboard, #sdk, #platform-client  (pins)
                                       graph: cross-repository links, patterns, requirements, level
platform/lemma.yaml                    services: gateway, worker (+ operations, schemas); #functions
                                       graph: worker.<fn> -> #functions:services.serverless.<fn>   (14 Lambda functions)
functions/lemma.yaml                     services: serverless (14 operations; schema resolver: make serverless)
dashboard/, sdk/ …                services/operations as needed; calls into Gateway inferred
```

Goal: a breaking change to a Functions Lambda function's request or response is reported against Worker (declared link, schema comparison), and, once transitive tracing exists (open), on through Gateway to the dashboard and SDKs.

Acceptance cases to replay: Functions `3ee5e0b07bd` (setup returns secrets, passed to refresh) and `679c8719ea0` (setup reports whether its failure is retriable), each checked against Worker at the same time; plus the rc0 scenarios in `.test/platform-workspace/lemma/scenarios`.

## Relation to rc0's manifest

| rc0 | This design |
| --- | --- |
| One manifest listing repositories with roles | A `lemma.yaml` per repository plus a parent `lemma.yaml` |
| `role: service` + `specs` / `routes` | `services.<name>.operations` with schema sources; specs and routes as sources |
| `role: client` + `generated_from` + `call_names` | Unchanged for inference; declared operations with code-referenced schemas where needed |
| `role: consumer` + `include` | Inference over code; consumer operations declared only when linking manually |
| `change` in the manifest, `--base` / `--head` | The parent names the change |
| Grammar file referenced by `grammar:` | `graph` section (links, patterns, requirements, level) |

## Open questions

Raised in discussion, not yet decided:

- **Transitive tracing** (gap C in the 2026-11-02 discussion): a Functions change leaves Worker's and Gateway's specs untouched; reaching the dashboard and SDKs needs field lineage across hops. Options discussed: declared lineage facts in the grammar, inferred pass-through by name (as suggestions), observed lineage (traces). Recommendation was declared lineage with inference as a suggestion engine.
- **SDK call → client method → operation** (gap B): resource-style SDK calls (`client.sessionOptions.list()`) cannot be named from `operationId`s; options: treat the generated client as a consumer and match SDK chains to client symbols by name, resolve chains with LSP go-to-definition, or a generator mapping file. rc1 lost this (the generic platform's inferred calls 100 → 8 operations).
- **Merging `graph` sections**: precedence when a repository's and a parent's `graph` declare overlapping links or requirements; whether a parent may override a repository's pins.
- **Address and anchor syntax**: `file:symbol` versus `file#Type`; nested members; anchors into resolver output.
- **Wire names under Tree-sitter fallback**: language resolvers apply naming conventions; when falling back to Tree-sitter, per-type conventions (`rename_all`, `alias_generator`) and `lemma.yaml` overrides were discussed (type conventions overridable by `lemma.yaml`), not decided.
- **Rust types without `JsonSchema`**: repository opt-in (`cfg_attr` derive), `serde-reflection`, or Tree-sitter.
- **Resolver trust and cost**: resolvers execute repository code; worktree and process per commit and language.
- **Changes spanning repositories** (a service PR with a companion consumer PR): how one check sees both.
- **Per-link level overrides** (`audit` for one link, `check` for another).
- **Non-HTTP inference**: queues (`openapi-queues.json`), webhooks (`openapi-webhooks.json`), shared packages, databases; all expressible as operations with schemas, but inference for them does not exist.

## Implementation status (2026-11-02, branch `feat/composable-manifests`)

Decided in review and built:

- **Inferred links: infer everything.** A service lists `openapi` (plus `routes`, `path_prefixes`, `route_aliases`, `shared_schemas`, `rejects_unknown_fields`); calling code is `code` (whole repository by default only for a service alone in its repository that declares nothing else); generated clients are `client: { generated_from, call_names, specs }` with `client_receivers` / `client_modules` on the calling code. Bridged to `lemma verify` by building its manifest in memory, one entry per part a service plays. Documented operations with an `operationId` become addressable operations. On rc0's 7 scenarios: 6 identical findings; `sync-auth-early` differs because one repository has one pin (open question below).
- **Types: Tree-sitter first** for Go, TypeScript (relative imports followed) and Rust (serde rules, encode/decode views); Python by Pydantic with a Tree-sitter fallback. Validated against Pydantic on Functions' TypedDicts.
- **Transitive tracing: impact by call graph**, by name. Seeds: a declared link's caller anchor (a method's type), or an inferred finding's calling function. Walks callers up to route handlers (registration lines naming a reached function, or the function around the route line, including a file's top-level code), then to inferred callers in other repositories. Names defined by more than 3 functions are not followed; a type's own methods are not its users. Reported as `affects` (distance, owner, at, calls), never as a problem. Functions `submit_task` break → Worker `POST /api/tasks_/{}/execute` → 54 Gateway operations → dashboard (24), SDK (4), Functions scripts (6) callers. Misses routes rc0 cannot extract (paths built at run time, e.g. Gateway `v3/tools/execute`).
- **Validation:** `#name` services give `git`/`path`/`manifest` and define nothing; local services name no source.
- **Vocabulary:** `link`, `operation`, `external service` added to CONTRIBUTING's glossary.
- **Grammar fix:** a service's own unserved paths are not unserved calls.

Still open: per-service pins within one repository (deploy skew); LSP precision for schemas and the call graph; SDK resource chains (gap B); merging `graph` sections; linking patterns.
