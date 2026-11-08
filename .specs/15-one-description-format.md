# One description format: removing YAML

**Status: proposal, 2026-11-08.** The rows marked *decided* under [Decisions](#decisions) were settled in discussion. Everything else is proposal. Nothing here is implemented. Statements marked *observed* were run on 2026-11-08 against build 8541531, on small probe workspaces unless stated otherwise. Code references describe that tree.

This builds on [the `.lm` grammar](13-lm-grammar.md), whose first phase lowered `.lm` into the structures YAML already used. It removes the other half. [Document 16](16-verifying-descriptions.md) gives `lemma verify` a new job, which is what lets its YAML manifest go.

## Problem

Lemma has three YAML formats and one language that can replace them:

| Format | Read by | `.lm` replacement |
| --- | --- | --- |
| `lemma.yaml`: a repository's services, exports and links, or a workspace's repositories | `lemma check`, `lemma index` and everything built on the index | A root `mod.lm`, for `lemma check` only |
| Grammar YAML: relations, rules and requirements, including the built-in `grammars/http-contracts.yaml` | `lemma verify`, and `lemma check` when its parent is YAML | `lemma.http`, which a test requires to compile to the same program |
| The `lemma verify` manifest: `manifest.yaml`, with repositories, roles and a change | `lemma verify` | None. Only the grammar it names can be `.lm` |

Having two formats costs three things.

**The documentation teaches both.** The quickstart and tutorials use `mod.lm`. The README, `docs/check.md`, `docs/getting-started.md` and most skills use `lemma.yaml`. A reader cannot tell which is current.

**The formats are not interchangeable, and the difference is silent.** *Observed:* `lemma index` on a repository with only `mod.lm` indexes every supported file and warns "no lemma.yaml here", because `selection::locate` looks only for that name. The same repository with a `lemma.yaml` indexes only what its `include` says. A repository that follows the quickstart is in the first case. A `mod.lm` workspace is not recognised as a workspace by the indexer, and `workspace_file` in the engine has the same limit.

**Evidence covers the format nobody is being taught.** When a directory has both files, `lemma.yaml` wins (`entry_manifest`), so the gates run on YAML. `.lm` equivalence was last shown on 2026-11-06: 709 lines of identical output on a six-repository workspace, and 54 replayed changes.

## Goal

After this work, a Lemma description is always `.lm`. Every command reads it the same way, and Lemma has no code that parses a Lemma-owned YAML format.

## Decisions

| Decision | State |
| --- | --- |
| Remove all three formats rather than deprecate them | *decided* |
| No compatibility period, aliases or migration hints. Lemma has no external users yet | *decided* |
| `lemma verify` stops checking changes and checks descriptions instead ([document 16](16-verifying-descriptions.md)), so its manifest is deleted, not ported | *decided* |
| Implementation starts after the agent benchmark round in progress | *decided* |
| Implementation also waits for the sentry and pre-launch branches to merge | proposal |
| A leftover `lemma.yaml` is an ordinary unread file | proposal |
| Every command loads a description with the one loader `lemma check` uses, type-checking included | proposal |
| A link naming a declared operation that no longer exists is a finding, not a load failure | proposal |
| `--change` accepts a repository's directory name as well as its name | proposal |
| `lm convert` is removed in the last step, once nothing needs it | proposal |
| A missing `local(...)` repository is an error. A `git(...)` repository that is not here is cloned, asked about or reported, by the `clone` setting | *decided* |
| A check that could not cover every repository exits 5 | *decided* |

## What stays

**YAML that belongs to users.** OpenAPI documents are often YAML, and Lemma must keep reading them (`lemma_http::is_yaml_document`, read in `adapters/mod.rs` and `extract.rs`). The parser dependency therefore stays, in those two places only. `lemma-grammar` can drop it altogether.

**The in-memory structures.** `.lm` lowers into `config::LemmaFile` and `lemma_verify::Manifest`, and `lemma check` builds a `Manifest` itself (`infer.rs`). Those types remain as lowered forms. They lose `Deserialize` and any comment that presents them as a file format. Renaming them is optional and separate.

**The fallback for old commits.** A commit that predates a repository's description is read with the working directory's (`language_manifest`). The replay gate depends on this, so it stays, without the choice between two file names.

## Field parity

Read through `lemma-graph/src/lm/mod.rs` against `config.rs`, every field `lemma.yaml` can set has a lowering from `.lm`:

| `lemma.yaml` | `.lm` |
| --- | --- |
| `index.include`, `exclude`, `languages` | `index` |
| `runtime`, its interpreters and `env` | `runtime` |
| `rejects_unknown_fields`, `exports.symbols`, `exports.routes` | The fields of those names, `routes` holding `register` and `define` |
| `exports.openapi` | `http.openapi(...)` in `operations` |
| `exports.operations_from`, with `adapter`, `command`, `entry`, `directory`, `setup`, `env`, `exclude`, `timeout_ms` | `lemma.hono.app`, `lemma.fastapi.app`, `http.adapter`, `http.adapter_command` |
| `exports.client`, with `generated_from`, `specs`, `call_names` | `client` |
| `exports.operations`, with `path`, `previously` | `operation`, `implementation`, `previously` |
| `schema`, with code, a document, or a `resolver` and its `output`, `env`, `cache` | `request`, `response` (or `sends`, `accepts`) |
| `#name` with `git`, `ref` or `path` | `repo name = git(...)` or `local(...)` |
| `graph.links`, `graph.level` | A graph of `Link`, and `workspace.config.level` |

`LowerError::Unsupported` rejects only what `lemma.yaml` cannot say either, such as two adapters on one service.

One field has no counterpart: `manifest`, used alone to name a second `lemma.yaml` in one repository. In `.lm` a repository has one root module, with others below it by their place, so such a repository becomes one root module and submodules. Lowering only ever sets the field to `mod.lm`, so it goes in the last step.

## Changes

### 1. One way to read a description

`lemma index`, the daemon and lookups read a root `mod.lm` through the loader `lemma check` already uses. That covers:

- `selection::locate`, `read_lemma_file`, `is_workspace` and `repository_selection`
- `workspace_file` and the refresh path in the engine
- the `lemma.yaml` default of every CLI argument, `config::LEMMA_FILE`, and `entry_manifest`, which has nothing left to choose between
- the MCP tool descriptions, error messages and log lines that name the file

A directory without `mod.lm` still gets every supported file indexed, with a warning naming `mod.lm`.

A description in which no service names its code, such as one service that is only an API document or a module of only requirements, also gets every supported file indexed, keeping its exports, with a warning. A workspace reports that warning under the member.

**One loader, because it is cheap.** *Observed:* `lm check` loads, resolves and type-checks a 13-module description of a real six-repository workspace in under 10 ms (the resolution of the timer) and 5.7 MB. The repositories there are local paths. A workspace pinning Git revisions reads through Git and should be timed in step 1. At this cost a separate parse-only reader is not worth two readers that could disagree.

Two behaviours need care, because a `mod.lm` is not one self-contained file:

- **The exports fingerprint.** `export_sources` holds a hash of the lowered description and the snapshot, so unchanged exports are not rebuilt. A root module can import others, and hashing the lowered form already covers what they contribute, so an edit to an imported module rebuilds exports. A test holds this. If sentry has landed, its answer fingerprint needs the same treatment.
- **A description that does not type-check.** A refresh that fails keeps the last good exports, so a half-edited file does not stop navigation. A half-edited `mod.lm` is far more likely to be temporarily invalid, so a failed refresh says so: a query prints a warning before its answer. JSON output and `lemma graph --format dot` or `mermaid` have no place for a note, so there the warning goes to the log. `lemma graph` run from a workspace follows callers across it unasked, and a failure there becomes a note on the graph. With `--workspace` it is still an error.

### 2. A removed operation stays a finding

With YAML, a link to an operation that a change removed is a `link` finding, marked introduced, and `lemma check` exits 4. With `.lm` the link's end is a name, so the module no longer type-checks.

*Observed:* removing a declared operation while another repository's link still names it makes `lemma check --change` print the type error and exit 1. Exit 1 means Lemma failed. The change is not reported, nothing says it introduced the problem, and no other repository gets checked.

This is the common breaking change, and the migration would make it worse. No gate covers it, because the gates' removed operations are discovered from documents, and those are still findings (*observed*).

`lemma check` therefore tolerates one class of type error: an end of a link naming an operation its service does not have. The link's text still gives the address, so lowering keeps the link and the existing `persists` check reports it as introduced or already there, as it does for YAML. Every other type error still stops the check with exit 1. `lm check` and editors keep reporting it as an error, because there it is one.

### 3. One grammar path

- Delete `grammars/http-contracts.yaml`, `lemma_verify::HTTP_CONTRACTS`, `parse_grammar` and the grammar's serde form. `lemma.http` is always the built-in grammar.
- Delete the test that requires the two to compile to the same program, after the gates in step 3 of the [rollout](#rollout) pass. It stops meaning anything once one side is gone.
- Replace `yaml_serde::Value` and `Mapping` as the data model of `lemma-grammar`. `GrammarFile.entities`, `relations` and `attributes`, facts' `values`, `RelationSpec` and the helpers in `compile.rs` take a small typed value instead. Today even a `.lm` grammar is built as a YAML value and read back (`lm.rs`, `relation_spec`).
- Inline YAML grammars in tests (`lemma-grammar`, and `demand.rs`, `extract.rs` and `lib.rs` in `lemma-verify`) become `.lm` modules.

`lemma check` keeps reading a finding's values by the names `lemma.http` uses. That coupling is noted in [spec 13](13-lm-grammar.md) and is not changed here.

### 4. Moving change checking to `lemma check`

`lemma verify`'s change checking overlaps `lemma check`, except for the following:

| `lemma verify` has | `lemma check` today | Action |
| --- | --- | --- |
| `--explain` | Absent | Add to `lemma check` |
| `--evaluation both` | Documented as `demand` or `full` | Add `both`. Two gates depend on it |
| `--relation` | Absent | Move to the new `lemma verify` |
| `grammar:` naming custom rules | A parent module's own requirements, selected by `check main` | Already covered |
| `client_receivers`, `client_modules`, `path_prefixes`, `route_aliases` | Always empty when `lemma check` builds its `Manifest` | See below |

Those four fields exist only in the manifest. `lemma check` has run without them all along, relying on inference. Drop them unless step 3's gates show a loss. If they do, add the specific field that closes the loss to `lemma.http`, with a test that fails without it.

### 5. Repository names

A repository's name becomes its root module's, so it is an identifier: `#payments-client` becomes `#payments_client`. That is how lowering already names it. It changes what findings print, and the documentation must say so once.

The directory keeps its hyphen, and that is what people and agents know it by. *Observed:* on a workspace with `repo pay_web = local("pay-web")`, `lemma check --change pay-web` fails with "the change names `#pay-web`, which the workspace does not compose". The stop hook in `skills/lemma-verify/hooks/stop-check.py` passes directory names, so it would break on every hyphenated repository.

`--change` therefore looks for a repository's name first, then for a repository whose directory has that name. This is the normal way to name a repository, so it is not a compatibility alias.

### 6. The converter

`lm convert` moves every remaining description across, and then goes away with `lemma-graph/src/lm/convert.rs` and `lemma-lm/src/convert.rs`. It has two defects to fix first:

- A link it cannot parse is written as `// could not convert: ...` and conversion carries on, which silently drops the link. During a migration that must be an error.
- *Observed:* a source file named after a service produces an invalid module. `src/api.ts` beside `service api` gives `let api = ...` and `` `api` is declared twice ``. The binding needs a name that cannot collide.

### 7. Documentation, skills and this repository

- **This repository:** `lemma.yaml` becomes `mod.lm`. `bench/lemmabench/scale.py` writes `mod.lm`. The hint in `install.sh` and the `external service` row of the vocabulary in `CONTRIBUTING.md` change.
- **Documentation:** `README.md`, `docs/check.md`, `docs/commands.md`, `docs/getting-started.md`, `docs/onboarding.md`, `docs/introduction.md`, `docs/language.md` and `docs/pre-release.md` describe only `.lm`. `docs/grammar.md` is rewritten around `.lm` or folded into `docs/language.md`. `docs/verify.md` is rewritten for [document 16](16-verifying-descriptions.md).
- **Skills:** every skill that writes or reads `lemma.yaml` moves to `mod.lm`. `skills/lemma-onboarding/manifest-template.yaml` and `manifest-reference.md` are deleted. `skills/lemma-audit/reference.md` becomes a `.lm` reference. The stop hook reads only `mod.lm`.
- **Website:** rebuild the documentation snapshot in `website/content`, and retitle or remove the Cross-repo verification and Grammars entries in `src/docs/navigation.mjs`, since the build rejects an unlisted page.
- **Comments and messages:** `schema.sql`, doc comments and strings such as "is not declared in any lemma.yaml" stop naming the file.
- **Older specs:** documents 8, 9 and 13 are records of their time and stay as written. Each gets a one-line pointer here.

### 8. Repositories that are not here

A module that imports another repository cannot be loaded without it, so what happens when that repository is absent has to be defined.

A `local(...)` repository that is missing is an error, from `lemma index` and `lemma check` alike. There is nowhere to get it from, and a check that guesses is undefined.

A `git(...)` repository that is not in the clone cache follows the `clone` setting, read from `.lemma/config.toml` and then from the user's `config.toml`:

| `clone =` | Interactive terminal | Otherwise |
| --- | --- | --- |
| `"ask"` (default) | Asks | Clones |
| `"always"` | Clones | Clones |
| `"never"` | Stops at the repositories it has, and warns | The same |

Answering no behaves as `"never"` for that run. `"never"` also forbids fetching a revision the cache lacks. It governs both places a repository is cloned, so `lemma index` and `lemma check` agree.

Only `lemma index` and `lemma check` clone or ask. Queries and MCP tools refresh the index constantly and have no terminal, so to them a repository not yet cloned is unavailable: they answer from the last index and warn. `lemma index` runs in the daemon, which has no terminal, so the client asks before handing over.

**Stopping at the repositories it has.** Names imported from an unavailable `git(...)` repository take the type-checker's unknown type instead of being errors. The description still loads, so `index.include` and the repository's own exports hold. Links reaching the absent repository are reported as not checked, by count and repository. A workspace checks the members it has and lists the rest. A `--change` naming an unavailable member is an error, since that is the one thing asked about.

**Exit codes.** `lemma check` exits 5 when it finished but did not cover every repository, so that a partial check cannot read as a pass. Exit 4 takes priority when there are also findings. `lemma index` exits 0 with the warning, since it claims nothing about safety.

## Correctness argument

Both formats already lower to the same structures, and [the field parity](#field-parity) holds, so the risk is not in evaluation. It lies in three places: code that reads one format and not the other, failures that are findings in YAML and load errors in `.lm`, and users of the manifest-only fields. The rollout proves equality while both formats still exist, and deletes YAML only after that.

Only two gates drive `lemma verify`: Verify and Mutation equality, both through `--evaluation both`. Mutation recall, replay, fixtures and scale already use `lemma check`.

## Rollout

Each step is its own change and must pass before the next starts.

0. **Prerequisites.** The benchmark round finishes, and the sentry and pre-launch branches merge, since both touch `check.rs`, the daemon and the skills.
1. **Indexing reads `mod.lm`.** *Acceptance:* on the five fixture systems and the six-repository workspace, an index built from `mod.lm` has the same exports, links and symbols as one built from `lemma.yaml`. An edit to an imported module rebuilds exports. A mid-edit type error keeps navigation working. Load cost is measured on a pinned workspace.
2. **Findings and names.** A removed declared operation is a finding. `--change` takes directory names. The converter's two defects are fixed. *Acceptance:* a test for each, including a removed declared operation reported as introduced with exit 4, and other repositories still checked.
3. **`.lm` everywhere, YAML still present.** Add `--explain` and `--evaluation both` to `lemma check`. Convert the private gates' descriptions and move the two gates that drive `lemma verify` to `lemma check`. *Acceptance:* every gate (verify, mutations, recall, replay, fixtures, scale) matches its current baseline from `.lm`, with recall no lower. Any difference is explained or fixed, not re-baselined.
4. **Delete grammar YAML** and the YAML value model. *Acceptance:* gates unchanged. `lemma-grammar` no longer depends on the parser.
5. **Delete `lemma.yaml`, the verify manifest and `lm convert`, and land the new `lemma verify`** ([document 16](16-verifying-descriptions.md), step 1) in the same change, so the command name never means nothing. *Acceptance:* gates unchanged. A search for the parser finds only the OpenAPI readers.
6. **Documentation, skills and site.** *Acceptance:* the site's documentation check passes, and a fresh reader following the quickstart and onboarding never meets YAML.

[Repositories that are not here](#8-repositories-that-are-not-here) is its own change, after step 2 and before step 3, since it needs the same kind of tolerance as a removed operation. *Acceptance:* a test for each row of the table, for a missing `local(...)` being an error, and for exit 5.

Benchmarks run before step 3 are not comparable with those after it, because the format changes what agents read. Take a new baseline after step 5.

## Open questions

- Should `lm check` remain a separate command once `lemma verify` exists? This document keeps it, because it needs no index and is what an editor runs.
- Should `lemma-verify` be renamed? After this work, it extracts facts and evaluates grammars for `lemma check`, and the name will mislead.
