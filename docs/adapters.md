# Reading operations from code

A service's committed OpenAPI documents say what it served when someone last regenerated them. Its code says what it serves now. An adapter reads the code: it imports the service the way the framework does, in a separate process, and writes the OpenAPI document the framework itself generates. `lemma check` checks consumers against that document, and `lemma index` places each endpoint at the code that registers it.

```yaml
services:
  api:
    index:
      include: [src]
    exports:
      openapi: [openapi.json]          # optional: the committed documents
      operations_from:
        adapter: hono-zod-openapi
        entry: [src/index.ts#app, src/routes]
```

## Built-in adapters

| Adapter | Reads | `entry` |
| --- | --- | --- |
| `hono-zod-openapi` | Hono apps whose routes use `@hono/zod-openapi`. Runs with Bun (`runtime.bun`). | The module exporting the app the server serves, `src/index.ts#app` (`#default` for a default export): its document has every route at the path it is served at, through `app.route(...)` mounts and `basePath(...)`. Add the route directories (`src/routes`) to place routes registered through a shared helper at the route module that calls it. Route directories alone are imported module by module and see each route only as its own router registers it. |
| `fastapi` | FastAPI apps. Runs with the service's Python (`runtime.python`). | `module:attribute`, the attribute an app or a function making one (`app.main:create_app`), or a module whose top-level apps are all read. Apps mounted inside the app are read under their mount path. |
| `express-routes` | Express 4 and 5 apps, routes only: method, path and the line registering each route, through `use('/prefix', router)` mounts, nested routers, mounted sub-apps and `app.route(...)`. Runs with Bun; `listen` is stubbed so importing never opens a port. Wildcard and optional paths cover what is below them, regular-expression and `all()` routes are listed as unread. | The module exporting the app the server serves, `src/app.ts#app` (`#default` for a default export or `module.exports`; a function taking no arguments is called to make the app). Add the route directories (`src/routes`) to place routes registered through a shared helper. |
| `flask-routes` | Flask apps, routes only: every rule of the URL map, through blueprints, nested blueprints, `add_url_rule` and class-based views, each placed at its view function or `MethodView` method; apps mounted with werkzeug's `DispatcherMiddleware` under their mount path. Runs with the service's Python. | `module:attribute`: an app, a function making one (`app:create_app`), or a `DispatcherMiddleware`; or a module whose top-level apps are all read. |
| `go-http-routes` | Go services routed with `gorilla/mux` or `net/http`'s `ServeMux`, routes only. Runs with `go run`: it builds a temporary program inside the service's module that calls the entry with zero values (pointers to zero values if that panics) and lists the routers it gets, each route placed at its handler's definition; it removes what it wrote and leaves `go.mod` and `go.sum` as they were. | What builds the router: `./internal/http#NewRouter` (a `*mux.Router` or `*http.ServeMux` parameter gets a fresh router, read after the call), `./pkg#Server.Routes`, `./pkg#Router` (a variable), or `./cmd/api` (its `init` functions and `http.DefaultServeMux`). Files tagged `//go:build lemma` are built in, for a constructor written for Lemma. |

Any other framework can be read by a script in the repository that follows the protocol below, named by its path: `adapter: tools/lemma-adapter.ts`. Its extension decides what runs it: `.ts` Bun, `.js` Node, `.py` the service's Python, `.go` `go run`. A `command` runs anything else, such as a binary printing utoipa's document.

A framework that knows its routes but not their schemas (Express, Flask, `gorilla/mux`) can still be read: its adapter says so with `x-lemma-schemas: false`, the committed documents keep the schemas, and the code says which routes exist and where each is handled.

## Fields

| Field | Meaning |
| --- | --- |
| `adapter` or `command` | A built-in adapter's name, a script in the repository by its path, or a command run with `sh -c`. |
| `entry` | What the adapter reads, relative to `directory`; given as `$LEMMA_ENTRY`, one per line. |
| `exclude` | Glob patterns of files under `entry` not to import, such as scripts that do work when imported; `$LEMMA_EXCLUDE`, one per line. |
| `directory` | Where the adapter runs, relative to the repository root and inside it; the root if absent. |
| `env` | Environment variables for the adapter and `setup`, on top of `runtime.env`. Importing an app often needs a few to keep it from connecting anywhere. |
| `setup` | Installs and builds what importing the code needs, in Lemma's worktree of a commit (see below). Never run in the working directory, which is taken as ready. |
| `timeout_ms` | How long the adapter may run before it is stopped; five minutes if absent. `setup` may run for thirty. |

## Where code is read

Importing code runs it, so Lemma reads code where it already runs: the working directory, uncommitted changes included. That is the head of every change you check. A commit's document is kept in Lemma's cache under the commit's Git tree, so it is generated once:

