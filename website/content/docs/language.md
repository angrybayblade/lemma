# The `.lm` language

`.lm` is the language of descriptions: one typed language for a repository's services and operations, the links between them, and the relations, rules, requirements, allowances and behaviors `lemma check` runs. Lemma checks a module before it uses it and reports the line of a misspelled operation or of a rule reading a relation that does not exist, instead of quietly checking nothing.

The design is in [spec 13](../.specs/13-lm-grammar.md), and every function and declaration of the standard library is in [its reference](standard-library.md).

## Repository modules

Each repository has a root module, `mod.lm`, named after the repository:

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

A workspace composes repositories:

```lm
module system

workspace {
    language: "0.2"
    repo billing = local("../billing")
    repo web = local("../web")
    config: { level: check }
}
```

Repositories are `local("../billing")`, a working directory; `local("../billing", rev: "3f2a1c9e04")`, a clone read at a commit; or `git(url: "...", rev: "...")`, cloned into Lemma's cache. A link's ends name operations, by `operationId` (`api.createCharge`) or by method and path (`api["POST /v1/charges"]`). A renamed operation says what it used to be: `previously: ["billing::api.cancel"]`.

[Check links](check.md#descriptions) lists a service's fields, the operations it declares and where schemas come from.

Names are bound, not matched as text. An import names a module (`import { api } from billing`), a file's place in its repository fixes its module's name (`checks/replay.lm` in `billing` is `billing.checks.replay`), a directory's module is its `mod.lm` (`checks/mod.lm` is `billing.checks`), and a repository's name is its root module's, the `mod.lm` at its root, so it is an identifier: a directory `web-app` holds `module web_app`, and findings print `#web_app`. A service that only groups other services passes its fields down to them.

## Custom requirements

Lemma always loads the standard-library module `lemma.http`, which holds the built-in HTTP contract requirements. A module of your own imports it and adds relations, rules and requirements:

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

Write it in a workspace's root module, or in a module the root module imports, and `lemma check` runs its requirements beside `lemma.http`'s. A `check main { run ... }` selects which requirements run; without one, every requirement of every loaded module runs. Lowercase names in rules are variables, and uppercase names, dotted paths and literals are constants. See [Requirements and rules](grammar.md) for what the relations mean.

### Allowances

An allowance covers a known finding that should not fail the check, with a reason and, optionally, a date after which the finding counts again:

```lm
module contracts

import lemma.http

allow lemma.http.consumer_calls_removed_operation {
    when: f == "src/legacy/sessions.ts"
    reason: "the legacy client is removed with the next major version"
    until: "2027-01-31"
}
```

`when` reads the requirement's message variables. Lemma reports a covered finding as allowed and informational, with the reason. It compares `until` with the committer date of the newest commit the check reads (a change's head), never the clock, so the same commits always give the same answer; past it, the findings count again and Lemma reports the allowance as expired. Lemma reports an allowance for a behavior that ran and covered none of its cases, so stale ones get removed. It evaluates requirements only over what a change touches, so it does not report allowances for them.

## Behaviors and checkers

A behavior is a requirement checked by running statements. It may demand evidence from a checker, a command that exercises the code and reports what it observed:

```lm
module platform

import * as python from lemma.python
import { Link } from lemma.links
import { Functions } from functions

repo functions = local("../functions")

let client = python.file("worker/client.py")
let handlers = python.file("worker/handlers.py")

export service Worker {
    client operation call_setup_webhook {
        implementation: client.symbol("setup_webhook")
    }

    export operation setup_monitor {
        implementation: handlers.symbol("setup_monitor")
    }
}

export graph links : Link {
    Worker.call_setup_webhook -> Functions.setup_webhook
}

export checker replay_setup_error(
    handler: Operation,
    through: Fact<Link>,
    upstream_retriable: Bool
): Evidence<Json> {
    command: ["python3", "tools/replay.py"]
    timeout_ms: 60000
}

export behavior forwards_upstream_retriable {
    let link = links[Worker.call_setup_webhook, Functions.setup_webhook]
    expect link.exists

    for flag in [true, false] {
        let trace = need replay_setup_error(
            handler: Worker.setup_monitor,
            through: link,
            upstream_retriable: flag
        )
        expect trace.http.status == 200
        expect trace.http.body.retriable == flag
            because "the worker passes on the retriable flag the functions service set"
    }
}
```

