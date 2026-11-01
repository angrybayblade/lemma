# Verifying changes across repositories

`lemma verify` checks a change in one repository against the repositories that depend on it. It extracts facts from every repository in a manifest, evaluates a grammar's rules and requirements over them, and reports each requirement a change violates, with the evidence behind it.

The first grammar covers HTTP contracts: a service that publishes operations (described by OpenAPI documents and route code), generated clients that map methods to those operations, and consumers that call them.

## Example

Gateway removed the `search` parameter from `GET /api/v3.1/session_configs`. Its query schema is strict, so a request that still sends `search` gets a 400.

```text
$ lemma verify search-removed.yaml
change platform fa87ef98a7^..fa87ef98a7  (56208 facts, souffle engine)
error   platform-client still declares query parameter `search` on GET /api/v3.1/session_configs, which the service removed
        [client_declares_removed_parameter]
error   sdk:ts/packages/core/src/models/SessionConfigs.ts::SessionConfigs.list calls GET /api/v3.1/session_configs and uses `search`, a query parameter that was removed (sdk:ts/packages/core/src/models/SessionConfigs.ts:84)
        [consumer_sends_removed_parameter]
2 errors, 0 warnings, 228 informational (--all lists them)
```

`--explain` shows why each finding holds, down to the facts it rests on:

```text
calls(SessionConfigs.list, GET /api/v3.1/session_configs, SessionConfigs.ts, 84)
  calls_client(SessionConfigs.list, sessionConfigs.list, SessionConfigs.ts, 84)
  client_operation(platform-client, sessionConfigs.list, GET, /api/v3.1/session_configs, src/compat/generated-operations.ts, 36)
  operation@base(gateway, GET /api/v3.1/session_configs, GET, /api/v3.1/session_configs)
removed_parameter(GET /api/v3.1/session_configs, search, query)
  parameter@base(GET /api/v3.1/session_configs, search, query)
  not parameter(GET /api/v3.1/session_configs, search, query)
uses(SessionConfigs.ts, search)
  imports(SessionConfigs.ts, types/sessionConfigs.types.ts)
  mentions(types/sessionConfigs.types.ts, search)
```

## The manifest

```yaml
grammar: grammars/http-contracts.yaml      # optional; this grammar is built in
change: { repository: platform, base: fa87ef98a7^, head: fa87ef98a7 }
repositories:
  platform:
    path: ../platform
    role: service
    service: gateway
    specs: [apps/gateway/openapi-v3.1.json, apps/gateway/openapi-internal-dashboard.json]
    routes: [apps/gateway/src/pages/api]
    schemas: [apps/gateway/src/lib, apps/gateway/src/common]
    route_aliases: { /api/v3/: /api/v3.1/ }
  platform-client:
    path: ../platform-client
    commit: 9de76a65
    role: client
    tables: [src/compat/generated-operations.ts, python/src/sdk_client/_operations.py]
    specs: [spec/openapi.json]
  sdk:
    path: ../sdk
    commit: d7a25360
    role: consumer
    include: [ts/packages/core/src, python/sdk]
    client_receivers: [client, _client]
  dashboard:
    path: ../dashboard
    commit: d67f8a5c
    role: consumer
    include: [src]
```

| Field | Meaning |
| --- | --- |
| `change` | The repository that changed and the commits before (`base`) and with (`head`) the change: a service, or a consumer whose new calls are checked against the services. |
| `path` | The repository's clone, relative to the manifest. Files are read from Git at the commit, without a checkout. |
| `commit` | The commit to read for repositories other than the changed one, the same before and after the change; the working directory if absent. |
| `role` | `service`, `client` or `consumer`. |
| `specs` | OpenAPI documents: a service's contract, or a client's copy of it. |
| `routes` | Directories with a service's route code, for checking the spec against the code. |
| `schemas` | For a service: further directories with schemas its routes import, such as shared zod schemas, read to tell which reject unknown keys. |
| `rejects_unknown_fields` | For a service: requests with fields it does not know fail, as with strict validation, even where its spec does not say so. |
| `tables` | Files with a client's operation tables (objects or calls naming a method, a path and a method chain). |
| `include` | Directories with a consumer's code; the whole repository if absent. |
| `client_receivers` | Names a consumer calls its client through, such as `client`. |
| `generated_from` | OpenAPI documents a client is generated from, one function per operation named after its `operationId` in camel case (`get_pollRevokeJob` is `getPollRevokeJob`). |
| `client_modules` | Modules of generated clients, such as `generated/worker`: functions a consumer imports from them are client calls. |

One clone can appear under several names, such as a service and a consumer of another service in the same repository. Every entry for the changed repository's clone is read at both commits of the change, unless it sets its own `commit`: pinning a consumer to an older commit asks how the change meets code from before it.

## What is extracted

| From | Facts |
| --- | --- |
| A service's OpenAPI documents, at both commits | operations, parameters (and whether required), request and response fields as dotted paths (`hooks[].handler.action`) |
| A service's route code | which request schemas are zod `.strict()` (in the route's file, a file it imports, or a union of them); routes declared with a method and path: TypeScript objects and calls, Go `net/http`, gorilla/mux, chi, gin and echo, Rust axum, actix-web and Rocket |
| A client's tables and spec | which method chain calls which operation; the parameters it declares |
| A consumer's code (TypeScript, JavaScript, Python, Go, Rust) | strings naming API paths, with the HTTP method where the call shows it; calls through its client (`this.client.sessionConfigs.list()`); the field names each file uses; its relative imports |

Paths compare in one form, so `{id}`, `${id}`, `:id` and `[id]` are the same parameter. Test files in consumers are skipped.

