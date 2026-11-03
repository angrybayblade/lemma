# Lemma usage reference

## Commands

| Command | Does |
| --- | --- |
| `lemma index <directory>` | Indexes the code `<directory>/lemma.yaml` says its services are made of (`index.include`, `exclude`, `languages`); writes `<directory>/.lemma/index.db`. The daemon then keeps it current. Fails without a `lemma.yaml`. |
| `lemma symbols <query>` | Searches indexed symbols by name or qualified name. |
| `lemma outline <file>` | Lists a file's symbols in source order. |
| `lemma refs <symbol>` | References through the symbol's language server. |
| `lemma definition <file>:<line>:<column>` | Where the symbol used at a position is defined (1-based line and column). |
| `lemma implementations <symbol>` | What implements an interface or abstract method, or overrides a method. |
| `lemma signature <symbol>` | The declaration and the server's hover text. |
| `lemma providers` | Configured language servers and whether each is installed. |
| `lemma mcp` | Serves these commands as MCP tools over stdin and stdout. |
| `lemma verify <manifest>` | Checks a change across the repositories in a manifest. |
| `lemma daemon status` / `stop` | Inspects or stops the background daemon. |

Every command takes `--json`. Positions in JSON are zero-based.

## Exit codes

| Code | Means |
| --- | --- |
| 0 | Answered; for `refs`, including "no references found" |
| 1 | Error: no index, unknown symbol, invalid configuration |
| 2 | `refs`: the name matched several symbols |
| 3 | `refs`: no answer (stale file, no server, server failed or unsupported) |
| 4 | `verify`: a finding is an error |

## `lemma verify` options

| Option | Does |
| --- | --- |
| `--explain` | Shows the facts and rules behind each finding |
| `--all` | Lists informational findings too |
| `--base <commit>`, `--head <commit>` | Checks these commits instead of the manifest's |
| `--relation <name>` | Prints every row of a derived relation (repeatable) |
| `--engine souffle` / `reference` | Picks the rule engine; Soufflé if installed by default |
| `--json` | Findings, values and relations as JSON |

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
| `client_calls_removed_operation` | error | A generated client method calls a removed operation |
| `consumer_calls_changed_operation` | info | A consumer calls an operation whose request changed, but never names the changed field |
| `route_not_in_spec` | info | A service serves a route its spec does not declare |

## Known limits that affect answers

- Field changes compare names, not types or allowed values.
- URLs built at run time from values the code does not show are not matched.
- Warnings rest on field names appearing near the call; common names can mislead.
- Only HTTP contracts with OpenAPI specs are checked, and only contract breaks, not behaviour changes.
