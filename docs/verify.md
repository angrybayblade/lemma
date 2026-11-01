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
| `change` | The repository that changed and the commits before (`base`) and with (`head`) the change. |
| `path` | The repository's clone, relative to the manifest. Files are read from Git at the commit, without a checkout. |
| `commit` | The commit to read for repositories other than the changed one; the working directory if absent. |
| `role` | `service`, `client` or `consumer`. |
| `specs` | OpenAPI documents: a service's contract, or a client's copy of it. |
| `routes` | Directories with a service's route code, for checking the spec against the code. |
| `tables` | Files with a client's operation tables (objects or calls naming a method, a path and a method chain). |
| `include` | Directories with a consumer's code; the whole repository if absent. |
| `client_receivers` | Names a consumer calls its client through, such as `client`. |

## What is extracted

| From | Facts |
| --- | --- |
| A service's OpenAPI documents, at both commits | operations, parameters (and whether required), request and response fields as dotted paths (`hooks[].handler.action`) |
| A service's route code | routes declared with a method and path |
| A client's tables and spec | which method chain calls which operation; the parameters it declares |
| A consumer's code (TypeScript, Python) | strings naming API paths, with the HTTP method where the call shows it; calls through its client (`this.client.sessionConfigs.list()`); the field names each file uses; its relative imports |

Paths compare in one form, so `{id}`, `${id}`, `:id` and `[id]` are the same parameter. Test files in consumers are skipped.

## Findings

| Requirement | Severity | Meaning |
| --- | --- | --- |
| `consumer_calls_removed_operation` | error | A consumer calls an operation the change removed. |
| `consumer_misses_new_required_parameter` | error | An operation a consumer calls now requires a parameter. |
| `consumer_misses_new_required_field` | error | An operation now requires a request field the consumer's code never names. |
| `consumer_sends_removed_parameter` | error | A consumer calls an operation and names a parameter the change removed. |
| `consumer_uses_removed_request_field`, `consumer_uses_removed_response_field` | warning | A consumer names a field the change removed or renamed. |
| `client_declares_removed_parameter` | error | A generated client still declares a parameter the service removed. |
| `client_calls_removed_operation` | error | A client method calls an operation the change removed. |
| `consumer_calls_changed_operation` | info | A consumer calls an operation whose request changed, but never names the changed field. |
| `route_not_in_spec` | info | The service serves a route its spec does not declare. Public specs leave internal routes out on purpose. |

"Names a field" means the consumer's file, or a file it imports, uses the field's last name as an object key, member, keyword argument or dictionary key. It is evidence, not proof: a consumer that builds requests dynamically can use a field without naming it, and a common name can appear for another reason. Without that evidence a field change is informational, so a change most consumers never touch does not produce a wall of errors.

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

## Limits of this version

- Only HTTP contracts, TypeScript and Python consumers, and OpenAPI specs.
- Field changes compare names, not types or enum values: a field whose allowed values change (`"prompt"` to `"condition_check"`) is not reported.
- Calls whose URL is built at run time from values the code does not show are not matched to an operation.
- Consumers are matched to operations by path and method; a consumer is reported against the operation it was written for (the base snapshot).
