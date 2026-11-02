# Pre-release checklist

Lemma 0.1 is in release candidates. This page is what each candidate is and is not ready for: what has been measured, what is known to be wrong or missing, and what must be true before 0.1.0. It doubles as the release notes. Each candidate updates it.

Install: see [Onboarding](onboarding.md#1-install). Report problems with the manifest, the command and its output.

## Status by release candidate

| Candidate | Focus | State |
| --- | --- | --- |
| 0.1.0-rc0 | Internal launch at generic platform: code intelligence in four languages, `lemma verify` across repositories, install script, agent skills | Released |
| 0.1.0-rc1 | CI stability: every check green on Linux and macOS; release builds verified on all four platforms; install script tested from a clean machine | Planned |
| 0.1.0-rc2 and later | Fixes from internal use; items below as they are resolved | Planned |

## What is ready

| Area | Ready for | Evidence |
| --- | --- | --- |
| Code intelligence: TypeScript, TSX, JavaScript, Python, Go, Rust | Daily use by people and agents | Symbols, outlines, references, definitions, implementations and signatures, each through the language's own server. Across 241 random symbols in five repositories, 96% of reported lines were real references ([comparison](comparison.md)); 1-3 ms once cached ([performance](performance.md)). |
| MCP server | Agents in Claude Code and other MCP clients | Agent benchmark rounds r0-r6 ([results](agent-benchmark.md)) |
| `lemma verify` for HTTP contracts | Internal use, errors gating CI after a reporting period | 318 historical changes replayed across Gateway, the dashboard and Worker with no false error; every error matched a real break ([results](verify.md#how-well-it-works)) |
| Grammars in YAML | Teams adding, removing and tuning checks | [Grammars](grammar.md) |

## Known limitations

Each item says what goes wrong, how much it matters, and what to do until it is fixed.

### Verification

| Limitation | Effect | Severity for rc0 | Until fixed |
| --- | --- | --- | --- |
| "Names a field" is evidence, not proof: a common name (`id`, `slug`, `toolkit`) used for a consumer's own objects counts as using the field | False **warnings** when a service drops a common field; 8 in 156 Worker changes, all in commits that also updated the caller. No false errors seen | Low: warnings do not fail CI | Keep warnings advisory; review them in the pull request |
| Field changes compare names, not types or allowed values | A field whose type or enum values change (`"prompt"` to `"condition_check"`) is not reported | Medium | Review enum and type changes by hand |
| URLs built at run time from values the code does not show are not matched | Those calls are missed, so their breaks are too | Medium for code that builds URLs dynamically | Check coverage with `--relation calls` ([Onboarding](onboarding.md#4-map-the-system)) |
| Strictness is read from TypeScript zod `.strict()` and specs' `additionalProperties: false` only | In Go and Rust services, sending a removed field is a warning even where the service rejects it | Low | Declare `rejects_unknown_fields: true` on the service |
| Only HTTP with OpenAPI specs | gRPC, GraphQL, queues and events between services are not checked | High outside HTTP | Out of scope for 0.1 |
| Only contract breaks, not behaviour | A change that keeps the contract but changes what it does (rate limits, defaults, ordering) is not caught | High for incidents like IR-70 | Behavioural rules are the next stage, after 0.1 |
| Rust consumers (code calling HTTP from Rust) are tested only by unit tests | Real-world gaps in reqwest call detection may exist | Low | Report missed calls |
| Calls through a generated client are matched by its function names: table chains, or camel-cased `operationId`s | A generator with another naming scheme is not matched | Medium for other generators | Use a `tables` file, or report the generator |
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

## Before 0.1.0

- [ ] CI green on every pull request for two weeks (rc1).
- [ ] Release builds checked on macOS arm64, macOS x86_64, Linux x86_64 and Linux arm64 (rc1).
- [ ] Install script run on a clean macOS and Linux machine (rc1).
- [ ] `lemma verify` reporting on generic platform pull requests for two weeks; every error checked by hand; no false error.
- [ ] A decision on read-versus-write evidence for response-field warnings, from what internal use shows.
- [ ] Field type and enum value changes compared, or kept out of 0.1 explicitly.
- [ ] Manifest and grammar formats frozen for 0.1 (changes after this need a migration note).
- [ ] Documentation reviewed by someone who did not write it: [Onboarding](onboarding.md), [Grammars](grammar.md), [Verifying changes](verify.md).
- [ ] License chosen and added.
