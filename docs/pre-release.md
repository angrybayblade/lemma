# Pre-release checklist

Lemma 0.1 is in release candidates. This page is what each candidate is and is not ready for: what has been measured, what is known to be wrong or missing, and what must be true before 0.1.0. It doubles as the release notes. Each candidate updates it.

Install: see [Onboarding](onboarding.md#1-install). Report problems with the manifest, the command and its output.

## Status by release candidate

| Candidate | Focus | State |
| --- | --- | --- |
| 0.1.0-rc0 | Internal launch: code intelligence in four languages, `lemma verify` across repositories, install script, agent skills | Released |
| 0.1.0-rc1 | Internal release: a general framework with no assumptions about any one API's layout; `lemma check` with inferred links, tracing and change-driven evaluation; `.lm` modules with behaviors and allowances, and the `lm` command; checks that a feature is wired up (see [New in rc1](#new-in-rc1) and [Changes in rc1](#changes-in-rc1)) | Released |
| 0.1.0-rc2 and later | Fixes from internal use; CI green for two weeks; install script run on a clean macOS and Linux machine; items below as they are resolved | Planned |

## What is ready

| Area | Ready for | Evidence |
| --- | --- | --- |
| Code intelligence: TypeScript, TSX, JavaScript, Python, Go, Rust | Daily use by people and agents | Symbols, outlines, references, definitions, implementations and signatures, each through the language's own server. Across 241 random symbols in five repositories, 96% of reported lines were real references ([comparison](comparison.md)); 1-3 ms once cached ([performance](performance.md)). |
| MCP server | Agents in Claude Code and other MCP clients | Agent benchmark rounds r0-r6 ([results](agent-benchmark.md)) |
| `lemma verify` for HTTP contracts | Internal use, errors gating CI after a reporting period | In rc0, 318 historical changes replayed on a production system of ten repositories with no false error; every error matched a real break. |
| `lemma check` across a system's repositories | Internal use, errors gating CI after a reporting period | On a production system of six repositories, for rc1: 54 past contract changes replayed with every finding, severity and trace as reviewed; synthetic breaks found every known caller (47 of 47 for new required fields, 33 of 33 for removed operations) and no other; change-driven evaluation gave the same findings as reading all code on 34 changes and 30 breaks, in about 60% of the time. On a generated system of 100,000 files, a warm change-driven check took 2.2 s and 192 MB. |
| `.lm` modules and `lm` | Describing systems, requirements, behaviors and allowances | The tutorials run as written in CI; a production system's six modules give the same `lemma check` findings as its `lemma.yaml` files. Editor support through `lm lsp`. |
| Grammars in YAML | Teams adding, removing and tuning checks | [Grammars](grammar.md) |

## Known limitations

Each item says what goes wrong, how much it matters, and what to do until it is fixed.

### Verification

| Limitation | Effect | Severity for rc0 | Until fixed |
| --- | --- | --- | --- |
| "Names a field" is evidence, not proof: a common name (`id`, `slug`) used for a consumer's own objects counts as using the field | False **warnings** when a service drops a common field; in rc0's replays, 8 in 156 changes to one service, all in commits that also updated the caller. No false errors seen | Low: warnings do not fail CI | Keep warnings advisory; review them in the pull request |
| Field changes compare names, not types or allowed values | A field whose type or allowed values change is not reported | Medium | Review enum and type changes by hand |
| URLs built at run time from values the code does not show are not matched | Those calls are missed, so their breaks are too | Medium for code that builds URLs dynamically | Check coverage with `--relation calls` ([Onboarding](onboarding.md#4-map-the-system)) |
| Strictness is read from TypeScript zod `.strict()` and specs' `additionalProperties: false` only | In Go and Rust services, sending a removed field is a warning even where the service rejects it | Low | Declare `rejects_unknown_fields: true` on the service |
| Only HTTP with OpenAPI specs | gRPC, GraphQL, queues, events and other RPC (such as serverless function invocations) are not checked | High outside HTTP | Declare those links in `lemma check`'s `lemma.yaml` files |
| Only direct contracts | A change is traced to the callers of the changed service, not on through services that pass its data to their own callers | High in layered systems | Use `lemma check`, which traces what a break affects further up, or run `lemma verify` for each service in the chain |
| Only contract breaks, not behaviour | A change that keeps the contract but changes what it does (rate limits, defaults, ordering) is not caught | High for incidents caused by behaviour changes | Behavioural rules are the next stage, after 0.1 |
| Rust consumers (code calling HTTP from Rust) are tested only by unit tests | Real-world gaps in reqwest call detection may exist | Low | Report missed calls |
| Calls through a generated client are matched by function names derived from `operationId`s (`call_names`) | A generator that names functions some other way (by resource and verb, say) is not matched | Medium for such generators | Report the generator |
| Consumers are matched to the operation they were written for (the base snapshot) | A call that only exists after the change is checked by `consumer_calls_unserved_operation`, not the field rules | Low | None needed |
| The manifest is written by hand | Setup takes an afternoon for a few repositories | Medium | [Onboarding](onboarding.md#4-map-the-system); the `lemma-onboarding` skill helps an agent write it |
| Grammars are YAML; `.ls` files are not implemented | None beyond syntax | Low | Use YAML |

### Checking links (`lemma check`)

`lemma check` lifts two of the limits above: links it cannot infer (queues, function runtimes) can be declared, and a break is traced on through the services that pass it along. It is new in this release candidate.

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| One commit per repository | Services in one repository cannot be checked at different commits, as when one deploys before another | Check each deploy order with its own parent `lemma.yaml` |
| Types are followed within the files read (Go and Rust: the anchored file's directory; TypeScript: relative imports; Python without Pydantic: the anchored file's directory) | Fields under types from elsewhere are not compared; each such type is reported as informational | Anchor schemas where their types are defined |
| Tracing reads the call graph by name | `affects` can list functions that only share a name with a caller, and misses routes whose paths are built at run time | Treat `affects` as where to look |
| Language servers are not used yet | Schemas and the call graph come from Tree-sitter and each language's own resolver | None needed for now |
| Routes are read from Express, Fastify, Hono and Koa routers, FastAPI and Flask decorators, route objects, Go and Rust routers; NestJS and other class-decorator frameworks are not | Such a service's routes are not compared with its spec (`operation_without_route`, `route_without_operation` stay quiet) | Declare its operations from the spec only; report the framework |
| A route Lemma cannot read is given the benefit of the doubt | An operation counts as still served while any route code writes its path, so a removal next to a leftover path string is missed | Review route removals in the pull request |
| A `{ method, path }` object in a service's route code counts as a route, as `createRoute({ method, path, request })` defines one | A table of another service's endpoints kept in route code reads as routes the spec leaves out (informational, or a warning when added) | Keep such tables outside the service's `routes` directories |
| Memory on large systems | One internal report saw a check of six repositories peak near 6.7 GB; measured runs of the same system stay under 1 GB, and a 100,000-file system under 1.5 GB reading all code | Report the command and system size if a check uses more |

### Code intelligence

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| Answers come from language servers, which must be installed and can be slow on first use | First lookups in a large repository take seconds; servers without dependencies installed miss references through imports | Install dependencies; the daemon keeps servers warm |
| pyright and ty do not answer implementation lookups | `lemma implementations` says the server does not support it for Python | Use `lemma refs` |
| gopls finds references only within one module without `go.work` | Cross-module references are missed | Add a `go.work` |
| "No references found" means the server saw none, not that a symbol is unused | Deleting on that basis can break dynamic uses | Cross-check with text search before deleting |

### Release and install

| Limitation | Effect | Until fixed |
| --- | --- | --- |
| Binaries are unstripped | Larger downloads | Fine for now |
| No Windows build | — | Use WSL |
| CI's macOS jobs moved to `macos-15` runners in rc1, after GitHub stopped running `macos-14` ones | Until a run passes on them, macOS builds and tests are unproven in CI | Run `cargo fmt`, `cargo clippy` and `cargo test` locally ([CONTRIBUTING](../CONTRIBUTING.md)) |
| Soufflé is optional and not bundled | `lemma verify` is about six times slower without it, same results | `brew install souffle` |

## New in rc1

- **`lemma check`**: composable `lemma.yaml` files or `.lm` modules describe a system; links between services are declared or inferred from specs and calling code, their schemas compared, and what a break affects traced up through the services that pass it on. Only the code a change's findings can rest on is read. Without `--base`, the work in progress since the branch left the default branch is checked.
- **Is it wired up?** Warnings for an operation a change adds that no route serves or no code calls, and a route a change adds that its spec leaves out; an error for a call of an operation whose route the change removed.
- **Adapters** read a service's operations from its code (built in: Hono with zod-openapi, FastAPI, Express, Flask and Go routers; any other through a script), remember a failure per clone and environment, and fail a `--level strict` check when they cannot run.
- **`.lm` modules and `lm`**: typed modules for services, links, rules, requirements, behaviors (requirements checked by running checkers) and allowances; `lm check`, `lm fmt`, `lm convert`, `lm new behavior` to scaffold a behavior and its checker, and `lm lsp` with VS Code and Zed extensions.
- **Code intelligence**: `lemma graph` shows what uses a symbol and what it uses, across a workspace; warnings where a language server leaves files out of references; workspaces index each repository and answer by name or export address from the workspace's root.
- **MCP**: `check` and `graph` tools, and guidance for agents to check their edits as they build.

## Changes in rc1

rc0 carried assumptions from the first system Lemma was built against. rc1 removes them; manifests written for rc0 need these edits:

| rc0 | rc1 |
| --- | --- |
| Paths in code had to contain `/api/` | A string is a path if it contains a leading segment some service in the manifest serves (`/v1/`, `/api/`, `/collections/`) |
| A "path family" was a path's first two segments | Services own `path_prefixes`, declared or defaulting to the prefix all their operations share |
| `generated_from` clients named functions by camel-casing the `operationId` | `call_names` templates say how: `{operationId}`, `{camel}`, `{pascal}`, `{snake}` |
| Client `tables` (objects with `method`, `path`, `chain`; Python `Operation(...)` tuples) | Removed: one client library's format, not a standard |
| `@/` and `~/` imports were guessed | Imports are resolved through `tsconfig.json` `paths` |

## Before 0.1.0

- [ ] CI green on every pull request for two weeks (rc2).
- [ ] Release builds checked on macOS arm64, macOS x86_64, Linux x86_64 and Linux arm64 (rc2).
- [ ] Install script run on a clean macOS and Linux machine (rc2).
- [ ] `lemma verify` reporting on real pull requests for two weeks; every error checked by hand; no false error.
- [ ] A decision on read-versus-write evidence for response-field warnings, from what internal use shows.
- [ ] Field type and enum value changes compared, or kept out of 0.1 explicitly.
- [ ] Manifest and grammar formats frozen for 0.1 (changes after this need a migration note).
- [ ] Documentation reviewed by someone who did not write it: [Onboarding](onboarding.md), [Grammars](grammar.md), [Verifying changes](verify.md).
- [ ] License chosen and added.
