# Check links between repositories

`lemma check` checks a change in one repository against every repository that depends on it, starting from `.lm` descriptions. Each repository describes itself in a root module, `mod.lm`; a workspace module, usually in a repository of its own, composes them. Lemma finds the operations that depend on each other, compares the schemas on both sides of each link, and traces what a broken link affects further up.

[`lemma verify`](verify.md) checks that a description still matches the code it describes, whatever a change does.

## Example

A billing service's function takes a request and returns a response. A web service calls it through a Go client. The billing team adds a required `currency` field and stops returning `receipt_url`.

```text
$ lemma check --change billing --base main --head HEAD
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

## Descriptions

Every repository has a root module, `mod.lm`, named after the repository. It says what the repository is: each service, what it is made of (`index`: the code Lemma reads for it), what it offers (`operations`, and any `operation` declared by hand) and, in a `graph`, the links Lemma cannot infer.

```lm
// billing/mod.lm
module billing

import * as http from lemma.http
import * as python from lemma.python

let handlers = python.file("billing/handlers.py")

export service api {
    runtime: { python: ".venv/bin/python" }
    index: { include: ["billing"] }
    operations: http.openapi("openapi.json") // calls to these operations are inferred

    // One Lemma cannot infer: a queue message handler.
    export operation create_charge {
        implementation: handlers.symbol("create_charge")
        request: handlers.type("CreateChargeRequest")
        response: handlers.type("CreateChargeResponse")
    }
}
```

```lm
// web/mod.lm
module web

import * as go from lemma.go
import { Link } from lemma.links
import { api } from billing

repo billing = git(url: "git@github.com:example/billing.git", rev: "3f2a1c9e04")

let charges = go.file("client/charges.go")

export service web {
    index: {
        include: ["src", "cmd", "client"]
        exclude: ["**/*_test.go"]
    }

    export client operation create_charge {
        implementation: charges.symbol("Charges.Create")
        sends: charges.type("CreateChargeRequest")
        accepts: charges.type("CreateChargeResponse")
    }
}

export graph links : Link {
    web.create_charge -> api.create_charge
}
```

```lm
// workspace/mod.lm: the entry point
module system

workspace {
    repo billing = local("../billing")
    repo web = local("../web")
    repo storefront = local("../storefront")
    config: { level: check }
}
```

Lemma reads the entry module, then every repository its `repo` declarations name, and theirs in turn. The first declaration of a name wins, so the workspace's pins override a repository's. A repository has one root module, and other modules sit below it by their place: `checks/replay.lm` in `billing` is `billing.checks.replay`, which the root module imports to include it.

### Repositories

| Written | Meaning |
| --- | --- |
| `repo billing = local("../billing")` | A local clone, relative to this file, read from its working directory. |
| `repo billing = local("../billing", rev: "3f2a1c9e04")` | The same clone, read at that commit. |
| `repo billing = git(url: "...", rev: "3f2a1c9e04")` | A repository cloned into Lemma's cache (`~/.cache/lemma/repos`), read at `rev`. A commit is preferred: it keeps checks reproducible. |

A repository's name is its root module's, so it is an identifier: a directory called `web-app` has the module `web_app`, and findings print `#web_app`. `--change` takes either the name or the directory.

