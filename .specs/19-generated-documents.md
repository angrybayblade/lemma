# Generated documents

**Status: implemented 2026-11-12, except [scheduling](#open-scheduling-and-cost), which is parked.**

## Problem

A service described by `http.openapi("openapi.json")` is checked against its committed document. Many services generate that document from code: a Go program that builds the router and prints its document, a Rust binary printing utoipa's document, a FastAPI app. An edit to a handler's request type that adds a required field breaks every caller, but until someone regenerates the document, Lemma sees no change and `lemma check` passes.

Adapters already read operations from code, but only through a separate provider (`http.adapter_command`), and the document an adapter wrote was kept by the repository's whole Git tree, so any edit anywhere ran every adapter again.

## Decisions

| Aspect | Decision |
| --- | --- |
| The command | `command(...)` is a value in the prelude: what to run (`sh -c`), `directory`, `setup`, `env`, `timeout_ms`, optional `entry` and `exclude`, and `inputs`. It replaces `http.adapter_command`. The built-in adapters (`hono.app`, `fastapi.app`, `http.adapter`) are commands too, with Lemma's script; `http.adapter` may name a script in the repository instead, which Lemma runs with its language's interpreter and, when that is Bun, patches as it does its own. All of them have type `Generator` |
| Where it is named | `http.openapi(document?, command?)`. The document stays optional for code-only services and the command stays optional for spec-first ones; one of the two is required. A generator may also be written straight into `operations`, as before |
| One command, many documents | A service names at most one command. Several documents may name the same one; it runs once, only when a check reads the service, and each document takes the operations under its paths, as the overlay already does |
| Inputs | Declared by the user, on the command. The document is kept by the objects those paths name at a commit, plus the working directory's changes under them, so a change elsewhere reuses it. Without `inputs` the whole repository is the key, and `lm check` warns. `lm check` rejects patterns, absolute paths and `..`, which would name no object at any commit and so give every commit one key. When no input exists at a commit, as with a typo, the whole repository keys it there for the same reason. Beyond that, Lemma does not check `inputs` against what the command reads: a too-narrow list keeps a stale document locally, while CI, starting cold, runs the command anyway |
| What is reported | Unchanged: the command's document is laid over the committed one, so consumers are checked against the code with `--change`, and the committed document's drift from the code is reported without it |

```lm
let billing_spec = command(
    "cargo run --quiet --bin openapi > \"$LEMMA_OUTPUT\"",
    directory: "services/billing",
    inputs: ["services/billing", "Cargo.lock"],
)

export service billing {
    operations: http.openapi("services/billing/openapi.json", command: billing_spec)
}
```

Names considered for `command:` were `generated_by` and `depends`. `depends` suggested a dependency on another service, which the vocabulary reserves, and make-style regeneration of the committed file, which Lemma does not do: the command writes to `$LEMMA_OUTPUT` in Lemma's cache, the working tree is never touched, and a failed run falls back to the committed document.

## A bug found on the way

Patching the base commit's document for a change (`adapters/lazy.rs`) took any change outside TypeScript as one that cannot affect the document. That holds for an adapter reading TypeScript and is wrong for any other: an edit to a Go handler, or a Python one under `fastapi.app`, reused the base document and was kept under the change's key. Patching now applies only to adapters run with Bun, and the cache protocol moved to 3 so documents kept that way are dropped.

## Measured

On an internal Go service of about 40 routes whose generator builds its router and prints the document its framework derives from handler types:

| Run | Time |
| --- | --- |
| The generator alone, Go build cache warm | 1.0 s |
| Same, first build after a pull | 4.5 s |
| In `lemma check --change`, head, after editing a handler | 2.7 s |
| In `lemma check --change`, base, built in Lemma's worktree | 5.6 s, then kept |
| A whole `lemma check --change` of its repository with nothing to run | 1.5 s |
| The same after an edit outside `inputs`, with `inputs` | 1.5 s |
| The same after an edit outside `inputs`, without `inputs` | 4.1 s |

The generator's output was byte-identical across runs and equal to the committed document. `crates/lemma-graph/tests/generated_document.rs` checks the same behavior offline with a shell script as the generator: an uncommitted edit, a committed edit with a stale document, and the command running again only when its inputs change.

## Open: scheduling and cost

Parked until a slower generator makes it matter:

- **When it runs.** Today an ask runs the command if its document is not kept. Sentry could start it when a file under `inputs` is saved, so an ask finds it ready.
- **The base document.** It is generated at the base too, so both sides come from the same source; a stale committed document would otherwise show its staleness as the change's. That costs one worktree build per base, kept by the base's `inputs`.
- **A slow command.** An ask blocks up to `timeout_ms`. An alternative is answering from the last good document and marking the operations it covers unknown until it is fresh.
- **CI.** Cold every job unless `GOCACHE` and `.lemma/` are kept between runs; no new mechanism is planned.
- **A syntax fast path for Go**, like the zod one, would patch the document from a changed struct. It would be a second reader of the framework's tags, the problem the command removes, so it is an optimization only.