1. **Kept.** Whenever an adapter reads a clean working directory, its document is that commit's too. Indexing on `main`, or CI on `main`, keeps the document for every commit a branch later starts from.
2. **Read in place.** A commit that is the working directory unchanged is read there.
3. **Built.** Any other commit of the changed repository is checked out in one worktree Lemma keeps per repository, `setup` runs, and the adapter reads it. What earlier builds installed stays, so installing again is mostly a check. The document is then kept.

Other repositories in a check are read where they are checked out. A pinned repository (`ref`, or a `git` source) uses a kept document if there is one and its committed documents otherwise; Lemma never runs another repository's `setup`.

In Python, a build imports the worktree's code before any installed copy of it. An adapter that imports a module from outside the code it is reading (an editable install of the main checkout) fails rather than describe today's code as a past commit's.

## What the document becomes

The code's operations are laid over the committed documents:

- An operation is matched by method and path, whatever its parameters are named (`{id}`, `{userId}`) and with or without a trailing slash. A document whose paths no route serves, but whose paths under another document's prefix are, is a version alias (`/v2/x` served by the `/v1/x` route), and its operations are matched there.
- A matched operation takes the code's parameters, request body and responses, and keeps the committed `operationId`, which links and generated clients use.
- An operation only the code describes is added, to the committed document whose prefix covers it, or to a document of its own.
- A committed operation the code no longer describes is removed, but only when the adapter says it read every route and the code matches at least half the committed operations. Fewer matches usually mean paths the adapter sees differently, such as a base path it was not shown; Lemma then replaces only what matched and warns.
- Components of every kind are added; one whose name a different committed component has is renamed for the code's operations.

Without committed documents, the code's document is the contract.

Without `--change`, `lemma check` compares the committed documents with the code and reports what the difference breaks; when nothing differs, it skips that inference.

## The protocol

An adapter is a process run in `directory` with:

| Variable | Meaning |
| --- | --- |
| `LEMMA_OUTPUT` | The file to write the OpenAPI document to (JSON). |
| `LEMMA_ENTRY` | The configured entries, one per line. |
| `LEMMA_EXCLUDE` | The configured exclusions, one per line. |
| `LEMMA_ROOT` | The root of the code being read: the repository, or Lemma's worktree of a commit. |

It writes an OpenAPI 3 document and exits with 0. The document is kept only if the process succeeds and the file is valid JSON; anything else is a failure, reported as a warning, and the service is checked against its committed documents. By convention, 2 means a missing variable and 3 that no route was found.

The document may carry three extensions:

| Extension | Where | Meaning |
| --- | --- | --- |
| `x-lemma-handler` | each operation | `file:line`, relative to `LEMMA_ROOT`: where the route is registered, or its handler's definition. A registration line means the last named argument of its call; a definition line (`func`, `def`, `function`) that definition; a decorator line the definition it decorates. |
| `x-lemma-unread` | the document | What could not be read: `{"module": "src/x.ts", "reason": "..."}` for code that could not be imported, `{"method": "POST", "path": "/x", "reason": "..."}` for a route that could not be described, with `"prefix": true` for every route under the path (a wildcard route). A route listed keeps its committed definition. |
| `x-lemma-complete` | the document | `true` when the adapter read every route the code registers, so an operation it does not describe is gone. Any unread module makes a reading incomplete. |
| `x-lemma-schemas` | the document | `false` when the adapter describes routes only, without parameters, bodies or responses: matched operations keep their committed definitions. |

The document is cached by the adapter's configuration, its script, the runtime it runs with and Lemma's version, so changing any of them reads the code again. A failure is kept under the same key, in a `.failed` file beside where the document would go, holding the adapter's whole output: the same code and adapter would fail the same way, so they are not run again, and the warning shows the output's first and last lines and names the file. Delete it to try again after changing something the key does not cover, such as an installed toolchain.

## Adding a built-in adapter

A built-in adapter is a script under `crates/lemma-graph/adapters/` and a row in `crates/lemma-graph/src/adapters/registry.rs` naming it and the interpreter it runs with. Until it is built in, the same script works from a repository by its path.

## Limits

- Routes added at run time, after the app is imported, are not seen. With `x-lemma-complete`, their committed operations would be removed; an adapter that cannot see every route should not claim it.
- The Hono adapter patches the copy of `@asteasolutions/zod-to-openapi` that `@hono/zod-openapi` resolves from the first entry; routes registered through another copy in the same process are not seen.
- Adapters run the code they read. A Go constructor that connects to a database, or a module that does work when imported, can fail or hang until `timeout_ms`; a constructor written for Lemma (`//go:build lemma`) or `exclude` avoids it.
- `lemma index` reads no JavaScript files yet, so an adapter's handlers in `.js` files are found but not placed at a definition.
- The Hono adapter runs with Bun. Apps built for Cloudflare Workers or Deno, which import `cloudflare:` or `jsr:` modules, cannot be imported by it.
- Committed documents may be JSON or YAML, OpenAPI 3.0 or 3.1. Swagger 2.0 is not read; `lemma check` warns about such a document.