A missing `local(...)` repository is an error. A `git(...)` repository that is not cloned follows [the `clone` setting](commands.md#repositories-that-are-not-cloned).

### Operations

An operation is something a service provides or calls, with a request and a response. A `client operation` is one the service calls: its schemas are `sends` and `accepts` where a provider's are `request` and `response`. Each one names where it is in code (`implementation`, which must resolve) and where its schemas come from:

| Schema source | Example | Read by |
| --- | --- | --- |
| A type in code | `request: charges.type("CreateChargeRequest")` | Tree-sitter for Go, TypeScript and Rust; Pydantic for Python, falling back to Tree-sitter |
| A command's output | `let spec = json.command("{python} scripts/spec.py")` and `request: spec.at("$.create_charge.request")` | The command, run in the repository at the commit; the location is a JSON path into its output |
| A document | `let spec = json.document("spec.json")` and `request: spec.at("$.create_charge.request")` | The file; `format: "openapi"` on `json.document` converts OpenAPI 3.0's `nullable` |

Lemma keeps a command's output, and a Python type read with Pydantic, in its cache for the commit they were produced at, by the code's Git tree, the command and its environment, the interpreter and its installed packages when Python runs, and `PATH`, `VIRTUAL_ENV` and `PYTHONPATH`; a commit not checked out runs in one worktree Lemma reuses per repository. Pass `cache: false` to a `json.command` whose output depends on anything else, such as a running service or files Git ignores.

`{python}`, `{node}`, `{go}` and `{cargo}` in a command are the service's `runtime`, which defaults to `python3`, `node`, `go` and `cargo`. A relative interpreter such as `.venv/bin/python` is the repository's own environment. `runtime: { env: { PYTHONPATH: "src" } }` sets environment variables for every command.

Each language's reader follows its serializer's rules, because a type is read differently on the sending and receiving sides:

| Language | Sent | Received |
| --- | --- | --- |
| Go (`encoding/json`) | Fields without `omitempty` are present; pointers, slices and maps may be `null` | Nothing is required; every field accepts `null` |
| Rust (serde) | Fields are present unless `skip_serializing_if`; `Option` is `null` when empty | Fields are required unless `Option` or `default`; only `Option` accepts `null`; `deny_unknown_fields` rejects others |
| Python, TypeScript | The same schema both ways: Pydantic's for Python; for TypeScript, `?` and `undefined` make a field optional and `null` makes it nullable | |

Lemma leaves types it cannot follow, such as a type from another package, open and reports them as informational; it does not compare the fields under them.

### Inferred links

`lemma verify --links` lists every link, declared and inferred:

```text
$ lemma verify --links
declared links: 1
  #web:services.web.create_charge -> #billing:services.api.create_charge
inferred links: 521
  #dashboard -> #platform:gateway: 203 calls to 123 operations
    src/server/clients/session.ts::createSession -> POST /api/v3/sessions
    ...
```

A service that declares its OpenAPI documents (`operations: http.openapi(...)`) needs no links: Lemma finds and checks calls to its operations from any repository's code. Lemma reads a service's code (`index.include`) for its route code and searches it for calls to others, unless the service is itself a generated client (`client`); for a service alone in its repository that declares nothing else, it searches the whole repository.

Calls through clients need no declaration. A call through a client library (`client.apiKeys.list()`) reaches the client method its names spell, where the code imports the client's package or its repository depends on it; the client's own code says what that method requests. Lemma recognizes a client generated at build time by its imports, a module inside the repository that no file holds (or a module re-exporting it), and its functions by the `operationId`s they are named after.

| Field of a service | Meaning |
| --- | --- |
| `index: { include: [...] }` | Directories or files the service is made of, relative to the repository root. |
| `index: { exclude: [...] }` | Glob patterns never read, such as `**/generated/**`. |
| `index: { languages: [...] }` | Only files in these languages (`typescript`, `tsx`, `python`, `go`, `rust`). |
| `operations: http.openapi("openapi.json")` | OpenAPI documents the service serves. Each operation with an `operationId` also becomes one of its operations, so a link can name it. A list holds several. |
| `operation` declarations | Operations declared where documents do not describe them (above). |
| `symbols: [...]` | Files whose definitions are the service's public interface, as a library's entry modules. |
| `operations: http.openapi("openapi.json", command: ...)`, or `hono.app(...)`, `fastapi.app(...)`, `http.adapter(...)` or `command(...)` alone | Reads the service's operations from its code instead of trusting committed documents: an adapter imports the code and writes the OpenAPI document the framework describes for it. The code's operations are laid over any committed documents in the same list (or stand alone without them), so consumers are checked against what the code serves, and each indexed operation sits at the code registering it (`found_by: adapter`). An adapter that cannot read the code, or reads only part of it, is reported as a warning, never passed silently. See [Reading operations from code](adapters.md). |
| `routes: { register: [...], define: [...] }` | The calls the service's code writes routes with, so each documented operation's handler is found by what is declared rather than guessed from the code: `register` (calls that take a route and its handler, such as `Handle` for `r.Handle("/x", NewHandler(db))` or `openapi` for `router.openapi(DEF, handler)`) and `define` (calls that only describe a route, whose value a `register` call passes, such as `createRoute`). Routes written another way are still found by reading the code; each stored operation records which way (`found_by`: `directive`, `code`, or `adapter`). A service with an adapter rarely needs these: its adapter says where each route is registered. |
| `rejects_unknown_fields: true` | Requests with fields the service does not know fail, as with strict validation, even where its documents and schemas do not say so. |
| `client: http.generated_client(generated_from: [...])` | Set when the service is itself a client of another: `generated_from` (documents it makes one function per operation from), `call_names` (how it names each function from the `operationId`: `{operationId}`, `{camel}`, `{pascal}` or `{snake}`, with any fixed text around it), and `specs` (its copy of the documents, whose declared parameters are checked). Its code is read for what each of its methods requests, and not searched for calls: it is the client, not a caller. |

## Checks

For each declared link, cheapest first:

1. **The link persists.** Both operations are declared at head, and their code anchors resolve. An operation removed while a link still names it is a `link` finding, marked introduced, so `lemma check` exits with 4 and goes on to check the other repositories.
2. **The schemas agree.** For the request, what the caller sends is compared with what the provider accepts; for the response, what the provider sends with what the caller reads. A field the receiver requires and the sender may leave out, a value the receiver does not accept, and a response field the caller expects and the provider dropped are problems.

Lemma reports a change that deletes a document a service exports, or leaves an `include` matching nothing, as `document_missing` or `include_matches_nothing`. [`lemma verify`](verify.md) finds those without a change.

With a change, only problems the change introduced count; what was already there is informational. Without one, every problem counts. The level decides what a problem is: a warning at `audit` (the default), an error at `check`, which exits with 4.

[`lemma.http`'s requirements](#lemmahttp-requirements) check inferred links. Its errors are problems; its warnings stay warnings. Inference reads only the code that may call an operation the change touches, with the files it imports and the files the change edits (see `--evaluation` in [Commands](commands.md#lemma-check-modlm)), so the count of inferred links is of calls to those operations. A trace asks for the callers of each further operation it reaches as it reaches it.

### Wiring

A change that adds to a service breaks nothing, so the checks above stay quiet even when it is only half done. Three requirements of `lemma.http` look at what a change adds, and only at that, so a system's older gaps stay out of the report:

| Requirement | Warns when |
| --- | --- |
| `operation_without_route` | The change adds an operation to a spec, and no route in the service's code serves it. Only for services whose routes Lemma reads (`index.include` holding route code it recognizes), and only when the service's route code never writes the path, whole or as a router's prefix and a path under it: a route Lemma cannot read, such as one registered in a helper, is given the benefit of the doubt. |
| `route_without_operation` | The change adds a route its service's spec does not declare. |
| `operation_without_consumer` | The change adds an operation that no code in the workspace calls, directly or through a generated client. |

A call in another repository's working directory counts, so a feature built across a service and its consumer checks clean once both halves are written, committed or not. An operation meant for callers outside the workspace is stated in a graph, and then needs no caller:

```lm
import { public_operation, public_service } from lemma.http

graph public : public_operation {
    "GET /v1/charges/{}"
}
```

Operations are written as the findings print them: the method, then the path with each parameter as `{}`. `public_service` marks every operation of a service (`"#billing:api"`). A call the change adds to an operation no spec declares is `consumer_calls_unserved_operation`, an error.

### Impact

Each problem lists what it affects further up, nearest first: the operations whose handlers reach the failing call, the code in other repositories that calls those operations, and so on. Lemma reads the call graph by name: a function calls another if its body mentions the other's name, and a method is reached through its type. Lemma does not follow names that several functions share, or names so many functions mention that reaching one through them is no evidence (an error helper every handler uses). A route's handler is found as exports find it: the call its registration names, the constant holding the registration, a helper its path is passed to, or an inline function, which counts only if its own code reaches the failing call. A walk stops at a route's registration (the code declaring the route, or a constant holding it): code that collects or documents routes, such as an OpenAPI generator, reads them without calling their handlers, and each route's operation is reached through the route itself. A trace is an approximation, so it is shown with a problem and never raises one.

## `lemma.http` inputs

`lemma.http` is the standard library's module of requirements for HTTP contracts. It is always loaded. Its facts come from the repositories' documents and code:

| From | Facts |
| --- | --- |
| A service's OpenAPI documents, at both commits | operations, parameters (and whether required), request and response fields as dotted paths (`items[].lines.amount`), whether the request rejects unknown fields |
| A service's route code | routes declared with a method and path (TypeScript objects and calls; Express, Fastify, Hono and Koa routers given a handler, as in `app.post("/charges", handler)`, a router known by how it is made or typed (`express.Router()`, `r: Router`) whatever its name; FastAPI and Flask decorators; each route under its router's prefix, whether the router is made with one (`APIRouter(prefix=...)`, `Blueprint(url_prefix=...)`, `new Router({ prefix })`, `basePath`) or mounted under one (`app.use("/v1", charges)`, `app.route`, `register(..., { prefix })`, `include_router(..., prefix=...)`, `register_blueprint`), in the same file or one importing the router; in a file its generator's banner marks as generated, only routes registered with a handler; Go `net/http`, gorilla/mux, chi, gin and echo; Rust axum, actix-web and Rocket); which request schemas are zod `.strict()`, in the route's file, a file it imports, or a union of them |
| A generated client's documents | the functions it exposes, named by `call_names`; the parameters it declares |
| A service's code (TypeScript, JavaScript, Python, Go, Rust) | paths in strings and requests, with the HTTP method where the call shows it; calls through clients (`this.client.invoices.list()`) and functions imported from generated clients; the field names each function and file uses; imports, including `tsconfig.json` path aliases |

A string in code counts as a path only if it contains a leading segment some service in the workspace serves, such as `/v1/`; everything before it (a base URL, `${API}`) is dropped. Paths compare in one form, so `{id}`, `${id}`, `:id`, `[id]` and `%s` are the same parameter. Lemma does not search test files for calls.

## `lemma.http` requirements

| Requirement | Severity | Meaning |
| --- | --- | --- |
| `consumer_calls_removed_operation` | error | Code calls an operation the change removed. |
| `consumer_calls_unserved_operation` | error | A change adds a call to a path under a service's prefixes that no service serves, such as an operation not yet deployed. |
| `consumer_misses_new_required_parameter` | error | An operation the code calls now requires a parameter. |
| `consumer_misses_new_required_field` | error | An operation now requires a request field the calling code never names. |
| `consumer_sends_removed_parameter` | error | Code names a parameter the change removed, and the operation rejects parameters it does not know. |
| `consumer_sends_ignored_parameter` | warning | As above, where the operation ignores unknown parameters. |
| `consumer_sends_rejected_field` | error | Code names a request field the change removed, and the operation rejects fields it does not know: its spec says `additionalProperties: false`, its route's zod schema is `.strict()`, or its service sets `rejects_unknown_fields`. |
| `consumer_uses_removed_request_field` | warning | Code names a request field the change removed or renamed; the operation now ignores it, so whatever it set no longer takes effect. |
| `consumer_uses_removed_response_field` | warning | Code names a response field the change removed or renamed. |
| `client_declares_removed_parameter` | error | A generated client still declares a parameter the service removed. |
| `client_code_sends_removed_parameter` | error | A generated client's code still sends a parameter the service removed, though its copy of the document no longer declares it. |
| `client_calls_removed_operation` | error | A client function calls an operation the change removed. |
| `consumer_calls_changed_operation` | info | Code calls an operation whose request changed, but never names the changed field. |
| `consumer_calls_unrouted_operation` | error | Code calls an operation the spec still declares whose route the change removed: a deleted handler. Only for services whose routes are read. |
| `operation_without_route` | warning | The change adds an operation to a spec, and no route in the service's code serves it. Only for services whose routes are read. |
| `route_without_operation` | warning | The change adds a route its spec does not declare. |
| `operation_without_consumer` | warning | The change adds an operation no code calls, directly or through a generated client. An operation stated in `public_operation`, or of a service in `public_service`, is exempt. |
| `route_not_in_spec` | info | The service serves a route its spec does not declare. Public specs often leave internal routes out on purpose. A route the change adds is also warned about by `route_without_operation`. |

"Names a field" means the field's last name appears as an object key, member, keyword argument, dictionary key, struct field or JSON tag, in the calling function itself, in a function it calls by name in its file or in a file it imports (an SDK's `transformListResponse`), or in declarations outside functions (schemas, types, constants) in its file or a file it imports. Functions it does not call do not count, since they often reuse common names for other things. Findings that rest on a name being absent, such as a new required field the code never sets, search the whole file and its imports instead. It is evidence, not proof: code that builds requests dynamically can use a field without naming it, and a common name can appear for another reason. Without that evidence a field change is informational, so a change most consumers never touch does not produce a wall of errors.

Add requirements of your own, or change which of these run, in a module: see [Requirements and rules](grammar.md).

## Command

```sh
lemma check [mod.lm] [--change <name or directory> [--base <commit>] [--head <commit>]] [--level audit|check|strict] [--all] [--explain] [--evaluation demand|full|both] [--engine souffle|reference] [--json]
```

`--change` names the changed repository as the workspace names it, by its directory, or `self` for the entry module's own repository. `--base` is read from Git; `--head` too when given, and otherwise the working directory is the head, uncommitted changes included. Every other repository is read at its `rev`, or its working directory. Without a change, inferred links are checked only where a service reads its operations from code: its committed documents are compared with the code.

When a commit predates the repository's `mod.lm`, Lemma uses the working directory's and skips documents it names that did not exist then; the report says so. History from before a team adopted Lemma replays this way.

Every option and exit code is in [Commands](commands.md#lemma-check-modlm).

## Limits

- One commit is read per repository, so services in one repository cannot be checked at different commits, as when one deploys before another.
- Types are followed within the files read: Go and Rust in the anchored file's directory, TypeScript through relative imports, Python in the anchored file's directory when Pydantic is not available.
- Routes whose paths are built at run time are not found, so tracing cannot pass through them.
- Tracing reads the call graph by name and can include functions that only share a name with a caller.
- The working directory is read as Git sees it: files it ignores (a `.venv`, build output) are not code. Links to directories are followed.
- Field changes compare names, not types or enum values: a field whose allowed values change is not reported.
- Calls whose URL is built at run time from values the code does not show are not matched to an operation.
- "Names a field" cannot tell a field read from a response apart from the calling code's own object that reuses the name, so warnings about common names (`id`, `slug`) can be wrong.
- Only HTTP contracts described by OpenAPI are inferred; declare other links in a `graph`.

See the [pre-release checklist](pre-release.md) for the full list.
