# Checking links between repositories

`lemma check` checks a change in one repository against every repository that depends on it, starting from composable `lemma.yaml` files. Each repository describes itself in its own `lemma.yaml`; a parent `lemma.yaml`, usually in a workspace repository, composes them. Lemma finds the operations that depend on each other, compares the schemas on both sides of each link, and traces what a broken link affects further up.

It complements `lemma verify` (see [Verifying changes](verify.md)), whose manifest lists repositories with roles. `lemma check` reuses `lemma verify`'s extraction and grammar for calls it infers from code, and adds links Lemma cannot infer, schema comparison for any language, and tracing.

## Example

A billing service's function takes a request and returns a response. A web service calls it through a Go client. The billing team adds a required `currency` field and stops returning `receipt_url`.

```text
$ lemma check lemma.yaml --change '#billing' --base main --head HEAD
1 links, 4 inferred, level check
  #billing       3f2a1c9e04..8b7d6e5f21
  #web           working directory
  self           working directory
error   request: the receiver requires `currency` at the root; the sender may leave it out
        [request] #web:services.web.create_charge -> #billing:services.api.create_charge  (introduced)
        affects 1 operation and 2 callers (traced by name):
          reached directly:
            #web:web: 1 operation
              POST /v1/checkout
                through src/checkout.go:Checkout -> src/charges.go:Charges.Create
          one call away:
            #storefront: 2 callers of 1 operation
              src/cart.ts::Cart.pay -> POST /v1/checkout
              src/quick-buy.ts::buy -> POST /v1/checkout
error   response: the receiver expects `receipt_url` at the root; the sender does not send it
        [response] #web:services.web.create_charge -> #billing:services.api.create_charge  (introduced)
2 errors, 0 warnings, 31 informational (--all lists them)
```

## Files

Every repository has a `lemma.yaml` at its root. It has two sections:

- `services`: what the repository is. Each service says what it is made of (`index`: the code Lemma reads for it) and what it offers (`exports`: OpenAPI documents, operations and public modules, the entry points queries and links start from), and may be a generated client. A name starting with `#` is an **external service**: another repository's `lemma.yaml`.
- `graph`: how it relates to others. `links` between operations that Lemma cannot infer, and the `level` of checking.

```yaml
# billing/lemma.yaml
services:
  api:
    runtime: { python: .venv/bin/python }
    index:
      include: [billing]
    exports:
      openapi: [openapi.json]               # calls to these operations are inferred
      operations:
        create_charge:                      # one Lemma cannot infer: a queue message handler
          path: billing/handlers.py:create_charge
          schema:
            request: billing/handlers.py#CreateChargeRequest
            response: billing/handlers.py#CreateChargeResponse
```

```yaml
# web/lemma.yaml
services:
  web:
    index:
      include: [src, cmd, client]
      exclude: ["**/*_test.go"]
    exports:
      operations:
        create_charge:
          path: client/charges.go:Charges.Create
          schema:
            request: client/charges.go#CreateChargeRequest
            response: client/charges.go#CreateChargeResponse
  "#billing":
    git: git@github.com:example/billing.git
    ref: 3f2a1c9e04                         # a commit, branch or tag
graph:
  links:
    - services.web.create_charge -> "#billing:services.api.create_charge"
```

```yaml
# workspace/lemma.yaml: the entry point
services:
  "#billing": { path: ../billing }
  "#web": { path: ../web }
  "#storefront": { path: ../storefront }
graph:
  level: check
```

Lemma reads the parent, then every repository its external services name, and theirs in turn. The first declaration of a name wins, so the parent's pins override a repository's.

### External services

| Field | Meaning |
| --- | --- |
| `path` | A local clone, relative to this file. Read from its working directory unless `ref` is given. |
| `git` | A repository to clone into Lemma's cache (`~/.cache/lemma/repos`). Read at its default branch unless `ref` is given. |
| `ref` | The commit, branch or tag to read. A commit is preferred: it keeps checks reproducible. |
| `manifest` | Where its `lemma.yaml` is, relative to its root. Alone, without `path` or `git`, it names another `lemma.yaml` in this repository, as in a monorepo. |

