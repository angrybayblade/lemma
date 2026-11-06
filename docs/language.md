# The `.lm` language

`.lm` modules say what `lemma.yaml` files and grammar files say, in one typed language: a repository's services and operations, the links between them, and the relations, rules and requirements `lemma verify` checks. Lemma checks a module before it uses it, so a misspelled operation or a rule reading a relation that does not exist is reported with its line, instead of quietly checking nothing.

`lemma.yaml` and YAML grammars keep working. Both forms lower to the same structures, so a check gives the same findings from either. The design, including the parts not built yet (behaviors, checkers and evidence), is in [spec 13](../.specs/13-lm-grammar.md).

## A repository's module

Each repository has a root module, `lemma.lm`, named after the repository:

```lm
module billing

import * as http from lemma.http
import * as python from lemma.python
import * as json from lemma.json

let handlers = python.file("billing/handlers.py")
let spec = json.command("{python} scripts/spec.py")

export service api {
    runtime: { python: ".venv/bin/python" }
    index: { include: ["billing"] }
    operations: http.openapi("openapi.json")

    // An operation documents do not describe: a queue message handler.
    export operation create_charge {
        implementation: handlers.symbol("create_charge")
        request: spec.at("$.create_charge.request")
        response: spec.at("$.create_charge.response")
    }
}
```

A caller declares its outbound call as a `client operation` and links it to the provider:

```lm
module web

import * as go from lemma.go
import { Link } from lemma.links
import { api } from billing

repo billing = git(url: "git@github.com:example/billing.git", rev: "3f2a1c9e04")

let charges = go.file("client/charges.go")

export service web {
    index: { include: ["src", "client"] }

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

A workspace composes repositories, as a parent `lemma.yaml` does:

```lm
module system

workspace {
    language: "0.2"
    repo billing = local("../billing")
    repo web = local("../web")
    config: { level: check }
}
```

| `lemma.yaml` | `.lm` |
| --- | --- |
| `"#billing": { git: ..., ref: ... }` | `repo billing = git(url: "...", rev: "...")` |
| `"#billing": { path: ../billing }` | `repo billing = local("../billing")` |
| `exports.openapi: [openapi.json]` | `operations: http.openapi("openapi.json")` |
| `exports.operations_from` | `operations: hono.app(entry: [...])`, `fastapi.app(...)`, `http.adapter("express-routes", ...)` or `http.adapter_command(...)` |
| `path: client/charges.go:Charges.Create` | `implementation: charges.symbol("Charges.Create")` |
| `request: client/charges.go#CreateChargeRequest` | `request: charges.type("CreateChargeRequest")` |
| `resolver` and `request: { path: "$.x" }` | `request: spec.at("$.x")` with `let spec = json.command("...")` |
| `exports.client` | `client: http.generated_client(generated_from: [...])` |
| `graph.links` | `graph links : Link { caller -> provider }` |
| `graph.level` | `config: { level: check }` in the workspace |

Names are bound, not matched as text. An import names a module (`import { api } from billing`), a module's name is fixed by its file's place in its repository (`checks/replay.lm` in `billing` is `billing.checks.replay`), and a repository's name is its root module's. A service that only groups other services passes its fields down to them.

## Grammars

The built-in HTTP contracts grammar is the standard-library module `lemma.http`. A grammar of your own imports it and adds relations, rules and requirements:

```lm
module contracts

import { calls, removed_operation } from lemma.http

rule removed_endpoint(op: Text) {
    (op) :- removed_operation(op)
}

export require consumer_calls_deprecated_removal {
    severity: warning
    when: calls(c, op, f, l), removed_endpoint(op)
    message: "{c} calls {op}, which was removed ({f}:{l})"
}
```

Name the module in a `lemma verify` manifest (`grammar: contracts.lm`). A `check main { run ... }` in it selects which requirements run; without one, every requirement of every loaded module runs. Lowercase names in rules are variables, and uppercase names, dotted paths and literals are constants. See [Grammars](grammar.md) for what the relations mean.

## Commands

- `lemma lm check [lemma.lm]` loads a module and everything it imports, and lists every problem as `file:line:column`.
- `lemma lm fmt [paths...] [--check]` rewrites `.lm` files in one canonical layout, keeping line breaks and comments; with `--check`, it lists the files that are not formatted instead. With no paths, it formats the `.lm` files under the current directory.
- `lemma lm convert [lemma.yaml] [--module name] [--write]` writes a `lemma.yaml` as a root module. With `--write`, it writes `lemma.lm` beside the file.
- `lemma lm lsp` serves modules to an editor (diagnostics as you type, hovers, go to definition, symbols, completion).
- `lemma check` reads `lemma.lm` when a repository has no `lemma.yaml`, and takes a `.lm` workspace module as its argument.

## Editors

`editors/` has a Tree-sitter grammar with highlighting, folding and indentation queries (Neovim, Helix, Zed), and a VS Code extension that highlights `.lm` files and starts `lemma lm lsp`. Setup steps are in [editors/README.md](../editors/README.md).

## Not built yet

- `behavior` and `checker` declarations are checked but not run, so behaviors give no evidence yet.
- `allow` declarations are checked but not applied to findings.
- A link to a discovered operation names it by its `operationId` (`Gateway.listUsers`); naming it by method and path is reported.
