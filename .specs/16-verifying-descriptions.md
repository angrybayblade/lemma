# `lemma verify`: does the description match the code?

**Status: step 1 of the [rollout](#rollout) implemented, 2026-11-08; the rest is proposal.** The rows marked *decided* under [Decisions](#decisions) were settled in discussion. Statements marked *observed* were run on 2026-11-08 against build 8541531, on a small probe workspace, before this work. Numbers marked *target* are unmeasured. Findings name a location as a repository and service, not yet a file and line.

This depends on [document 15](15-one-description-format.md), which removes YAML. `lemma verify` then only ever has to understand `.lm`.

## Problem

`lemma check` only sees what a description says, so a clean check on an incomplete description looks exactly like a clean check on a complete one. This is Lemma's most dangerous failure, because it reads as safety.

*Observed:* at level `check`, with no change named, on a workspace with one link:

| The description says | `lm check` | `lemma check` |
| --- | --- | --- |
| `index: { include: ["nope"] }`, a directory that does not exist | 0 errors | 0 findings, exit 0 |
| `http.openapi("missing.json")`, a document that does not exist | 0 errors | 0 findings, exit 0 |

Today the only protection is `skills/lemma-audit`, in which an agent works through a table of searches by hand. Most of that table is mechanical, and it should be a command.

Meanwhile `lemma verify` duplicates `lemma check` under a different manifest, which gives it no reason to exist as it stands.

## Three questions, three commands

| Command | Question | Reads | Compares |
| --- | --- | --- | --- |
| `lm check` | Is this module valid? | `.lm` files only | Nothing |
| `lemma verify` | Does this description still match the code? | One snapshot of the code | The description with the code |
| `lemma check` | Does this change break anything? | Base and head | Base with head |

`lemma verify` never compares two commits and never reports a contract disagreement. Whether a caller's schema agrees with its provider's is `lemma check`'s question, even at a single snapshot.

## Decisions

| Decision | State |
| --- | --- |
| `lemma verify` does not look for breaking changes | *decided* |
| It checks a description's consistency with the code: missing links, outdated links, wrong configuration | *decided* |
| Missing links split into certain findings and candidates, and candidates never fail | proposal |
| One detector serves `lemma verify` and `lemma check` | proposal |
| Known exceptions use `allow` | proposal |
| It reports what it looked at | proposal |
| It proposes edits and does not apply them | proposal |
| It reads the working directory only | proposal |

## Interface

```text
lemma verify [mod.lm] [--all] [--links] [--relation <name>] [--json]
```

| Option | Meaning |
| --- | --- |
| `--all` | List every candidate and informational finding, not just their counts and the first ten. |
| `--links` | Also list every link, declared and inferred. Moves from `lemma check`. |
| `--relation <name>` | Also print every row of a derived relation. Moves from today's `lemma verify`. |
| `--json` | Findings, candidates and what was looked at, as JSON. |

`--anchors` moves here from `lemma check` and stops being optional. An anchor nothing calls is a description problem, and this is the command that is allowed to be slow. Exits 0 when no finding is an error, 4 when one is, and 1 on failure, as the other commands do. A description that does not load is a failure, and prints what `lm check` prints.

There is no `--change`, `--base` or `--head`, and no option for another commit. Language servers read a working tree, so an older commit could not be verified as fully. CI verifies what it checked out.

The entry module sets the scope. A workspace module verifies every repository it composes. A repository's root module verifies that repository alone, and the findings that need other repositories are reported as not looked at.

The command refreshes the index first and uses the running daemon's language servers, or starts its own, as `--anchors` does. It runs what the description says to run (adapter commands and schema resolvers), as `lemma check` does, so it needs a repository's dependencies installed. Where they are not, that is reported under [what was looked at](#reporting-what-was-looked-at).

The `check` MCP tool gets a sibling, `verify`, with the same options, so agents reach it the same way.

```text
$ lemma verify
error   billing/mod.lm:12  exports `openapi/v2.json`, which does not exist
        [document_missing]
error   web/mod.lm:9  `implementation` names `src/charges.ts:createCharge`, which does not resolve
        [anchor_unresolved]
error   billing/src/refunds/routes.ts:3  registers routes, and no service includes it
        billing/mod.lm:8 could read:  index: { include: ["src/api", "src/refunds"] }
        [code_outside_services]
2 candidates
        web/src/jobs/publish.ts:18  imports a queue client and mentions `send_message`; no operation describes it
        [undeclared_channel]
looked at
        5 repositories, 12,431 files, 1,204 operations, 38 declared links, 521 inferred calls
        not read: go in `gateway` (gopls did not start); anchors there were not checked
3 errors, 0 warnings, 2 candidates
```

## Findings

A finding has a location in the description or the code, a name an `allow` can select, and the evidence that shows it. Where the fix is mechanical it also carries the proposed edit, as `.lm` at a file and line. The `Today` columns say what happens now, and where that was not run, they say so.

### Wrong configuration

| Finding | Meaning | Today |
| --- | --- | --- |
| `include_matches_nothing` | An `include` or `exclude` pattern matches no tracked file | Silent (*observed*) |
| `document_missing` | An exported document does not exist | Silent (*observed*) |
| `document_unreadable` | A document does not parse or describes no operations | Not run |
| `schema_unresolved` | A type or document location does not resolve, or a schema command fails | Not run |
| `schema_open` | A schema uses types Lemma could not follow, so fields go uncompared. Warning | Informational, per `docs/check.md` |
| `runtime_missing` | A named interpreter or runtime does not exist | Not run |
| `external_unpinned` | A repository named by `git` has no pinned commit. Warning | Manual |
| `service_empty` | A service reads no files and offers nothing | Not run |

`lm check` keeps whatever needs no code. Anything that has to open a repository belongs here.

### Outdated links

In `.lm`, a link to a declared operation is a name. If the declaration goes, `lm check` reports it, so a link cannot quietly point at a declaration that is gone. What can drift is the code behind a declaration, and operations discovered from documents.

| Finding | Meaning | Today |
| --- | --- | --- |
| `operation_not_documented` | A link names a discovered operation that its service's documents do not describe | `lemma check`, exit 4 (*observed*) |
| `anchor_unresolved` | An operation's `implementation` does not resolve | `lemma check`, exit 4 (*observed*) |
| `anchor_uncalled` | Nothing calls an anchored method, while a sibling is called. Warning | `lemma check --anchors` |
| `previously_still_present` | A rename's old name still exists, so the claim of a rename is false. Warning | Absent |
| `link_redundant` | A declared link is one inference finds too, and says nothing more. Informational | Absent |

### Missing, certain

Shown from the code, with no judgement required about what it is:

| Finding | Meaning | Today |
| --- | --- | --- |
| `document_not_exported` | A tracked file inside a service's `include` parses as an OpenAPI document, and is in no service's `operations`. Recognised by its content, not its name | Manual |
| `code_outside_services` | Route registrations, recognised by the same adapters and `routes.register` names that index a service, in a tracked file no service includes and none excludes | Manual |

Certain does not mean unwanted. A vendored document is correctly reported and intentionally left out, which is what [`allow`](#accepting-a-finding) is for.

### Missing, suspected: candidates

| Candidate | Meaning |
| --- | --- |
| `undeclared_channel` | A publish, consume or subscribe to a queue or topic that no operation describes |
| `undeclared_invocation` | A function runtime invoked by name, such as a Lambda `Invoke`, that no operation describes |
| `undeclared_rpc` | An RPC stub called that no operation describes |
| `undeclared_webhook` | An outbound request to a URL built from configuration, or a route shaped like a webhook receiver |
| `unrecognised_client` | Code imports a package built in this workspace, and calls into it resolve to nothing. It may be a client, or just a library |
| `uncomposed_repository` | A sibling directory is a repository with a `mod.lm`, and this workspace does not list it |
| `document_outside_services` | A tracked OpenAPI document that no service includes. It may be a served API or a third party's |

Candidates can be wrong. They are never errors and never affect the exit code. Each has a location, the pattern that matched and the operations a fix would add. An agent or a person decides what they mean, which is what is left of the `lemma-audit` skill.

The patterns need no new extraction. The relations that `lemma.http` reads from code include `imports(file, imported)` and `mentions(scope, name)`, so a pattern is an import plus a mentioned name, written as ordinary `.lm` rules. That also means a team can add its own queue library. `mentions` knows names and not receivers or arguments, so precision may be poor, and [it is measured](#measuring-it) before a pattern ships.

## Reporting what was looked at

A clean result must not be read as "nothing is missing", and missing links can never be a guarantee. Every run says which repositories, files, operations and links it read, what it skipped and why, and which language servers did not start. A language it could not read is a statement in the output. The JSON form carries the same data, so CI can fail on reduced reach if a team wants that.

## One detector, two questions

If a change breaks a description, `lemma check` must still fail. So the checks in [Outdated links](#outdated-links) and those in [Wrong configuration](#wrong-configuration) that a change can cause are one detector, in `lemma-graph`, returning findings for a snapshot. `lemma verify` reports everything the detector returns. `lemma check --change` runs it at base and head and reports what the change introduced, as it treats other findings today.

So `lemma check` gains two new reports as a side effect: a change that deletes an exported document, and one that empties an `include`, stop being silent.

The missing-link detectors and candidates run only in `lemma verify`. They read the whole system, and a change-scoped check could not afford them.

A declared operation removed while a link still names it is a type error, and [document 15](15-one-description-format.md#2-a-removed-operation-stays-a-finding) turns it into a `lemma check` finding. `lemma verify` needs no finding of its own for that case. The same tolerance gives it a location and `lm check`'s message.

## Accepting a finding

A finding that is correct and intended, such as a vendored OpenAPI document nobody serves, needs a way to stop failing CI. `allow` already means this.

A new standard-library module, `lemma.verify`, declares one relation and one requirement per finding and candidate, with named variables. The detector supplies the relation's rows, and from there the findings travel the path a grammar's findings already do: `Allowed::cover_findings` matches a requirement's name and reads the values as text.

```lm
allow lemma.verify.document_not_exported {
    when: file == "vendor/payments.json"
    reason: "a partner's API, kept for reference"
    until: "2027-06-30"
}
```

`lemma verify` runs whole, so covering nothing means something here, as it does for a behavior ([spec 13, section 8](13-lm-grammar.md)). An allowance that covers no finding is reported, and one past its `until` stops covering. Candidates can also be allowed, so that a reviewed one does not come back.

## Cost and when to run

The command reads the whole workspace with full evaluation and asks language servers about anchors. Change-driven evaluation does not apply, because there is no change. It is expected to take as long as a cold `lemma check --links`, plus the anchor questions. *Target:* under a minute on a six-repository workspace.

Run it:

- at the end of onboarding, as the criterion for a description being done
- in CI, whenever a change touches a `.lm` file, and on a schedule, because code can drift from a description without touching it
- when `lemma check` finds fewer links than expected

Do not run it after every edit, and do not add it to the stop hook.

## Skills

- `skills/lemma-verify` teaches `lemma check`, so it is renamed `lemma-check` before the command's meaning changes. An agent reading the old name would run the wrong command.
- `skills/lemma-audit` runs `lemma verify`, applies what it certainly finds, and uses judgement on candidates. Its table of searches goes away.
- `skills/lemma-onboarding` ends with a clean `lemma verify`.

## Determinism

Findings are bound to one snapshot and sorted. What a language server contributes (anchors) depends on it having started, which is why reach is reported rather than assumed. Patterns are versioned with Lemma. A changed pattern can change candidates between releases, but it never changes the exit code.

## Where the code goes

- `lemma-graph`: the detector, split by kind of finding, reading the lowered description and the facts inference already builds. It moves the existing `link` and anchor findings rather than copying them.
- `lemma-lang` standard library: the `lemma.verify` module of relations and requirements, and the candidate patterns as rules.
- `lemma-cli`: the `verify` command, its options, its output and the MCP tool.

## Measuring it

`lemma check` is trusted because recall is measured. `lemma verify` needs the same.

**Fixtures, in this repository.** One small system per finding, with the finding present and with it fixed.

**Description mutation, in the private gates.** Start from a description audited by hand and apply one mutation at a time:

| Mutation | Expected |
| --- | --- |
| Remove a document from `operations` | `document_not_exported` |
| Mistype a document's path | `document_missing` |
| Remove a directory from `include`, where it holds routes | `code_outside_services` |
| Mistype an `include` | `include_matches_nothing` |
| Rename the symbol behind an `implementation` | `anchor_unresolved` |
| Point an `implementation` at an uncalled sibling | `anchor_uncalled` |
| Remove the pin of a `git` repository | `external_unpinned` |
| Remove a repository from the workspace | `uncomposed_repository` (candidate) |
| Remove a declared link over a queue or a function runtime | A candidate, if a pattern knows it. Otherwise counted as a miss |

Removing a link for a documented HTTP call is not a mutation worth testing, because inference finds that call anyway. *Target:* every mutation with a certain finding is found. Candidate mutations report a number and carry no target until there is a baseline.

**Clean baseline.** The audited description's findings, reviewed by hand and frozen, as the replay baseline is. A new finding on unchanged code is a false positive until shown otherwise.

**Candidate precision.** Review candidates on the same workspace. *Target:* at least half point at something real. A pattern below that is removed rather than shipped noisy.

## Rollout

1. Rename the skill. Land the new command with the wrong-configuration and outdated-link findings, in the change that deletes today's `lemma verify` ([document 15](15-one-description-format.md#rollout), step 5). Move `--anchors` and `--links`. *Acceptance:* fixtures and the clean baseline.
2. Add the certain missing-link findings. *Acceptance:* description-mutation recall on the private workspace.
3. Add `lemma.verify`, allowances, reporting on reach, and proposed edits. *Acceptance:* every finding with a mechanical fix proposes an edit that makes it go away when applied.
4. Add candidates, one family at a time. *Acceptance:* the precision target.
5. Replace the audit skill's table, and end onboarding with `lemma verify`.

## Prior art

- **`buf lint` and `buf breaking`** split "is this definition well formed" from "does this change break consumers" for Protobuf. That is the same separation as `lemma verify` and `lemma check`.
- **`terraform plan -refresh-only`** reports drift between a declared configuration and what exists, without proposing a change to either.
- **Knip** finds unused files, exports and dependencies in JavaScript projects by comparing what is declared with what is used, and accepts configured exceptions.

## Open questions

- Is "verify" the right word once it stops verifying changes? `--level audit` already exists on `lemma check`, so `lemma audit` would collide. This document keeps `verify`.
- Should warnings fail in CI? `lemma check` has `--level` for this. `lemma verify` may need the same.
- Can a relation's rows come from a built-in detector in today's language, or does `lemma.verify` need a new kind of provider? [Spec 13](13-lm-grammar.md) has graphs supplied by providers, but its open items include provided graphs for behaviors.
- Could sentry's prepared inputs make this fast enough for more frequent use? Out of scope until it is measured.
