# Verifying changes across repositories

`lemma verify` checks a change in one repository against the repositories that depend on it. It extracts facts from every repository in a manifest, evaluates a grammar's rules and requirements over them, and reports each requirement a change violates, with the evidence behind it.

The built-in grammar covers HTTP contracts: services that publish operations (described by OpenAPI documents and route code), clients generated from their specs, and consumers that call them.

## Example

A billing service renames the request field `memo` to `note` on `POST /v1/invoices`. Its request schema sets `additionalProperties: false`, so a request that still sends `memo` is rejected. A web app calls the endpoint with `fetch`; a Python SDK calls it through a client generated from the spec.

```text
$ lemma verify manifest.yaml
change billing HEAD~1..HEAD  (61 facts, souffle engine)
error   sdk:billing_sdk/invoices.py::Invoices.create calls POST /v1/invoices and names `memo`; request field `memo` was removed and POST /v1/invoices now rejects requests that send it (sdk:billing_sdk/invoices.py:6)
        [consumer_sends_rejected_field]
error   web:src/invoices.ts::createInvoice calls POST /v1/invoices and names `memo`; request field `memo` was removed and POST /v1/invoices now rejects requests that send it (web:src/invoices.ts:4)
        [consumer_sends_rejected_field]
2 errors, 0 warnings, 0 informational
```

`--explain` shows why each finding holds, down to the facts it rests on:

```text
calls(web:src/invoices.ts::createInvoice, POST /v1/invoices, web:src/invoices.ts, 4)
  references_operation(...)
    calls_path(web:src/invoices.ts::createInvoice, POST, /v1/invoices, web:src/invoices.ts, 4)
    operation@base(billing, POST /v1/invoices, POST, /v1/invoices)
removed_request_field(POST /v1/invoices, memo, memo)
  request_field@base(POST /v1/invoices, memo, memo)
  not request_field(POST /v1/invoices, memo, memo)
uses(web:src/invoices.ts::createInvoice, memo)
  mentions(web:src/invoices.ts::createInvoice, memo)
rejects_unknown(POST /v1/invoices)
  strict_request(POST /v1/invoices)
```

## The manifest

```yaml
grammar: contracts.yaml                    # optional; the built-in HTTP contracts grammar otherwise
change: { repository: billing, base: HEAD~1, head: HEAD }
repositories:
  billing:
    path: ../billing
    role: service
    specs: [openapi.json]
    routes: [src/routes]
  billing-client:                          # the client the SDK generates from billing's spec
    path: ../billing
    commit: main
    role: client
    generated_from: [openapi.json]
    call_names: ["{snake}"]                # createInvoice is called as create_invoice
  web:
    path: ../web
    role: consumer
    include: [src]
  sdk:
    path: ../sdk
    role: consumer
    include: [billing_sdk]
    client_receivers: [_client]
```

| Field | Meaning |
| --- | --- |
| `change` | The repository that changed and the commits before (`base`) and with (`head`) the change: a service, or a consumer whose new calls are checked against the services. `--base` and `--head` override the commits. |
| `path` | The repository's clone, relative to the manifest. Files are read from Git at the commit, without a checkout. |
| `commit` | The commit to read for repositories other than the changed one, the same before and after the change; the working directory if absent. |
| `role` | `service`, `client` or `consumer`. |
| `specs` | OpenAPI documents: a service's contract, or a client's copy of it. |
| `service` | On a service: the name used in findings. On a consumer: the service its code belongs to; its references to that service's own operations (route declarations) are not calls. |
| `routes` | For a service: directories with its route code, for checking the spec against the code and telling which request schemas reject unknown fields. |
| `schemas` | For a service: further directories with schemas its routes import. |
| `route_aliases` | For a service: path prefixes it also serves routes under, such as `{ /v1/: /v2/ }`; each route counts at both paths. Rarely needed: when most of a spec's operations have no route of their own but one under another spec's prefix, Lemma infers the alias. |
| `path_prefixes` | For a service: the path prefixes it owns, such as `/v1/billing/`. A changed consumer's new call to an unserved path under one is reported. If absent, derived from each spec: the prefix its paths share, or, where another service's paths start with that too, the prefixes that tell them apart. Rarely needed. |
| `rejects_unknown_fields` | For a service: requests with fields it does not know fail, as with strict validation, even where its spec and schemas do not say so. |
| `generated_from` | For a client: OpenAPI documents it is generated from, one function per operation. |
| `call_names` | For a client: how it names an operation's function, from the `operationId`: templates with `{operationId}`, `{camel}`, `{pascal}` or `{snake}`, such as `["{camel}"]` or `["{pascal}.Invoke"]`. The `operationId` as written if absent. |
| `include` | For a consumer: directories with its code; the whole repository if absent. |
| `client_receivers` | For a consumer: names it calls its clients through, such as `client`. |
| `client_modules` | For a consumer: modules of generated clients, such as `generated/billing`; functions imported from them are client calls. |

