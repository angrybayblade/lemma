# Verify descriptions

`lemma verify` answers one question: does this description still match the code? It reads the description and one snapshot of the code, and reports what the description says that the code does not bear out.

| Command | Question | Reads |
| --- | --- | --- |
| `lm check` | Is this module valid? | `.lm` files only |
| `lemma verify` | Does this description still match the code? | The code at one snapshot |
| `lemma check` | Does this change break anything? | The code before and with a change |

`lemma verify` never compares two commits and never reports a contract disagreement. [`lemma check`](check.md) compares a caller's schema with its provider's, even at a single snapshot. A clean `lemma check` on an incomplete description looks exactly like a clean `lemma check` on a complete one, which is why this command exists.

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
| `--all` | List every informational finding, every candidate and every request that was not read, not just the first ten of each. |
| `--links` | Also list every link, declared and inferred. |
| `--relation <name>` | Also print every row of a relation `lemma.http` or a module derives, such as `calls`. Repeatable. Relations that compare the code before and after a change are empty, since there is no change. |
| `--json` | Print findings, candidates, every link, the relations asked for, and what was looked at (`looked_at`), as JSON. |

The entry module sets the scope. A workspace verifies every repository it composes. A repository's root module verifies that repository alone. There is no `--change`, `--base` or `--head`: it reads the working directory, and CI verifies what it checked out.