- Each iteration of a `for` is a case, named by the behavior and its loop values (`platform::forwards_upstream_retriable[flag=true]`). A failed `expect` ends its case; sibling cases still run. The behavior's outcome is the worst of its cases: `violated`, `unknown`, `stale` or `satisfied`.
- `need` takes the checker's evidence, or ends the case as unknown when there is none. Reading a JSON field that is absent gives a missing value, and an `expect` on it fails saying so (`trace.http.body.retriable` is missing).
- A behavior that ran no `expect` is unknown, unless it says `allow_empty: true`. One that loops over facts Lemma reads from code is at best unknown, since behaviors read only stated graphs and the rules over them for now.
- A checker runs `command` without a shell in its repository at the snapshot, in a worktree for a commit. `LEMMA_INPUT` names a JSON file with its arguments: operations by address with their implementation's file and symbol, facts by relation and values, and each repository's checkout. It writes `{ "observed": ... }` or `{ "unavailable": "why" }` to the file `LEMMA_OUTPUT` names. Spec 13 section 10 has the whole protocol.
- Lemma keeps evidence in its cache under a key of the checker's declaration, its arguments, the state of the code it reads (Git trees, plus changed files in a working directory) and its environment, and reuses it while that key holds. `lemma check --no-run` uses kept evidence only: a case whose code changed is `stale`, one never run is `unknown`.
- A behavior with parameters runs from a check with arguments: `run forwards(field: "retriable")`.

## Checks and levels

```lm
check main {
    run lemma.http.*
    run platform.forwards_upstream_retriable { severity: warning }
    skip lemma.http.consumer_sends_ignored_parameter
}
```

A workspace's `check main` selects the requirements and behaviors `lemma check` runs; `lemma check --select <name>` runs another check. Without one, every requirement of every loaded module runs, and every behavior without parameters. Lemma always loads `lemma.http`, so a `check main` that names only behaviors runs no HTTP contract check; add `run lemma.http.*` to keep them.

The level (the workspace's `config: { level: ... }` or `lemma check --level`) decides what fails: `audit` fails on nothing, `check` fails on error-severity findings and violated behaviors, and `strict` also fails on error-severity behaviors that are unknown or stale, and on code an adapter could not read, whose committed documents stood in for it. With a change, only what the change introduced counts: a behavior's case counts unless the base, run on kept evidence, ended it the same way. The report also lists requirements and behaviors the change added, removed or changed.

## Commands

`.lm` files have their own command, `lm`, beside `lemma`: it works with modules, and `lemma` checks and verifies what they declare.

- `lm check [mod.lm]` loads a module and everything it imports, and lists every problem as `file:line:column`.
- `lm fmt [paths...] [--check]` rewrites `.lm` files in one canonical layout, keeping line breaks and comments; with `--check`, it lists the files that are not formatted instead. With no paths, it formats the `.lm` files under the current directory.
- `lm new behavior <name> --handler <operation> [--through <link>]` adds a checker, a behavior and the checker's script for one of the module's operations, to fill in from `TODO`s ([tutorial 4](tutorials/4-behaviors.md#start-from-a-template)).
- `lm lsp` serves modules to an editor (diagnostics as you type, hovers, go to definition, symbols, completion, formatting).
- `lemma check [mod.lm]` checks a change against what a module declares, and runs its requirements, behaviors and allowances (`--select`, `--no-run`, `--level strict`).
- `lemma verify [mod.lm]` compares a description with the code at one snapshot ([Verifying descriptions](verify.md)).

## Editors

`editors/` has a Tree-sitter grammar with highlighting, folding and indentation queries (Neovim, Helix, Zed), and a VS Code extension that highlights `.lm` files and starts `lm lsp`. Setup steps are in [editors/README.md](../editors/README.md).

## Not built yet

- Behaviors read stated graphs and the rules over them; facts Lemma reads from code (`calls` and the rest of `lemma.http`) are not available to them yet.
- Evidence stays on the machine that ran the checker; copying the cache's `evidence/` directory carries it.
- Operations an adapter discovers cannot be linked yet, and `previously` applies to links only.
- `lemma check`'s link checks (persistence and schema agreement) are built in, so allowances cannot name them.
