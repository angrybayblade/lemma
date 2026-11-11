# Behavior harnesses and language packs

**Status: parked proposal, 2026-11-10.** Nothing here is implemented, and work is paused until the [open questions](#open-questions) are taken up again. The rows marked *decided* under [Decisions](#decisions) were settled in discussion. Statements marked *observed* were run on 2026-11-10 against small probes, on Bun 1.4.4, Node 24.11 and Go 1.27.1. Statements marked *surveyed* come from reading a production system of four repositories: a Go service, a Python functions service it calls, a TypeScript API built on Next.js with one Hono router per route file, and a Next.js dashboard. The system is not named here. Lemma's fixtures (Hono, Express, FastAPI, Flask, Go) were read too.

This builds on behaviors and checkers ([document 13](13-lm-grammar.md), sections 9 and 10) and changes neither their semantics nor the checker protocol.

## Problem

A behavior proves a runtime claim by running code: run an operation's handler, answer one of its outbound calls with a chosen value, and observe what the handler returns. The checker protocol (`LEMMA_INPUT`, `LEMMA_OUTPUT`) is already language-neutral. What is Python-only is the code that does the running. `lm new behavior` writes a complete checker script only for Python. For TypeScript it leaves the replaced call as a `TODO`, and for Go it writes a script that exits 1 ([pre-release](../docs/pre-release.md#behaviors-beta)).

Even the Python script works only on code shaped like the tutorial's: a module-level function called as `module.function(...)`, and a handler that takes one dict. The survey shows that real code in all three languages is shaped differently. And the knowledge of each language is compiled into Lemma's scaffolder, so supporting one more language means a Lemma release.

## Decisions

| Decision | State |
| --- | --- |
| Lemma core defines an abstract interface for running a case, the replay case, and ships no harness for any language | *decided* |
| Harnesses come in language packs that users install as they need them | *decided* |
| Each step is incremental and configurable: a language pack declares what it supports, and workspaces and services choose | *decided* |
| Heuristic or non-exhaustive techniques, and performance work, come later; the first versions do exactly what is declared and report what they covered | *decided* |
| A behavior names no language pack; Lemma picks one from the handler's language and the configuration | proposal |
| A missing language pack makes the case unknown, never an error | proposal |

## What running a case takes

Every harness, whatever its language, has four jobs:

1. **Prepare.** Make the handler's code importable or buildable: interpreter, module root, path aliases, environment variables, and anything the code connects to at import.
2. **Enter.** Invoke the operation's handler with the case's input.
3. **Replace.** Answer the outbound call named by a link with the case's value instead of sending it, and block any call nobody chose to answer.
4. **Observe.** Report the handler's response, each replaced call with what was sent, and how the run was done (the coverage).

The language decides how each job can be done. The service decides which way works.

## What the target code looks like

*Surveyed.*

| | Go service | Python functions service | TypeScript API |
| --- | --- | --- | --- |
| Handler | A struct implementing `ServeHTTP`, built by a constructor that takes a database client, a struct of service clients and a feature-flag client; wired by a DI container | A class per operation, registered by a metaclass; a classmethod `_execute(data, tool, executables)`; dispatched through an event handler that validates a payload | `router.openapi(def, async (c) => ...)`, one router per route file under `pages/api`, with no assembled app; auth read from ambient context storage |
| Outbound call | A generated client per operation: a concrete struct with an `Invoke` method, all sharing one transport that is either the AWS Lambda SDK or HTTP POST to one URL, chosen by an environment variable, with the operation named in the body | An inherited classmethod `http_request` on a mixin, about 39,000 call sites; below it `requests.request`, or a session built inside the function when an SSRF guard is on | A generated client over axios, used as `client.fn(...)` from a module-level namespace; global `fetch` through wrappers; SDK clients held privately by modules |
| What importing does | Nothing notable | Builds a config object from the environment; reloads or `exec`s app modules on every call | Validates about 7 mandatory environment variables and throws without them; connects to Redis at import; starts a feature-flag connection |
| How its own tests fake calls | Point the transport at `httptest.NewServer` through the environment variable (9 files); one interface with a hand-written fake (1 file) | `patch.object` on classes, mostly `http_request` (812 files); patch the name where it is used, not where it is defined | `vi.mock` (517 files) and `vi.spyOn` (153); `fetch` stubs (10); msw (4) |
| Where today's Python approach fails | No runtime patching; every client is a concrete type | `from x import f` binds a copy; methods are inherited; app modules are reloaded; the transport changes with configuration; `asyncio.run` hides inside sync handlers | ESM named imports cannot be reassigned from outside; the code runs only through Next.js or Vite, which resolve `@/` aliases |

The fixtures are the easy case. Their handlers are plain functions and their calls go through `requests.*` or global `fetch`.

## Findings

1. **The hard part is entering, not replacing.** No service in the survey has a handler you can call as `handler(request)`. Each needs either its framework's in-process request path or a constructor with real dependencies. Every team has already written this once, in its own tests: an in-memory SQLite database client for the Go handlers, `testClient(route)` for the TypeScript routes, `Serverless.invoke(payload)` in Python. A harness should reuse that setup, written once per service, not once per behavior.
2. **Replacing the client operation's symbol is the precise seam, and every language can reach it if the harness works at the right level.** *Observed:*
   - **Python:** patching where a name is looked up, and on the class that defines a method, reaches every call site in the survey. The functions service's own tests already do this.
   - **TypeScript under Bun:** `mock.module` from `bun:test` replaced a named ESM export that the handler had already imported, under a plain `bun run`. Patching a class prototype also worked.
   - **TypeScript under Node 24:** `mock.module` from `node:test` works only behind `--experimental-test-module-mocks`, and only when it is installed before the handler's module is imported. Node also refused the extensionless relative imports that TypeScript source uses, so source code needs a loader.
   - **Go:** `go test -overlay` replaced the body of a concrete client method held in an unexported field, and added a generated `_test.go` to the handler's package. Nothing was written into the working tree.
3. **Each language also has a transport-level seam, and teams already use it.** The Go service's tests use an environment variable plus `httptest`. The TypeScript generated client accepts an axios adapter, and global `fetch` can be replaced. Below the mixin, the Python service has `requests.request`. Replacing at this level also exercises the real client's encoding and error mapping. That is more faithful, but the fake must recognize the call on the wire, and on the wire a call is not always the provider's OpenAPI method and path: the Go service's calls all go to one URL, with the operation named in the JSON body.
4. **A replacement that is never reached must be reported, not silent.** A Python patch on the defining module misses a `from` import, and a patch on `requests.request` misses the pinned session. In both cases the real call runs, and the case may still hold. A harness has to count each replacement's hits and block every unanswered outbound call. This is also what lets a first version be simple: a gap it does not cover shows up as a missed replacement, not as a false pass.
5. **Harnesses must leave the repository's state alone.** Evidence is keyed on the state of the code. The tutorial already needs a `.gitignore` for `__pycache__`, because a run otherwise changes the state it is keyed on. Go overlays, `PYTHONDONTWRITEBYTECODE` and temporary files outside the tree avoid the problem.

## Proposal

### Layers

| Layer | What | Provided by |
| --- | --- | --- |
| Checker protocol | Any command; `LEMMA_INPUT` in, `{ observed }` out | Lemma core; exists, unchanged |
| Replay case | The abstract interface: one schema for "enter this operation with this input, answer these links with these values", and one shape for what was observed. The same for every language, versioned as `replay/1`, `replay/2`, ... | Lemma core |
| Language pack | An installable directory with a manifest and a harness that speaks the replay case for one language, optionally narrowed to frameworks | Installed by the user: Lemma's own language packs, a third party's, or the team's |

Lemma core knows the replay case, how to install, find and choose a language pack, and how to test one against the interface. It knows nothing about Python, TypeScript or Go beyond the file extensions that name a handler's language. The language knowledge in `crates/lemma-lm/src/scaffold.rs` moves out of core into the language packs.

### The replay case

A harness receives the checker protocol's input (document 13, section 10). Its arguments follow this schema:

```json
{
  "handler": { "operation": "...", "implementation": { "file": "...", "symbol": "..." } },
  "input": { "customer": "c_1" },
  "replace": [
    { "through": { "relation": "lemma.links::Link", "values": { "caller": { ... }, "provider": { ... } } },
      "answer": { "error": "card declined", "retriable": true } },
    { "through": { ... }, "raise": { "type": "Timeout", "message": "..." } }
  ],
  "strategy": { "enter": "setup", "replace": "symbol" },
  "setup": "tests/lemma_setup.py",
  "options": { }
}
```

It writes:

```json
{
  "observed": {
    "response": { "status": 502, "body": { ... } },
    "raised": null,
    "calls": [ { "through": "orders.charge -> api.createCharge", "sent": [ ... ], "answered": true } ],
    "blocked": [ { "kind": "http", "target": "POST https://..." } ]
  },
  "coverage": {
    "enter": "setup",
    "replacements": { "orders.charge -> api.createCharge": { "strategy": "symbol", "hits": 1 } }
  },
  "toolchain": { "language_pack": "python@0.1.0", "python": "3.14.0" }
}
```

Or, for anything it cannot do, `{ "unavailable": "this language pack does not support replace: transport" }`.

- `response` is what the handler returned. With the `setup` entry it is `{ status, headers, body }`, so `expect trace.response.status == 502` reads the same in every language. `raised` is set when the handler threw.
- `calls` records each replaced call, with its arguments as JSON. This is how evidence correlates the intercepted call with the response, as document 13 requires; echoing the inputs back would not.
- Outbound calls that nobody chose to answer are blocked and listed in `blocked`. By default they fail the call they were made from. This keeps runs deterministic and offline.
- Coverage goes to the existing `coverage` field: it is kept with the evidence but is not part of its key.
- `options` is passed through untouched from the service's configuration, for anything a language pack wants configured that the replay case does not name.

**Versioning.** A language pack declares the replay case versions it speaks. A new version only adds fields; a harness ignores fields it does not know. Lemma sends the highest version both sides speak, and refuses a language pack that speaks none of Lemma's.

### Strategies

Each job has more than one strategy. The replay case names them; a language pack implements the ones it chooses and lists them in its manifest. One that is asked for a strategy it lacks answers `unavailable`, so the case is unknown and says why.

| Job | Strategy | How | Faithfulness | Setup |
| --- | --- | --- | --- | --- |
| Enter | `call` | Call the implementation's symbol with `input` | Lowest: skips routing, decoding and middleware | None |
| Enter | `setup` | The service's setup file builds the handler or app; the harness sends `input` through it in process | High | One file per service |
| Enter | `process` | Start the service and send a real request | Highest | A start command, ports, everything it connects to |
| Replace | `symbol` | Replace the client operation's implementation | Skips the client's encoding | None |
| Replace | `transport` | Answer at the HTTP layer inside the process | Runs the real client | A matcher when the wire does not show the operation |
| Replace | `server` | A fake provider serves the call, and the service's base-URL variable points at it | Runs the real client and transport | The base-URL variable's name |

`process` and `server` do not depend on the handler's language. They are language packs too (an `http` language pack, say), not part of core, and they serve Rust services, for which no other language pack is planned.

### Language packs

A language pack is a directory with a `language-pack.json` manifest:

```json
{
  "name": "python",
  "version": "0.1.0",
  "language": "python",
  "replay": ["replay/1"],
  "harness": { "command": ["{python}", "harness.py"] },
  "strategies": { "enter": ["call", "setup"], "replace": ["symbol"] },
  "templates": { "setup": "templates/lemma_setup.py" }
}
```

- `language` is what it runs: `python`, `typescript`, `go`, or `any` for one, such as `http`, that does not care. A narrower one adds `frameworks`, such as `["django"]`, so that it can be chosen over a general one.
- `harness.command` runs without a shell. `{python}`, `{bun}`, `{node}` and `{go}` are the service's runtimes, as adapters' commands use `{python}` today. A language pack never installs anything into the repository it runs on.
- `strategies` lists what it supports, for `lemma language-pack list` and for messages. The harness's own `unavailable` answer is what counts.
- `templates` are what `lm new behavior` copies for a service that has no setup file yet, so the scaffolder needs no language knowledge.

**Installing.** Language packs live in Lemma's data directory (`$XDG_DATA_HOME/lemma/language-packs/<name>/<version>`, or `~/.local/share/lemma/...`), not in its cache, since a cache may be cleared.

```console
$ lemma language-pack add python          # Lemma's own, latest release
$ lemma language-pack add python@0.1.0    # pinned
$ lemma language-pack add ./packs/python-acme
$ lemma language-pack add git+https://example.com/acme/lemma-django.git@v2
$ lemma language-pack list
$ lemma language-pack remove python@0.1.0
$ lemma language-pack sync                # install what the workspace pins
$ lemma language-pack test ./packs/python-acme
```

Lemma's own language packs are released apart from Lemma, as archives with a published digest. Lemma's binary carries none of them.

**Choosing one.** For each `replay` demand, Lemma picks a language pack:

1. The service's `harness.language_pack`, if set.
2. Otherwise, the installed one whose `language` matches the handler's file and whose `frameworks`, if any, the service declares; the narrowest wins.
3. The version is the workspace's pin, else the newest installed.

When none is installed, the case is unknown, with the reason and the command that fixes it: `no language pack for python; run lemma language-pack add python`. At level `check` that reports; at `strict` it fails, as unknown cases do today.

**Evidence.** The key includes the chosen language pack's name, version and a digest of its files. Upgrading a language pack runs its cases again; evidence from another language pack is never reused.

**Testing a language pack.** `lemma language-pack test` runs a conformance suite of fixture systems against it, one per language and per strategy the manifest lists: cases that must hold, cases that must be violated, an unanswered call that must be blocked, and a replacement that must report zero hits when it is not reached. A strategy that is not listed is not tested, so a language pack can pass while supporting only `call` and `symbol`.

### Configuration

Every level is optional, and each one narrows the one above it.

```lm
// The workspace pins versions, so every machine and CI run the same harness.
workspace {
    repo orders = local("orders")
    language_packs: { python: "0.1", go: "0.2" }
}
```

```lm
// The service says how its code is prepared and entered, once.
export service orders {
    runtime: { python: ".venv/bin/python" }
    harness: {
        language_pack: "python"          // optional: chosen from the handler's file otherwise
        setup: "tests/lemma_setup.py"    // enables the setup entry
        enter: "setup"                   // default strategies for this service
        replace: "symbol"
        env: { SKIP_VALIDATION: "true" } // part of the evidence key, as a checker's env is
        timeout_ms: 60000
        options: { reapply_on_import: true }  // passed to the language pack as is
    }
}
```

```lm
// The behavior says only what the case is. It overrides a strategy when it needs to.
import { replay } from lemma.replay

export behavior passes_on_retriable {
    let link = links[orders.charge, api.createCharge]
    expect link.exists

    for retriable in [true, false] {
        let trace = need replay(
            handler: orders.place_order,
            input: { customer: "c_1" },
            replace: [{ through: link, answer: { error: "card declined", retriable: retriable } }]
        )
        expect trace.calls.len() == 1
            because "the declined charge must come from the replaced call"
        expect trace.response.body.retriable == retriable
    }
}
```

`lemma.replay` is a small standard-library module holding the `replay` checker and the types of its arguments. It names no language: the same behavior runs whichever language pack is chosen. Hand-written checkers keep working as they do today; `replay` is one more checker, not a replacement for them.

The setup file follows its language's convention and needs no library:

| Language | Convention |
| --- | --- |
| Python | A module with `def lemma_setup():` returning the callable or app |
| TypeScript | A module exporting `lemmaSetup()`, returning a handler, a Hono app or a `fetch` function |
| Go | A file tagged `//go:build lemma` with `func LemmaSetup() http.Handler` in the handler's package; the routes adapter already builds files tagged `lemma` |

### First versions of the language packs

Each first version supports the strategies in its row and nothing heuristic. It replaces exactly the places it is told to, and reports hits, so what it misses is visible.

| Language pack | First version | Later |
| --- | --- | --- |
| `python` | `call` and `setup` (any callable or app the setup returns; coroutines run with `asyncio.run`); `symbol` on the implementation's own module and, for `Class.method`, on its class; async stubs for async originals | Rebinding every reference to the original by identity, to reach `from` imports and aliases; reapplying patches when modules are reloaded or `exec`d; `transport` for `requests`, `httpx`, `urllib3` and `aiohttp` |
| `typescript` | Bun only; `setup` through `request`, `fetch` or `(req, res)`; `symbol` through `mock.module` and prototype patching (*observed*) | `transport` for `fetch` and axios; Node through a `--import` loader; running inside the repository's own test runner (`vitest run`) for code that only Vite or Next.js can resolve |
| `go` | `setup` through `LemmaSetup()` and `httptest.NewRecorder`, in a generated `_test.go` placed with `-overlay`; `symbol` for methods whose results decode from JSON, by overlaying the method's body (*observed* on a probe) | Arbitrary signatures; `transport` through `http.DefaultTransport`; building the test binary once per code state |
| `http` | `server` for providers with an OpenAPI document, matched by method and path | Matchers for calls whose operation is only in the body; `process` |

## Cross-cutting requirements

- **Exact, then broader.** A first version does only what is declared. Anything that guesses (rebinding by identity, detecting a framework, matching calls on the wire by their body, finding a setup file without being told) comes later, behind an option, and still reports what it covered.
- **Faithfulness is visible.** A satisfied case says which strategies produced it, in its coverage, and the report shows them. A case run with `symbol` does not claim the client's encoding is right.
- **Determinism.** No harness reads the clock, the network or the machine's state except through the case. Unanswered outbound calls are blocked. Output is sorted.
- **No writes to the tree.** Overlays, temporary directories outside the repository, and bytecode caches turned off.

## Later: optimisation

Correctness first. These wait until behaviors are in use and their cost is measured:

- Running a behavior's cases in one process, or keeping a warm process per code state, instead of starting an interpreter or compiling Go per case. This needs either a batch form of the checker protocol or a long-lived harness.
- Running checkers in parallel (a known limitation today).
- Narrowing evidence keys below whole repositories.

## Out of scope

- Static or symbolic evidence for the same behaviors (document 13, section 11). Harnesses produce runtime evidence only.
- Databases. A handler that needs rows to reach its outbound call gets them from its setup, as its own tests do. Lemma does not fake storage.
- Recording real traffic to replay later.
- A hosted registry of language packs. Lemma's own are release archives; anyone else's are a path or a Git URL.

## Rollout

Each step is useful alone, and each ends with a behavior that holds on a fixed handler and is violated on a broken one, in the conformance suite and on one real operation in the surveyed system.

1. **The interface.** Replay case `replay/1`, `lemma.replay`, the `harness` service field, the `language_packs` workspace field, `lemma language-pack add|list|remove|sync`, choosing a language pack per demand, and unknown cases when none is installed. *Acceptance:* with a test language pack from a local path, a case holds; with it removed, the case is unknown and names the command to install it.
2. **The `python` language pack, first version.** Today's script moves out of the binary into it, with hit counts and blocked calls; `lm new behavior` writes a `replay` behavior and copies the setup template. *Acceptance:* the tutorial runs on `replay` with no per-behavior script, and a behavior over the functions service's webhook setup holds with `http_request` replaced on its class.
3. **`lemma language-pack test`** and the conformance suite, so the next language packs are built against it.
4. **The `go` language pack, first version.** *Acceptance:* a behavior over the Go service's webhook-setup handler, with its call to the functions service replaced, is violated when the handler drops the upstream error, without editing the repository.
5. **The `typescript` language pack, first version (Bun).** *Acceptance:* a behavior over a route that calls a service through a generated client, entered through the setup's `request`.
6. **The `http` language pack.** *Acceptance:* the Go service's transport environment variable points at it, and the step 4 behavior holds with the real client running.

Then the "Later" column of each language pack, one strategy at a time, each with its conformance cases.

## Open questions

- Whether `replace` should also take a client operation that has no link, for calls to third parties that no description models.
- What each language pack's default `replace` strategy should be, once it supports more than one: `symbol` (cheapest, least faithful) or `transport`.
- How language packs are signed, beyond a published digest.
- Whether `lm new behavior` should still write a hand-written checker script for a language with no language pack installed, or only point at `lemma language-pack add`.
