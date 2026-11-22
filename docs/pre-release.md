# Pre-release checklist

Lemma 0.1 is in release candidates. This page lists what each candidate is and is not ready for: what was measured, what is known to be wrong or missing, and what must be true before 0.1.0. It doubles as the release notes. Each candidate updates it.

Install: see [Onboarding](onboarding.md#1-install). Report problems with the description, the command and its output.

## Status by release candidate

| Candidate | Focus | State |
| --- | --- | --- |
| 0.1.0-rc0 | Internal launch: code intelligence in four languages, contract checks across repositories, install script, agent skills | Released |
| 0.1.0-rc1 | Internal release: a general framework with no assumptions about any one API's layout; `lemma check` with inferred links, tracing and change-driven evaluation; `.lm` modules with behaviors and allowances, and the `lm` command; checks that a feature is wired up; one store file in place of SQLite; one daemon per workspace, with sentry; evidence kept by the files a checker's run read; source contracts, experimental (see [New in rc1](#new-in-rc1)) | Released |
| 0.1.0-rc2 and later | Fixes from the first outside trial of rc1 (see [Since rc1](#since-rc1)); layouts met in public repositories read; a removed response field read off the body an error; behavior simulation scaffolded for Python, TypeScript and Go; a recorder for macOS, so evidence is kept by the files a run read there too; fixes from internal use; install script run on a clean macOS and Linux machine; items below as they are resolved | Planned |

## Feature surfaces

What Lemma does falls into four surfaces. Each is at a different level in this candidate.

| Surface | Commands | Level in rc1 | What that means |
| --- | --- | --- | --- |
| Code discovery | `lemma symbols`, `outline`, `refs`, `definition`, `implementations`, `signature`, the MCP tools | Ready | Daily use. The first lookup in a repository waits for its language server to load the project |
| Code impact | `lemma check --change`, `lemma graph` | Ready for HTTP contracts | Every finding on the replayed changes below, and every known caller of the synthetic breaks. Links other than HTTP with OpenAPI are declared by hand |
| Behavior simulation | behaviors and checkers in `.lm`, `lm new behavior` | [Beta](#behaviors-beta) | Runs real code in any language a command can run. The scaffold writes a complete checker only for Python |
| Source contracts | a checker with `engine: "proof"` | Experimental | Checks a small, explicit subset of TypeScript, Python and Go against a contract, with an SMT solver you install. Code it cannot read gives `unknown`, never a pass ([source contracts](source-contracts.md)) |
| Verification loop | `lemma check` on work in progress, sentry, the MCP `check` tool, the `lemma-verify` skill's stop hook | Experimental | Sentry's answers equalled a fresh check in every replayed case. How much the loop helps an agent is not yet measured on public tasks |

## Ready

| Area | Ready for | Evidence |
| --- | --- | --- |
| Code intelligence: TypeScript, TSX, Python, Go, Rust | Daily use by people and agents | Symbols, outlines, references, definitions, implementations and signatures, each through the language's own server. Across 241 random symbols in five repositories, 96% of reported lines were real references ([comparison](comparison.md)); 1-3 ms once cached ([performance](performance.md)). |
| MCP server | Agents in Claude Code and other MCP clients | Agent benchmark rounds r0-r6 ([results](agent-benchmark.md)) |
| `lemma.http` requirements for HTTP contracts | Internal use, errors gating CI after a reporting period | In rc0, 318 historical changes replayed on a production system of ten repositories with no false error; every error matched a real break. |
| `lemma check` across a system's repositories | Internal use, errors gating CI after a reporting period | On a production system of six repositories, for rc1: 54 past contract changes replayed with every finding, severity and trace as reviewed; synthetic breaks found every known caller (47 of 47 for new required fields, 33 of 33 for removed operations) and no other; change-driven evaluation gave the same findings as reading all code on 54 changes and 30 breaks, in about 60% of the time; sentry gave the same findings as a fresh check in 214 cases, each asked once the files had settled and once while they were still changing. On a generated system of 100,000 files, a warm change-driven check took 2.7 s and 365 MB. |
| `.lm` modules and `lm` | Describing systems, requirements and allowances; behaviors are in [beta](#behaviors-beta) | The tutorials run as written in CI; a production system of six repositories is described and checked in `.lm`. Editor support through `lm lsp`. |
| Requirements and rules in `.lm` | Teams adding, removing and tuning checks | [Requirements and rules](grammar.md) |

## Known limitations

Each item says what goes wrong, how much it matters, and what to do until it is fixed.

### Contract checks

| Limitation | Effect | Severity for rc0 | Until fixed |
| --- | --- | --- | --- |
| "Names a field" is evidence, not proof: a common name (`id`, `slug`) used for a consumer's own objects counts as using the field | False **warnings** when a service drops a common field; in rc0's replays, 8 in 156 changes to one service, all in commits that also updated the caller. No false errors seen | Low: warnings do not fail CI | Keep warnings advisory; review them in the pull request |
| Field changes compare names, not types or allowed values | A field whose type or allowed values change is not reported | Medium | Review enum and type changes by hand |
| URLs built at run time from values the code does not show are not matched | Those calls are missed, so their breaks are too | Medium for code that builds URLs dynamically | Check coverage with `lemma verify --relation calls` ([Onboarding](onboarding.md#4-describe-the-system)) |
| Strictness is read from TypeScript zod `.strict()` and specs' `additionalProperties: false` only | In Go and Rust services, sending a removed field is a warning even where the service rejects it | Low | Declare `rejects_unknown_fields: true` on the service |
| Only HTTP with OpenAPI specs | gRPC, GraphQL, queues, events and other RPC (such as serverless function invocations) are not checked | High outside HTTP | Declare those links in a `.lm` description |
| Only contract breaks, not behaviour | A change that keeps the contract but changes what it does (rate limits, defaults, ordering) is not caught | High for incidents caused by behaviour changes | Test what matters with a [behavior](tutorials/4-behaviors.md) (beta) |
| Rust consumers (code calling HTTP from Rust) are tested only by unit tests | Real-world gaps in reqwest call detection may exist | Low | Report missed calls |
| Calls through a generated client are matched by function names derived from `operationId`s (`call_names`) | A generator that names functions some other way (by resource and verb, say) is not matched | Medium for such generators | Report the generator. Where the client's own code writes each operation's path, include that code in the client's `index`: its methods are read from it |
| Consumers are matched to the operation they were written for (the base snapshot) | A call that only exists after the change is checked by `consumer_calls_unserved_operation`, not the field rules | Low | None needed |
| A description is written by hand | Setup takes an afternoon for a few repositories | Medium | [Onboarding](onboarding.md#4-describe-the-system); the `lemma-onboarding` skill helps an agent write it |
| `lemma verify` compares what a description says with the code; it does not look for what a description leaves out | An OpenAPI document no service exports, or route code no service includes, stays unnoticed | Medium | The `lemma-audit` skill looks by hand |

### Checking links (`lemma check`)

Links Lemma cannot infer (queues, function runtimes) can be declared, and Lemma traces a break through the services that pass it along. `lemma check` is new in this release candidate.

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| One commit per repository | Services in one repository cannot be checked at different commits, as when one deploys before another | Check each deploy order with its own workspace module |
| Types are followed within the files read (Go and Rust: the anchored file's directory; TypeScript: relative imports; Python without Pydantic: the anchored file's directory) | Fields under types from elsewhere are not compared; each such type is reported as informational | Anchor schemas where their types are defined |
| Tracing reads the call graph by name | `affects` can list functions that only share a name with a caller, and misses routes whose paths are built at run time | Treat `affects` as where to look |
| Language servers are not used yet | Schemas and the call graph come from Tree-sitter and each language's own resolver | None needed for now |
| Routes are read from Express, Fastify, Hono and Koa routers, FastAPI and Flask decorators, route objects, Go and Rust routers; NestJS and other class-decorator frameworks are not | Such a service's routes are not compared with its spec (`operation_without_route`, `route_without_operation` stay quiet) | Declare its operations from the spec only; report the framework |
| A route Lemma cannot read is given the benefit of the doubt | An operation counts as still served while any route code writes its path, so a removal next to a leftover path string is missed | Review route removals in the pull request |
| A `{ method, path }` object in a service's route code counts as a route, as `createRoute({ method, path, request })` defines one | A table of another service's endpoints kept in route code reads as routes the spec leaves out (informational, or a warning when added) | Keep such tables outside the service's `routes` directories |
| Memory on large systems | One internal report saw a check of six repositories peak near 6.7 GB; measured runs of the same system stay under 1 GB, and a 100,000-file system under 1.5 GB reading all code | Report the command and system size if a check uses more |

### Behaviors (beta)

Behaviors run your code through checkers. They work end to end, and the tutorial runs as written in CI, but their `.lm` syntax, the checker protocol and how evidence is kept may change between releases.

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| `lm new behavior` writes a complete checker script only for Python | For TypeScript it leaves replacing the outbound call to you; for Go it writes a placeholder; for other languages it writes none | Write the checker by hand: any command that reads `LEMMA_INPUT` and writes `LEMMA_OUTPUT` works |
| Evidence is kept by the files a run read only where Lemma can watch the run: Linux with `strace` installed, for a checker run in a working directory | On macOS, at a commit, and without `strace`, any change to a repository a checker reads runs it again | Install `strace` on Linux; keep checkers fast elsewhere |
| A run that lists a directory rests on that listing | A new file in a directory the run listed runs it again, even when nothing uses the file | None |
| A checker's cases run one after another unless it says `concurrent: true` | A check with many cases takes as long as all of them together | Mark a checker whose runs write only where `LEMMA_OUTPUT` says `concurrent: true` ([language](language.md)) |
| Behaviors read stated graphs only | Facts Lemma reads from code (`calls` and the rest of `lemma.http`) are not available to them | Declare the links a behavior needs |
| Evidence stays on the machine that ran the checker | CI and each developer run checkers themselves | Copy the cache's `evidence/` directory |

### Code intelligence

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| Answers come from language servers, which must be installed and can be slow on first use | First lookups in a large repository take seconds; servers without dependencies installed miss references through imports | Install dependencies; the daemon keeps servers warm |
| Some large TypeScript files do not parse whole (four in zod's sources) | `lemma index` names each such file; symbols after the point of failure may be missing from `symbols` and `outline` | Look the symbol up by a file that uses it, or use text search |
| `lemma index` reads no JavaScript files (`.js`, `.jsx`, `.mjs`, `.cjs`) | What they define cannot be looked up by `lemma symbols`, `refs` or `graph`; `lemma check` still reads them for calls to operations | Ask about the TypeScript that uses them, or use text search |
| pyright and ty do not answer implementation lookups | `lemma implementations` says the server does not support it for Python | Use `lemma refs` |
| gopls finds references only within one module without `go.work` | Cross-module references are missed | Add a `go.work` |
| "No references found" means the server saw none, not that a symbol is unused | Deleting on that basis can break dynamic uses | Cross-check with text search before deleting |

### Release and install

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| Binaries are unstripped | Larger downloads | Fine for now |
| No Windows build | Does not run natively on Windows | Use WSL |
| Soufflé is optional and not bundled | `lemma check` is about six times slower without it, same results | `brew install souffle` |

## Since rc1

On main, not yet in a release. Most of it came from the first trial of rc1 outside the project; the rest from layouts met in public repositories.

- **The stop gate checks where it used to pass.** It read repositories from the directories beside `mod.lm` that hold a `.git` directory, so a Git worktree, or a repository beside the workspace, was never checked and the agent finished with errors in place. It reads them from the workspace's module now, finds the workspace from inside a repository too (or from `LEMMA_WORKSPACE`), checks commits a task made as well as uncommitted edits, and when `lemma check` fails or does not finish it sends the agent back with the reason.
- **The stop gate sends an agent back for warnings too.** A response field a change removed that the caller still reads is a warning, and in the trial it was a real failure at run time. `LEMMA_GATE_LEVEL=error` restores errors only ([`lemma-verify`](../skills/lemma-verify/SKILL.md)).
- **A removed response field the code reads off the body is an error.** `body.status` after `const body = await res.json()`, `res.data.items`, or `resp.json()["items"]` fails at run time once the field is gone, so `consumer_reads_removed_response_field` reports it as an error. A name the code only mentions near the call, or reads off the response object itself (`res.status`, `res.headers`), stays the warning `consumer_uses_removed_response_field` ([check](check.md#lemmahttp-requirements)).
- **Routes are placed as public services write them.** A route written under the path an OpenAPI document's `servers` URL (or Swagger's `basePath`) puts its operations, as `/api/articles` for a document served at `https://example.test/api`, serves the documented `/articles`. Gin and echo routes registered by a function in another package are placed under the group passed to it. A YAML document is read where a route's operation is placed. Renaming a path parameter, or sending a required query parameter in the path string, is no longer reported, and a route removed beside another method's route on the same path is.
- **A graph says when it is not whole.** `lemma graph` asked the language server about the first 60 names in a body; a longer body lost the calls after them with no sign. The limit is 200 now, which no sampled symbol reached, and past it a note gives how many names were left unasked.
- **A type Lemma cannot follow is no longer compared.** A request type from another package, or written `z.infer<typeof X>`, was read as "anything" and failed the check with errors that were not known to be true. It is reported by name and its place left out, as these notes had said.
- **The stop gate checks committed work, from wherever the agent runs.** A repository with commits not on its default branch is checked as well as one with uncommitted edits, so an orchestrator that commits each task is held to the check. An agent started inside a repository is checked from the workspace above it, or the one `LEMMA_WORKSPACE` names.
- **Nothing passes quietly.** A plain `lemma check` names the repositories with uncommitted edits it could not judge and the `--change` that would. A behavior says why its evidence was not kept (`not_kept` in JSON), as when a repository it reads is not a Git repository where no run can be watched. A failed command's message says how it ended and keeps its first and last lines. `lm check` reports rules that cannot run, which used to surface only when links were inferred.
- **`lemma verify` runs no behavior's checker.** It reported none of them, and running them only cost time.
- **Contracts from code are shown at the entry.** A service needs no OpenAPI document: its functions and types, or its routes, are the contract ([Onboarding](onboarding.md#4-describe-the-system)). The entry pages had shown only documents.

## New in rc1

- **`lemma check`**: `.lm` modules describe a system; links between services are declared or inferred from specs and calling code, their schemas compared, and what a break affects traced up through the services that pass it on. Lemma reads only the code a change's findings can rest on. With `--change <repository>` and no `--base`, it checks the work in progress since the branch left the default branch.
- **Is it wired up?** Warnings for an operation a change adds that no route serves or no code calls, and a route a change adds that its spec leaves out; an error for a call of an operation whose route the change removed.
- **Adapters** read a service's operations from its code (built in: Hono with zod-openapi, FastAPI, Express, Flask and Go routers; any other through a script), remember a failure per clone and environment, and fail a `--level strict` check when they cannot run.
- **`.lm` modules and `lm`**: typed modules for services, links, rules, requirements, behaviors (requirements checked by running checkers) and allowances; `lm check`, `lm fmt`, `lm new behavior` to scaffold a behavior and its checker, and `lm lsp` with VS Code and Zed extensions.
- **Code intelligence**: `lemma graph` shows what uses a symbol and what it uses, across a workspace; warnings where a language server leaves files out of references; workspaces index each repository and answer by name or export address from the workspace's root.
- **MCP**: `check` and `graph` tools, and guidance for agents to check their edits as they build.
- **Python references across a `src` layout.** In a project that keeps its package under `src/`, references from outside that directory, its tests among them, were missing without a warning. They are found now.
- **One store file.** `.lemma/index.ls` replaces the SQLite index and `lemma check`'s separate cache: files, symbols, cached answers and what checks keep between runs, in one append-only file. rc0's index (`.lemma/index.db`) is not read: after upgrading, a command says to run `lemma index`, which builds the new file; the old `index.db` files can be deleted. Stop rc0's daemon first with `lemma daemon stop`: the two builds' daemons do not see each other.
- **Symbol ids** are 128-bit hashes of a symbol's identity, shown as 16 hexadecimal digits (`#8bce55e22bc817fe`), and stay the same while a symbol keeps its path, name, kind and position among same-named symbols.
- **One parse per file in a check.** Each file is parsed once per check, for its code facts and for tracing together, rather than three times.
- **Generated documents.** `http.openapi("openapi.json", command: ...)` names the command that generates a document, so a handler edit is checked against consumers before the document is regenerated; a command's `inputs` decide when it runs again (see [Read operations from code](adapters.md)). `http.adapter_command(...)` is removed: write `command(...)`.
- **Adapters outside TypeScript run on every change.** A change to a Python or Go handler under an adapter, or to code a command reads, used to reuse the base commit's document. Documents kept that way are dropped the first time rc1 runs.
- **A repository's own adapter script.** `http.adapter("tools/lemma-adapter.ts", ...)` names a script in the repository, run with the interpreter its extension calls for. A `.ts` script is patched on a TypeScript edit as a built-in adapter is, where the same script under `command(...)` runs every time.
- **Evidence by what a run read.** On Linux with `strace`, Lemma watches a checker's run and keeps its evidence until a file that run read, listed or looked for changes, so an edit elsewhere in the repository runs nothing again. Over 72 one-file changes in six public repositories, no reused evidence was contradicted by a fresh run, and the share of cases run again fell from all of them to between 11% and 100%, depending on how much each test imports ([spec 13](../.specs/13-lm-grammar.md), section 10).
- **Source contracts (experimental).** A checker with `engine: "proof"` reads a TypeScript, Python or Go function and checks it against a contract with an SMT solver ([source contracts](source-contracts.md)). It needs Z3 installed and reads a subset of each language: what it cannot read, and a call the contract gives no model for, make the case `unknown`.
- **One daemon per workspace, with sentry.** Commands run in a background daemon that `lemma` starts on first use. Its sentry watches the workspace's repositories and keeps a check's inputs prepared, so a check after an edit reads only what changed. Sentry is experimental ([`lemma daemon`](commands.md#lemma-daemon)).
- **Checkers and warm runtimes.** `concurrent: true` on a checker lets its cases run at once. While the daemon runs, a repository's Python resolvers and type reads run in a process that has already imported its dependencies ([`lemma daemon`](commands.md#lemma-daemon)).

## Checklist for rc1

- [x] CI green on Linux and macOS for the tagged commit.
- [x] The replays above pass on the tagged build: findings, recall, change-driven against full, and sentry against a fresh check.
- [x] Code discovery from a cold start on Linux, on a fresh clone of one public repository per language (Python, TypeScript, Go, Rust): index, `symbols`, `outline`, `refs`, `signature`, `implementations`, `graph`. A first `refs` took 0.5 to 2 s, and 16 s in the Rust workspace while its server loaded; every other answer came in under 0.3 s.
- [x] Code impact: the replays above; `lemma graph` on each of those repositories.
- [x] Behavior simulation: the tutorial's behavior written for a Python, a TypeScript and a Go service; each is violated when the flag is dropped, satisfied when it is passed on, and reused on the next check.
- [x] Verification loop: an uncommitted change that breaks a caller in another repository is an error from `lemma check --change` through sentry in 0.1 s, `--since` lists it as new, and it is gone once the caller is fixed.
- [x] Python references in a project that keeps its package under `src/`. The fresh clone showed pyright reporting none from the tests directory: Lemma's settings had switched off its search of `src`. Fixed in this candidate; on flask, references to `url_for` went from 2 to 57.
- [x] Source contracts: the example's handlers in Python, TypeScript and Go hold against one contract, and a guard switched off in each is reported, with Z3 4.15.4.
- [x] Upgrading from rc0 on Linux: the old index is left alone and `lemma index` builds the new one; this install script installs rc0's archives and skills with their checksum verified.
- [x] Behaviors where `strace` is not installed: they run, and evidence falls back to whole repositories.
- [ ] All of the above from a cold start on macOS.
- [ ] Install script run on a clean macOS and Linux machine, after the tag.

## Before 0.1.0

- [ ] CI green on every pull request for two weeks.
- [ ] Release builds checked on macOS arm64, macOS x86_64, Linux x86_64 and Linux arm64 (rc2).
- [ ] Install script run on a clean macOS and Linux machine (rc2).
- [ ] `lemma check` reporting on real pull requests for two weeks; every error checked by hand; no false error.
- [ ] Read-versus-write evidence for response fields, from what internal use shows: a field read off a call's body is an error (rc2); whether any other read should be.
- [ ] Field type and enum value changes compared, or kept out of 0.1 explicitly.
- [ ] The `.lm` language frozen for 0.1.
- [ ] Documentation reviewed by someone who did not write it: [Onboarding](onboarding.md), [Requirements and rules](grammar.md), [Verifying descriptions](verify.md).
- [x] License chosen and added: Apache-2.0.