An external service defines nothing; its operations are in its own `lemma.yaml`. A local service names no source. Lemma rejects a file that mixes the two.

### Operations

An operation is something a service provides or calls, with a request and a response. Each one names where it is in code (`path: file:Symbol`, which must resolve) and where its schemas come from:

| Schema source | Example | Read by |
| --- | --- | --- |
| A type in code | `request: client/charges.go#CreateChargeRequest` | Tree-sitter for Go, TypeScript and Rust; Pydantic for Python, falling back to Tree-sitter |
| A command's output | `resolver: { command: "{python} scripts/spec.py" }` and `request: { path: "$.create_charge.request" }` | The command, run in the repository at the commit; the location is a JSON path into its output |
| A document | `request: { file: spec.json, path: "$.create_charge.request" }` | The file; `format: openapi` converts OpenAPI 3.0's `nullable` |

A command's output, and a Python type read with Pydantic, are kept in Lemma's cache for the commit they were produced at, by the code's Git tree, the command and its environment, the interpreter and its installed packages when Python runs, and `PATH`, `VIRTUAL_ENV` and `PYTHONPATH`; a commit not checked out runs in one worktree Lemma reuses per repository. Set `cache: false` on a resolver whose output depends on anything else, such as a running service or files Git ignores.

`{python}`, `{node}`, `{go}` and `{cargo}` in a command are the service's `runtime`, which defaults to `python3`, `node`, `go` and `cargo`. A relative interpreter such as `.venv/bin/python` is the repository's own environment. `runtime.env` sets environment variables for every command, such as `PYTHONPATH: src`.

Each language's reader follows its serializer's rules, because a type is read differently on the sending and receiving sides:

| Language | Sent | Received |
| --- | --- | --- |
| Go (`encoding/json`) | Fields without `omitempty` are present; pointers, slices and maps may be `null` | Nothing is required; every field accepts `null` |
| Rust (serde) | Fields are present unless `skip_serializing_if`; `Option` is `null` when empty | Fields are required unless `Option` or `default`; only `Option` accepts `null`; `deny_unknown_fields` rejects others |
| Python, TypeScript | The same schema both ways: Pydantic's for Python; for TypeScript, `?` and `undefined` make a field optional and `null` makes it nullable | |

Types that cannot be followed, such as a type from another package, are left open and reported as informational; fields under them are not compared.

### Inferred links

A service that exports its OpenAPI documents (`exports.openapi`) needs no links: calls to its operations from any repository's code are found and checked, as `lemma verify` does. A service's code (`index.include`) is read for its route code and searched for calls to others, unless the service is itself a generated client (`exports.client`); a service alone in its repository that declares nothing else has the whole repository searched.

Calls through clients need no declaration. A call through a client library (`client.authConfigs.list()`) reaches the client method its names spell, where the code imports the client's package or its repository depends on it; the client's own code says what that method requests. A client generated at build time is recognised by its imports, a module inside the repository that no file holds (or a module re-exporting it), and its functions by the `operationId`s they are named after.