## Findings

| Requirement | Severity | Meaning |
| --- | --- | --- |
| `consumer_calls_removed_operation` | error | A consumer calls an operation the change removed. |
| `consumer_calls_unserved_operation` | error | A changed consumer adds a call to a path in a service's family (`/api/v3.1`) that no service serves, such as an endpoint not yet deployed. |
| `consumer_misses_new_required_parameter` | error | An operation a consumer calls now requires a parameter. |
| `consumer_misses_new_required_field` | error | An operation now requires a request field the consumer's code never names. |
| `consumer_sends_removed_parameter` | error | A consumer calls an operation and names a parameter the change removed, and the operation rejects parameters it does not know. |
| `consumer_sends_ignored_parameter` | warning | As above, where the operation ignores unknown parameters. |
| `consumer_sends_rejected_field` | error | A consumer names a request field the change removed, and the operation rejects fields it does not know: its spec says `additionalProperties: false`, its route's zod schema is `.strict()`, or its service sets `rejects_unknown_fields`. |
| `consumer_uses_removed_request_field` | warning | A consumer names a request field the change removed or renamed; the operation now ignores it, so whatever it set no longer takes effect. |
| `consumer_uses_removed_response_field` | warning | A consumer names a response field the change removed or renamed. |
| `client_declares_removed_parameter` | error | A generated client still declares a parameter the service removed. |
| `client_calls_removed_operation` | error | A client method calls an operation the change removed. |
| `consumer_calls_changed_operation` | info | A consumer calls an operation whose request changed, but never names the changed field. |
| `route_not_in_spec` | info | The service serves a route its spec does not declare. Public specs leave internal routes out on purpose. |

"Names a field" means the field's last name appears as an object key, member, keyword argument, dictionary key, struct field or JSON tag, in the calling function itself or in declarations outside functions (schemas, types, constants) in its file or a file it imports. Other functions of the same file do not count, since they often reuse common names for other things. Findings that rest on a name being absent, such as a new required field the consumer never sets, search the whole file and its imports instead. It is evidence, not proof: a consumer that builds requests dynamically can use a field without naming it, and a common name can appear for another reason. Without that evidence a field change is informational, so a change most consumers never touch does not produce a wall of errors.

`lemma verify` exits with 4 if any finding is an error, so CI can gate on it. `--json` prints the findings with their values. `--relation calls` also prints every row of a derived relation, here which consumer calls which operation, for inspecting what the rules concluded.

## Grammars

Requirements live in a grammar, not in Lemma's code. The built-in one is [`grammars/http-contracts.yaml`](../grammars/http-contracts.yaml); copy it and point a manifest's `grammar` at the copy to add or change requirements. A grammar declares relations, rules in Datalog, and requirements:

```yaml
rules:
  - removed_parameter(op, n, loc) :- parameter@base(op, n, loc), !parameter@head(op, n, loc).
requirements:
  consumer_sends_removed_parameter:
    when: calls(c, op, f, l), removed_parameter(op, n, loc), uses(f, n)
    severity: error
    explain: "{c} calls {op} and uses `{n}`, a {loc} parameter that was removed ({f}:{l})"
```

`@base` and `@head` read a relation in the snapshot before or with the change; without one, the head snapshot is meant. Rule heads need no declaration: their columns come from the rule bodies. Negation (`!`) is allowed when everything it names is bound, and rules cannot depend on themselves through negation.

## Engines

Rules run on [Soufflé](https://souffle-lang.github.io) when it is installed (`brew install souffle`), otherwise on Lemma's built-in reference engine; `--engine souffle` or `--engine reference` picks one. Both give the same results. Soufflé's numbers are 32-bit; Lemma reports values that do not fit instead of truncating them.

## How well it works

Measured on the generic platform's repositories (November 2026):

- **Coverage.** Against an independent map of which code calls which route, Lemma found every real call the map lists from the dashboard and the SDKs to Gateway, and all 35 operations Gateway (TypeScript) calls on Worker (Go) through its generated client. It also found 116 SDK calls made through Gateway's generated client, which the map cannot see.
- **Gateway changes.** All 94 changes to Gateway's specs, each checked against the consumers as they were when it merged. Lemma flagged three. All three were real breaks that the dashboard or SDK had to follow up on, as each PR or its companion PR confirms. It flagged nothing else, and no missed break was found.
- **Worker changes.** All 156 changes to Worker's spec, checked against Gateway at the same commits. Lemma reported no errors and 8 warnings, none of them a real break. Each was a common field name (`id`, `slug`, `toolkit`) that Gateway, updated in the same commit, uses for its own objects. One change made a field newly required (`auth` on tool sync). With Gateway pinned to the commit before it, Lemma reports that the caller never sends the field.
- **Consumer changes.** 68 dashboard changes that touch Gateway paths. Lemma flagged one: a call to an endpoint that Gateway only started serving a week later.
- **Rust.** On Qdrant (actix-web), all 69 operations in its spec are found among the routes Lemma reads from its handlers.

A run takes 9 to 20 seconds with Soufflé.

## Limits of this version

- Only HTTP contracts and OpenAPI specs.
- Field changes compare names, not types or enum values: a field whose allowed values change (`"prompt"` to `"condition_check"`) is not reported.
- Calls whose URL is built at run time from values the code does not show are not matched to an operation.
- "Names a field" cannot tell a field read from a response apart from a consumer's own object that reuses the name, so warnings about common names (`id`, `slug`) can be wrong.
- Consumers are matched to operations by path and method; a consumer is reported against the operation it was written for (the base snapshot).
