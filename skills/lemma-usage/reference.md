# Lemma usage reference

## Commands

| Command | Does |
| --- | --- |
| `lemma index <directory>` | Indexes the code `<directory>/mod.lm` says its services are made of (`index`: `include`, `exclude`, `languages`); writes `<directory>/.lemma/index.db`. The daemon then keeps it current. Without a `mod.lm`, or when no service names its code, indexes every supported file and warns. |
| `lemma symbols <query>` | Searches indexed symbols by name or qualified name. |
| `lemma outline <file>` | Lists a file's symbols in source order. |
| `lemma refs <symbol>` | References through the symbol's language server. |
| `lemma definition <file>:<line>:<column>` | Where the symbol used at a position is defined (1-based line and column). |
| `lemma implementations <symbol>` | What implements an interface or abstract method, or overrides a method. |
| `lemma signature <symbol>` | The declaration and the server's hover text. |
| `lemma providers` | Configured language servers and whether each is installed. |
| `lemma mcp` | Serves these commands as MCP tools over stdin and stdout. |
| `lemma check [mod.lm]` | Checks a change across the repositories a workspace composes. |
| `lemma verify [mod.lm]` | Checks that a description still matches the code at one snapshot. |
| `lemma daemon status` / `stop` | Inspects or stops the background daemon. |

Every command takes `--json`. Positions in JSON are zero-based.

## Exit codes

| Code | Means |
| --- | --- |
| 0 | Answered; for `refs`, including "no references found" |
| 1 | Error: no index, unknown symbol, invalid configuration |
| 2 | `refs`: the name matched several symbols |
| 3 | `refs`: no answer (stale file, no server, server failed or unsupported) |
| 4 | `check` and `verify`: a finding is an error |
| 5 | `check`: it finished, but a `git(...)` repository was not cloned |

## `lemma check` options

| Option | Does |
| --- | --- |
| `--change <name or directory>` | The changed repository, or `self` |
| `--base <commit>`, `--head <commit>` | The commits before and with the change |
| `--level audit` / `check` / `strict` | What fails; overrides the workspace's `level` |
| `--explain` | Shows the facts and rules behind each inferred finding |
| `--evaluation demand` / `full` / `both` | How much code is read; `both` fails if the two differ |
| `--all` | Lists informational findings too |
| `--select <name>` | Runs this check, behavior or requirement |
| `--no-run`, `--run-base` | Runs no checkers; also runs them at the base |
| `--no-clone` | Never clones; exits 5 if a repository is missing |
| `--engine souffle` / `reference` | Picks the rule engine; Soufflé if installed by default |
| `--json` | Findings and values as JSON |

## `lemma verify` options

| Option | Does |
| --- | --- |
| `--all` | Lists every informational finding |
| `--links` | Also lists every link, declared and inferred |
| `--relation <name>` | Prints every row of a derived relation (repeatable) |
| `--json` | Findings, links and relations as JSON |

Findings name a location as `#repo:service`: `include_matches_nothing`, `document_missing`, `document_unreadable`, `schema_unresolved`, `schema_open`, `runtime_missing`, `external_unpinned`, `service_empty`, `operation_not_documented`, `anchor_unresolved`, `anchor_uncalled`, `previously_still_present`, `link_redundant`.

## Built-in requirements

| Requirement | Severity | Means |
| --- | --- | --- |
| `consumer_calls_removed_operation` | error | A consumer calls an operation the change removed |
| `consumer_calls_unserved_operation` | error | A changed consumer adds a call to a path no service serves |
| `consumer_misses_new_required_parameter` | error | An operation a consumer calls now requires a parameter |
| `consumer_misses_new_required_field` | error | An operation now requires a request field the consumer's code never names |
| `consumer_sends_removed_parameter` | error | A consumer sends a removed parameter to an operation that rejects unknown ones |
| `consumer_sends_ignored_parameter` | warning | As above, where the operation ignores unknown parameters |
| `consumer_sends_rejected_field` | error | A consumer sends a removed request field to an operation that rejects unknown fields |
| `consumer_uses_removed_request_field` | warning | As above, where the operation ignores unknown fields |
| `consumer_uses_removed_response_field` | warning | A consumer reads a response field the change removed |
| `client_declares_removed_parameter` | error | A generated client still declares a removed parameter |
| `client_code_sends_removed_parameter` | error | A generated client's code still sends a removed parameter; its copy of the spec was updated without generating the code again |
| `client_calls_removed_operation` | error | A generated client method calls a removed operation |
| `consumer_calls_changed_operation` | info | A consumer calls an operation whose request changed, but never names the changed field |
| `consumer_calls_unrouted_operation` | error | A consumer calls an operation whose route the change removed, though the spec still declares it. Only when no route code still writes the path, so a route moved where Lemma cannot read it is not reported |
| `operation_without_route` | warning | The change adds a spec operation no route serves |
| `route_without_operation` | warning | The change adds a route the spec does not declare |
| `operation_without_consumer` | warning | The change adds an operation nothing in the workspace calls (exempt if in `public_operation`) |
| `route_not_in_spec` | info | A service serves a route its spec does not declare (introduced when the change adds it, which `route_without_operation` also warns about) |

## Known limits that affect answers

- Field changes compare names, not types or allowed values.
- URLs built at run time from values the code does not show are not matched.
- Warnings rest on field names appearing near the call; common names can mislead.
- Only HTTP contracts with OpenAPI specs are checked, and only contract breaks, not behaviour changes.