| Field | Meaning |
| --- | --- |
| `index.include` | Directories or files the service is made of, relative to the repository root. |
| `index.exclude` | Glob patterns never read, such as `**/generated/**`. |
| `index.languages` | Only files in these languages (`typescript`, `tsx`, `python`, `go`, `rust`). |
| `exports.openapi` | OpenAPI documents the service serves. Each operation with an `operationId` also becomes one of its operations, so a link can name it. |
| `exports.operations` | Operations declared where documents do not describe them (above). |
| `exports.symbols` | Files whose definitions are the service's public interface, as a library's entry modules. |
| `exports.operations_from` | Reads the service's operations from its code instead of trusting committed documents: an adapter imports the code and writes the OpenAPI document the framework describes for it. Built in: `hono-zod-openapi` and `fastapi`; any other framework through a `command`. The code's operations are laid over the committed `openapi` documents (or stand alone without them), so consumers are checked against what the code serves, and each indexed endpoint sits at the code registering it (`found_by: adapter`). An adapter that cannot read the code, or reads only part of it, is reported as a warning, never passed silently. See [Reading operations from code](adapters.md). |
| `exports.routes` | The calls the service's code writes routes with, so each documented operation's handler is found by what is declared rather than guessed from the code: `register` (calls that take a route and its handler, such as `Handle` for `r.Handle("/x", NewHandler(db))` or `openapi` for `router.openapi(DEF, handler)`) and `define` (calls that only describe a route, whose value a `register` call passes, such as `createRoute`). Routes written another way are still found by reading the code; each stored endpoint records which way (`found_by`: `directive`, `code`, or `adapter`). A service with `operations_from` rarely needs these: its adapter says where each route is registered. |
| `rejects_unknown_fields` | As in `lemma verify`'s manifest (see [Verifying changes](verify.md#the-manifest)). |
| `exports.client` | Set when the service is itself a client of another: `generated_from` (documents it makes one function per operation from), `call_names`, and `specs` (its copy of the documents, whose declared parameters are checked). Its code is read for what each of its methods requests, and not searched for calls: it is the client, not a caller. |

## Checks

For each declared link, cheapest first:

1. **The link persists.** Both operations are declared at head, and their code anchors resolve.
2. **The schemas agree.** For the request, what the caller sends is compared with what the provider accepts; for the response, what the provider sends with what the caller reads. A field the receiver requires and the sender may leave out, a value the receiver does not accept, and a response field the caller expects and the provider dropped are problems.

With a change, only problems the change introduced count; what was already there is informational. Without one, every problem counts. The level decides what a problem is: a warning at `audit` (the default), an error at `check`, which exits with 4.

Inferred links are checked by `lemma verify`'s grammar. Its errors are problems; its warnings stay warnings.

### What a problem affects

Each problem lists what it affects further up, nearest first: the operations whose handlers reach the failing call, the code in other repositories that calls those operations, and so on. The call graph is read by name: a function calls another if its body mentions the other's name, and a method is reached through its type. Names that several functions share, and names so many functions mention that reaching one through them is no evidence (an error helper every handler uses), are not followed. A route's handler is found as exports find it: the call its registration names, the constant holding the registration, a helper its path is passed to, or an inline function, which counts only if its own code reaches the failing call. A walk stops at a route's registration (the code declaring the route, or a constant holding it): code that collects or documents routes, such as an OpenAPI generator, reads them without calling their handlers, and each route's operation is reached through the route itself. A trace is an approximation, so it is shown with a problem and never raises one.

## Command

```sh
lemma check [lemma.yaml] [--change <#repo> --base <commit> [--head <commit>]] [--level audit|check] [--all] [--engine souffle|reference] [--json]
```

`--change` names the changed repository as the parent's services name it (`#billing`), or `self` for the parent's own. `--base` is read from Git; `--head` too when given, and otherwise the working directory is the head, uncommitted changes included. Every other repository is read at its `ref`, or its working directory. Without a change, inferred links are checked only where a service reads its operations from code: its committed documents are compared with the code.

When a commit predates the repository's `lemma.yaml`, the working directory's is used, and documents it names that did not exist then are skipped; the report says so. History from before a team adopted Lemma can be replayed this way.

## Limits

- One commit is read per repository, so services in one repository cannot be checked at different commits, as when one deploys before another.
- Types are followed within the files read: Go and Rust in the anchored file's directory, TypeScript through relative imports, Python in the anchored file's directory when Pydantic is not available.
- Routes whose paths are built at run time are not found, so tracing cannot pass through them.
- Tracing reads the call graph by name and can include functions that only share a name with a caller.
- The working directory is read as Git sees it: files it ignores (a `.venv`, build output) are not code. Links to directories are followed.
