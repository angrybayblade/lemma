# Verifying descriptions

`lemma verify` answers one question: does this description still match the code? It reads the description and one snapshot of the code, and reports what the description says that the code does not bear out.

| Command | Question | Reads |
| --- | --- | --- |
| `lm check` | Is this module valid? | `.lm` files only |
| `lemma verify` | Does this description still match the code? | The code at one snapshot |
| `lemma check` | Does this change break anything? | The code before and with a change |

`lemma verify` never compares two commits and never reports a contract disagreement. Whether a caller's schema agrees with its provider's is [`lemma check`](check.md)'s question, even at a single snapshot. A clean `lemma check` on an incomplete description looks exactly like a clean `lemma check` on a complete one, which is why this command exists.

## Example

```text
$ lemma verify
error   #billing:api  exports `gone.json`, which does not exist
        [document_missing]
error   #billing:api  `nope` matches no tracked file
        [include_matches_nothing]
error   #web_app:services.web.send_charge -> #billing:services.api.receive_charge  the provider operation #billing:services.api.receive_charge's code anchor `src/app.ts:nothing` does not resolve
        [anchor_unresolved]
3 errors, 0 warnings, 0 informational
```

Each finding has its severity, what is described wrongly as `#repo:service` or the link it is about (without a file and line), what was found, and its name in brackets.

## Command

```sh
lemma verify [mod.lm] [--all] [--links] [--relation <name>] [--json]
```

| Option | Meaning |
| --- | --- |
| `--all` | List every informational finding, not just the first ten. |
| `--links` | Also list every link, declared and inferred. |
| `--relation <name>` | Also print every row of a relation `lemma.http` or a module derives, such as `calls`. Repeatable. Relations that compare the code before and after a change are empty, since there is no change. |
| `--json` | Print findings, and the links and relations asked for, as JSON. |

The entry module sets the scope. A workspace verifies every repository it composes. A repository's root module verifies that repository alone. There is no `--change`, `--base` or `--head`: it reads the working directory, and CI verifies what it checked out.

A repository that is not cloned follows [the `clone` setting](commands.md#repositories-that-are-not-cloned), and one it stops without is listed as `not looked at`.

The whole workspace is read with full evaluation, so a run takes about as long as a `lemma check` that reads every repository. It runs what the description says to run (adapter commands and schema resolvers), as `lemma check` does, so it needs a repository's dependencies installed. Run it when a description changes, on a schedule, and when `lemma check` finds fewer links than expected, not after every edit. A description that does not load is a failure, and prints the same errors as `lm check`.

Exits with 0 when no finding is an error, 4 when one is, and 1 on failure.

The MCP server has a `verify` tool with the arguments `module`, `all` and `links`, so agents reach it the same way ([Commands](commands.md#lemma-mcp)).

## Findings

### Wrong configuration

| Finding | Severity | Meaning |
| --- | --- | --- |
| `include_matches_nothing` | error | An `include` or `exclude` pattern matches no tracked file. |
| `document_missing` | error | A document a service exports does not exist. |
| `document_unreadable` | error | A document does not parse, or describes no operations. |
| `schema_unresolved` | error | A type or document location does not resolve, or a schema command fails. |
| `schema_open` | informational | A schema uses types Lemma could not follow, so the fields under them go uncompared. |
| `runtime_missing` | error | An interpreter or toolchain the service's `runtime` names does not exist. |
| `external_unpinned` | warning | A repository named by `git(...)` has no pinned commit, so what it is read at can change. |
| `service_empty` | error | A service reads no files and offers nothing. |

### Outdated links

| Finding | Severity | Meaning |
| --- | --- | --- |
| `operation_not_documented` | error | A link names an operation its service's documents do not describe, or that is no longer declared. |
| `anchor_unresolved` | error | An operation's `implementation` does not resolve. |
| `anchor_uncalled` | warning | Nothing outside its type calls an anchored method, while a sibling method of the same type in the same file is called. The anchor probably names the wrong method (a generated client's `Invoke` where the code calls `InvokeWithRuntimeEnv`), and every trace from it is empty. |
| `previously_still_present` | warning | An operation says what it used to be named (`previously`), and that name is still declared, so the claim of a rename is false. |
| `link_redundant` | informational | A declared link is one inference finds too, and says nothing more. |

`lm check` reports a link naming an operation its service does not have when the service discovers no operations. A service with `operations:` might discover the name, so `lm check` cannot rule it out; this command can.

To find `anchor_uncalled`, Lemma asks language servers who calls each link's caller anchor. A repository whose language has no server installed is reported as not checked. When the Lemma daemon is running, its language servers answer, and stay warm for the next command; without one (or with `--no-daemon`), `lemma verify` starts its own, which takes seconds per repository.

## What it does not find

A clean result does not mean nothing is missing. `lemma verify` compares what a description says with the code; it does not yet look for what a description leaves out, such as an OpenAPI document no service exports, route code no service includes, or a call over a queue no operation describes. The [`lemma-audit` skill](../skills/lemma-audit/SKILL.md) works through those by hand.

There is no way yet to accept a finding with an `allow`.