A repository that is not cloned follows [the `clone` setting](commands.md#repositories-that-are-not-cloned), and one it stops without is listed as `not looked at`.

`lemma verify` reads the whole workspace with full evaluation, so a run takes about as long as a `lemma check` that reads every repository. It runs what the description says to run to read schemas and operations (adapter commands and schema resolvers), as `lemma check` does, so it needs a repository's dependencies installed. It runs no behavior's checker and reports nothing about behaviors: `lemma check` does. Run it when a description changes, on a schedule, and when `lemma check` finds fewer links than expected, not after every edit. A description that does not load is a failure, and prints the same errors as `lm check`.

It exits with 0 when no finding is an error, 4 when one is, and 1 on failure.

The MCP server has a `verify` tool with the arguments `module`, `all` and `links`, so agents reach it the same way ([Commands](commands.md#lemma-mcp)).

## Findings

### Wrong configuration

| Finding | Severity | Meaning |
| --- | --- | --- |
| `include_matches_nothing` | error | An `include` or `exclude` pattern matches no tracked file. |
| `document_missing` | error | A document a service exports does not exist. |
| `document_unreadable` | error | A document does not parse, or describes no operations. |
| `document_references_not_followed` | error or warning | A document refers to other files or URLs (`$ref: ./paths/users.yaml`). Only references within the document (`#/...`) are followed, so the paths and schemas kept elsewhere are not read. It is an error where a whole path is kept elsewhere, since that path's operations are missing from every check, and a warning where only parts of an operation are. Export the document bundled into one file. |
| `schema_unresolved` | error | A type or document location does not resolve. A schema command, resolver or adapter that fails to run is not reported here: `lemma check` warns about it (`resolver`, `adapter`) and fails on it at `--level strict`. |
| `schema_open` | informational | A schema uses types Lemma could not follow, so the fields under them go uncompared. |
| `runtime_missing` | error | An interpreter or toolchain the service's `runtime` names does not exist. |
| `external_unpinned` | warning | A repository named by `git(...)` has no pinned commit, so what it is read at can change. |
| `service_empty` | error | A service reads no files and offers nothing. |

### Outdated links

| Finding | Severity | Meaning |
| --- | --- | --- |
| `operation_not_documented` | error | A link names an operation its service's documents do not describe, or that is no longer declared. |
| `anchor_unresolved` | error | The `implementation` of an operation a declared link names does not resolve. An operation no link names is not checked. |
| `anchor_uncalled` | warning | Nothing outside its type calls an anchored method, while a sibling method of the same type in the same file is called. The anchor probably names the wrong method (a generated client's `Invoke` where the code calls `InvokeWithRuntimeEnv`), and every trace from it is empty. |
| `previously_still_present` | warning | An operation says what it used to be named (`previously`), and that name is still declared, so the claim of a rename is false. |
| `link_redundant` | informational | A declared link is one inference finds too, and says nothing more. |

`lm check` reports a link naming an operation its service does not have when the service discovers no operations. A service with `operations:` might discover the name, so `lm check` cannot rule it out; this command can.

To find `anchor_uncalled`, Lemma asks language servers who calls each link's caller anchor. Lemma reports a repository whose language has no server installed as not checked. When the Lemma daemon is running, its language servers answer, and stay warm for the next command; without one (or with `--no-daemon`), `lemma verify` starts its own, which takes seconds per repository.

## Candidates

A candidate is a suspected gap in the description: found by a pattern, not shown from the code, so it can be wrong. Candidates are listed after the findings, counted on the last line when there are any, and never change the exit code.

```text
candidate #web:scripts  2 places in 1 files write the paths of 2 described operations, and no service includes it
        first: scripts/seed.ts:1  GET /v1/invoices/{}
        [calls_outside_services]
```

| Candidate | Meaning |
| --- | --- |
| `calls_outside_services` | Code in a directory no service includes writes the paths of described operations. If it is a consumer, nothing checks it: a change that breaks it gives no finding. |

The location is the outermost directory that holds the code and nothing a service includes, which is what an `include` would have to add. Each place is one Lemma would read as a call if a service included the file: a path in a string or a request, with the method where the call shows one. To settle a candidate, add the directory to a service's `include` (or give it a service of its own), or add it to a service's `exclude` to say it is left out on purpose.

What the pattern looks for, and what it leaves out:

- Tracked code files that no service includes and none excludes. Test files are counted apart ([below](#what-was-looked-at)).
- A path counts with its leading slash or without, and under a base URL only where the code left out of the same repository names the base, as inside a service.
- A path of one fixed segment (`/health`, `/users/{id}`) counts only in a file that also writes a path of two or more: alone, such a string is written in too much code that asks nothing of the service.
- A route a file registers is served, not called. A repository the workspace does not compose is not read at all.

## What was looked at

Every run ends by saying what it read, so a result with no findings is not taken for more than it is:

```text
looked at
        9 repositories, 19 operations, 175 calls of them read from code
        tests are not read: 135 places in 5 test files write a described operation's path (#web 96, #api 39)
        212 included files are in languages Lemma does not read (#web Vue 212)
        16 requests not read: 10 computed, 4 joined, 2 wrapped
          #web:web:src/agent.js:20  computed  `${API_ROOT}${url}`
          #web:web:src/articles.service.ts:22  joined  '/articles' + (config.type === 'feed' ? '/feed' : '')
          and 14 more (--all lists them)
```

A call of an operation is a place in code Lemma read as a request of its path.

Test files are never read as calls, whatever a service includes: a test is checked by running it, and tests also write paths they mock or expect to fail. A test file is one named `*.test.*`, `*.spec.*`, `*_test.go`, `*_test.py` or `test_*`, or under a `test`, `tests` or `__tests__` directory. The line says how many places in tests write a described operation's path, per repository, found as [candidates](#candidates) are. A change to those operations is not checked against them.

Files in a language Lemma has no reader for are not read at all: no request, route or name in them is seen, and nothing in `lemma check` rests on them. The line counts the ones the services include, tests left out, per repository and language. The languages counted are Astro, C#, Dart, Elixir, Handlebars, Java, Kotlin, PHP, Ruby (with ERB), Scala, Svelte, Swift and Vue; a file of a type Lemma does not know is not counted.

A request not read is a call that looks like a request and that Lemma could not read as a call of any path, with why and the path argument as the code writes it:

| Reason | Meaning |
| --- | --- |
| `computed` | A request function is given a path computed elsewhere: `fetch(url)`, `axios.get(endpoint)`. Often a wrapper's own request, whose callers are read where they write the path. |
| `joined` | The path may go on past what is written: a piece glued to it, or another argument a wrapper joins to it (`api.get('articles', slug)`). |
| `unproven` | A single word that is a path only if what it is given to makes requests, which nothing in the file shows (`store.get('user')`). |
| `wrapped` | A path's first word given to a method not named for an HTTP verb, on something the file makes requests through (`Api.query('articles')`). |

These requests may call a described operation; no finding says anything about them. With `--json` the same is under `looked_at`: `repositories`, `operations`, `calls`, `calls_in_tests` (each with `repository`, `files` and `places`), `unread_files` (each with `repository`, `language` and `files`) and `unread_requests` (each with `file`, `line`, `reason` and `written`). `lemma check` notes the ones a change adds ([`change_adds_unread_request`](check.md#lemmahttp-requirements)).

## Limits

A clean result does not mean nothing is missing. Of what a description can leave out, `lemma verify` looks only for code that writes described paths ([candidates](#candidates)). It does not yet look for an OpenAPI document no service exports, route code no service includes, a call over a queue no operation describes, or a repository the workspace does not compose. The [`lemma-audit` skill](../skills/lemma-audit/SKILL.md) works through those by hand.

There is no way yet to accept a finding or a candidate with an `allow`; a service's `exclude` settles a candidate.
