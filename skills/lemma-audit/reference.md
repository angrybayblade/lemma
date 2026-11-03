# lemma.yaml reference

The full description is in [Checking links](https://github.com/angrybayblade/lemma/blob/main/docs/check.md). This is the field list.

```yaml
services:
  <service>:                          # a service defined in this repository
    runtime:                          # interpreters and toolchains for commands
      python: .venv/bin/python        # relative paths are the repository's own environment
      node: node
      go: go
      cargo: cargo
      env: { PYTHONPATH: src }        # environment for every command
    index:                            # what the service is made of: indexed and searched
      include: [src]
      exclude: ["**/generated/**"]
      languages: [typescript]
    exports:                          # what it offers: entry points for queries and links
      openapi: [openapi.json]         # documents it serves: calls to them are inferred
      operations:
        <operation>:
          path: <file>:<Symbol>       # must resolve: `Charges.Create`, `create_charge`
          schema:
            resolver:                 # optional: a command whose JSON output the locations are in
              command: "{python} scripts/spec.py"
              output: spec.json       # its standard output if absent
              env: { KEY: value }
            request: <file>#<Type>    # a type in code
            response: { path: "$.op.response" }                # in the resolver's output
            # or: { file: spec.json, path: "$...", format: openapi }
      symbols: [src/index.ts]         # a library's public modules
    path_prefixes: [/v1/billing/]
    route_aliases: { /v1/: /v2/ }
    rejects_unknown_fields: false
    client_receivers: [client]
    client_modules: [generated/billing]
    client:                           # a generated client of another service
      generated_from: [openapi.json]
      call_names: ["{camel}"]
      specs: [openapi.json]
  "#<name>":                          # an external service: another repository
    path: ../other                    # or git: <url>
    ref: <commit, branch or tag>
    manifest: lemma.yaml              # alone: another lemma.yaml in this repository
graph:
  links:
    - services.<service>.<operation> -> "#<name>:services.<service>.<operation>"
  level: audit                        # or check
```

## Addresses

- `services.<service>.<operation>` names an operation in the same `lemma.yaml`.
- `#<name>:services.<service>.<operation>` names one in an external service's.
- `services.<service>.operations.<operation>` is accepted too.
- A documented operation is named by its `operationId`.

## Rules Lemma enforces

- A `#name` service gives `git`, `path` or `manifest`, not both `git` and `path`, and defines nothing.
- A service without `#` gives no `git`, `path`, `ref` or `manifest`.
- Unknown fields are rejected.
