# Pre-release checklist

Lemma 0.1 is in release candidates. This page is what each candidate is and is not ready for: what has been measured, what is known to be wrong or missing, and what must be true before 0.1.0. It doubles as the release notes. Each candidate updates it.

Install: see [Onboarding](onboarding.md#1-install). Report problems with the manifest, the command and its output.

## Status by release candidate

| Candidate | Focus | State |
| --- | --- | --- |
| 0.1.0-rc0 | Internal launch: code intelligence in four languages, `lemma verify` across repositories, install script, agent skills | Released |
| 0.1.0-rc1 | A general framework: no assumptions about any one API's layout (see [Changes in rc1](#changes-in-rc1)); CI stability; release builds verified on all four platforms; install script tested from a clean machine | Planned |
| 0.1.0-rc2 and later | Fixes from internal use; items below as they are resolved | Planned |

## What is ready

| Area | Ready for | Evidence |
| --- | --- | --- |
| Code intelligence: TypeScript, TSX, JavaScript, Python, Go, Rust | Daily use by people and agents | Symbols, outlines, references, definitions, implementations and signatures, each through the language's own server. Across 241 random symbols in five repositories, 96% of reported lines were real references ([comparison](comparison.md)); 1-3 ms once cached ([performance](performance.md)). |
| MCP server | Agents in Claude Code and other MCP clients | Agent benchmark rounds r0-r6 ([results](agent-benchmark.md)) |
| `lemma verify` for HTTP contracts | Internal use, errors gating CI after a reporting period | In rc0, 318 historical changes replayed on a production system of ten repositories with no false error; every error matched a real break. To be measured again with rc1's general rules. |
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
| Only HTTP with OpenAPI specs | gRPC, GraphQL, queues, events and other RPC (such as serverless function invocations) are not checked | High outside HTTP | Out of scope for 0.1 |
| Only direct contracts | A change is traced to the callers of the changed service, not on through services that pass its data to their own callers | High in layered systems | Run `lemma verify` for each service in the chain |
| Only contract breaks, not behaviour | A change that keeps the contract but changes what it does (rate limits, defaults, ordering) is not caught | High for incidents caused by behaviour changes | Behavioural rules are the next stage, after 0.1 |
| Rust consumers (code calling HTTP from Rust) are tested only by unit tests | Real-world gaps in reqwest call detection may exist | Low | Report missed calls |
| Calls through a generated client are matched by function names derived from `operationId`s (`call_names`) | A generator that names functions some other way (by resource and verb, say) is not matched | Medium for such generators | Report the generator |
| Consumers are matched to the operation they were written for (the base snapshot) | A call that only exists after the change is checked by `consumer_calls_unserved_operation`, not the field rules | Low | None needed |
| The manifest is written by hand | Setup takes an afternoon for a few repositories | Medium | [Onboarding](onboarding.md#4-map-the-system); the `lemma-onboarding` skill helps an agent write it |
| Grammars are YAML; `.ls` files are not implemented | None beyond syntax | Low | Use YAML |

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
| CI is new in rc0 | Checks may be flaky until rc1 | Run `cargo fmt`, `cargo clippy` and `cargo test` locally ([CONTRIBUTING](../CONTRIBUTING.md)) |
| Soufflé is optional and not bundled | `lemma verify` is about six times slower without it, same results | `brew install souffle` |

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

- [ ] CI green on every pull request for two weeks (rc1).
- [ ] Release builds checked on macOS arm64, macOS x86_64, Linux x86_64 and Linux arm64 (rc1).
- [ ] Install script run on a clean macOS and Linux machine (rc1).
- [ ] `lemma verify` reporting on real pull requests for two weeks; every error checked by hand; no false error.
- [ ] A decision on read-versus-write evidence for response-field warnings, from what internal use shows.
- [ ] Field type and enum value changes compared, or kept out of 0.1 explicitly.
- [ ] Manifest and grammar formats frozen for 0.1 (changes after this need a migration note).
- [ ] Documentation reviewed by someone who did not write it: [Onboarding](onboarding.md), [Grammars](grammar.md), [Verifying changes](verify.md).
- [ ] License chosen and added.
