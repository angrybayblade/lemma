# `.lm` description reference

The full description is in [Check links](https://github.com/angrybayblade/lemma/blob/main/docs/check.md) and [The `.lm` language](https://github.com/angrybayblade/lemma/blob/main/docs/language.md). This is the field list.

A repository's root module, `mod.lm`:

```lm
module billing

import * as http from lemma.http
import * as hono from lemma.hono
import * as typescript from lemma.typescript
import * as json from lemma.json
import { Link } from lemma.links
import { orders } from other

repo other = git(url: "git@github.com:example/other.git", rev: "3f2a1c9e04")   // or local("../other"), local("../other", rev: "...")

let handlers = typescript.file("src/handlers.ts")
let spec = json.command("{python} scripts/spec.py", output: "spec.json", env: { KEY: "value" })

export service api {
    runtime: {                                 // interpreters and toolchains for commands
        python: ".venv/bin/python"             // relative paths are the repository's own environment
        node: "node"
        bun: "bun"
        go: "go"
        cargo: "cargo"
        env: { PYTHONPATH: "src" }             // environment for every command
    }
    index: {                                   // what the service is made of: indexed and searched
        include: ["src"]
        exclude: ["**/generated/**"]
        languages: ["typescript"]
    }
    operations: [
        http.openapi("openapi.json"),          // documents it serves: calls to them are inferred
        hono.app(                              // optional: read operations from the code, not committed documents
            entry: ["src/index.ts#app", "src/routes"],
            directory: "apps/api",             // where it runs
            setup: "npm ci",                   // prepares Lemma's worktree of a commit no one has read clean
        ),
    ]
    symbols: ["src/index.ts"]                  // a library's public modules
    routes: {                                  // optional, rarely needed with an adapter: the calls routes are written with
        register: ["openapi"]                  // take a route and its handler: router.openapi(DEF, handler)
        define: ["createRoute"]                // only describe a route, passed to a register call later
    }
    rejects_unknown_fields: false

    export operation charge {
        implementation: handlers.symbol("charge")       // must resolve: `Charges.Create`, `create_charge`
        request: handlers.type("ChargeRequest")         // a type in code
        response: spec.at("$.charge.response")          // in the command's output
    }

    export client operation notify {
        implementation: handlers.symbol("notify")
        sends: handlers.type("Notice")
        accepts: handlers.type("Ack")
    }
}

export service client_sdk {
    client: http.generated_client(             // only if this service IS a generated client of another;
        generated_from: ["openapi.json"],      // its code is then not searched for calls
        call_names: ["{camel}"],
        specs: ["openapi.json"],
    )
}

export graph links : Link {
    api.notify -> orders.receive_notice
}
```

Other adapters: `fastapi.app(entry: ["app.main:app"])`, `http.adapter("express-routes", entry: [...])`, and `http.adapter_command(command: "...", entry: [...])`.

A schema can also be a document: `json.document("spec.json", format: "openapi").at("$...")`. `json.command(..., cache: false)` re-runs a command whose output depends on more than the code.

A workspace:

```lm
module system

workspace {
    repo billing = local("../billing")
    repo web = local("../web")
    config: { level: check }                   // audit, check or strict
}
```

## Names and addresses

- A repository's name is its root module's, an identifier: the directory `web-app` holds `module web_app`, and findings print `#web_app`.
- In a graph, a link's end is `service.operation`, by `operationId` for a documented operation, or by method and path: `api["POST /v1/charges"]`. Services in other repositories come in through `import { api } from billing`.
- Findings and commands write an operation as `services.<service>.<operation>`, or `#<name>:services.<service>.<operation>` in another repository.
- A renamed operation says what it used to be: `previously: ["billing::api.cancel"]`.

## Rules Lemma enforces

- A repository's root module declares `module <name>`, with the name its `repo` declarations use.
- A service in another repository is reached through `import`, and a `repo` declaration says where its repository is.
- Unknown fields are rejected.