One clone can appear under several names, such as a service and a consumer of another service in the same repository. Every entry for the changed repository's clone is read at both commits of the change, unless it sets its own `commit`: pinning a consumer to an older commit asks how the change meets code from before it.

## What is extracted

| From | Facts |
| --- | --- |
| A service's OpenAPI documents, at both commits | operations, parameters (and whether required), request and response fields as dotted paths (`items[].lines.amount`), whether the request rejects unknown fields |
| A service's route code | routes declared with a method and path (TypeScript objects and calls, Go `net/http`, gorilla/mux, chi, gin and echo, Rust axum, actix-web and Rocket); which request schemas are zod `.strict()`, in the route's file, a file it imports, or a union of them |
| A client's specs | the functions it exposes, named by `call_names`; the parameters it declares |
| A consumer's code (TypeScript, JavaScript, Python, Go, Rust) | paths in strings and requests, with the HTTP method where the call shows it; calls through clients (`this.client.invoices.list()`) and functions imported from client modules; the field names each function and file uses; imports, including `tsconfig.json` path aliases |

A string in code counts as a path only if it contains a leading segment some service in the manifest serves, such as `/v1/`; everything before it (a base URL, `${API}`) is dropped. Paths compare in one form, so `{id}`, `${id}`, `:id`, `[id]` and `%s` are the same parameter. Test files in consumers are skipped.

## Findings

| Requirement | Severity | Meaning |
| --- | --- | --- |
| `consumer_calls_removed_operation` | error | A consumer calls an operation the change removed. |
| `consumer_calls_unserved_operation` | error | A changed consumer adds a call to a path under a service's prefixes that no service serves, such as an endpoint not yet deployed. |
| `consumer_misses_new_required_parameter` | error | An operation a consumer calls now requires a parameter. |
| `consumer_misses_new_required_field` | error | An operation now requires a request field the consumer's code never names. |
| `consumer_sends_removed_parameter` | error | A consumer names a parameter the change removed, and the operation rejects parameters it does not know. |
| `consumer_sends_ignored_parameter` | warning | As above, where the operation ignores unknown parameters. |
| `consumer_sends_rejected_field` | error | A consumer names a request field the change removed, and the operation rejects fields it does not know: its spec says `additionalProperties: false`, its route's zod schema is `.strict()`, or its service sets `rejects_unknown_fields`. |
| `consumer_uses_removed_request_field` | warning | A consumer names a request field the change removed or renamed; the operation now ignores it, so whatever it set no longer takes effect. |
| `consumer_uses_removed_response_field` | warning | A consumer names a response field the change removed or renamed. |
| `client_declares_removed_parameter` | error | A generated client still declares a parameter the service removed. |
| `client_calls_removed_operation` | error | A client function calls an operation the change removed. |
| `consumer_calls_changed_operation` | info | A consumer calls an operation whose request changed, but never names the changed field. |
| `route_not_in_spec` | info | The service serves a route its spec does not declare. Public specs often leave internal routes out on purpose. |

"Names a field" means the field's last name appears as an object key, member, keyword argument, dictionary key, struct field or JSON tag, in the calling function itself or in declarations outside functions (schemas, types, constants) in its file or a file it imports. Other functions of the same file do not count, since they often reuse common names for other things. Findings that rest on a name being absent, such as a new required field the consumer never sets, search the whole file and its imports instead. It is evidence, not proof: a consumer that builds requests dynamically can use a field without naming it, and a common name can appear for another reason. Without that evidence a field change is informational, so a change most consumers never touch does not produce a wall of errors.

`lemma verify` exits with 4 if any finding is an error, so CI can gate on it. `--json` prints the findings with their values. `--relation calls` also prints every row of a derived relation, here which consumer calls which operation, for inspecting what the rules concluded.

## Grammars

Requirements live in a grammar, not in Lemma's code. The built-in one is [`grammars/http-contracts.yaml`](../grammars/http-contracts.yaml); copy it and point a manifest's `grammar` at the copy to add or change requirements. See [Grammars](grammar.md).

## Engines

Rules run on [Soufflé](https://souffle-lang.github.io) when it is installed (`brew install souffle`), otherwise on Lemma's built-in reference engine; `--engine souffle` or `--engine reference` picks one. Both give the same results. Soufflé's numbers are 32-bit; Lemma reports values that do not fit instead of truncating them.

## Limits of this version

- Only HTTP contracts described by OpenAPI.
- Field changes compare names, not types or enum values: a field whose allowed values change is not reported.
- Calls whose URL is built at run time from values the code does not show are not matched to an operation.
- "Names a field" cannot tell a field read from a response apart from a consumer's own object that reuses the name, so warnings about common names (`id`, `slug`) can be wrong.
- Only direct contracts are checked: a change is traced to the callers of the changed service, not on through services that pass its data along.
- Consumers are matched to operations by path and method; a consumer is reported against the operation it was written for (the base snapshot).

See the [pre-release checklist](pre-release.md) for the full list.
