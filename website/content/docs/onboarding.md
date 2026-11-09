# Onboarding

This guide takes a team from nothing to Lemma answering code questions for its agents and checking changes across its repositories in CI. It assumes several repositories that talk over HTTP: services with OpenAPI specs, generated clients, and the code that calls them.

| Step | Gets you |
| --- | --- |
| [1. Install](#1-install) | The `lemma` binary, and optionally the agent skills |
| [2. Code intelligence](#2-code-intelligence) | Symbols, references, definitions and implementations in each repository |
| [3. Agents](#3-agents) | The same questions as MCP tools for coding agents |
| [4. Describe the system](#4-describe-the-system) | A `mod.lm` for each repository and a workspace that composes them |
| [5. Check a change](#5-check-a-change) | Findings for one change, with evidence |
| [6. Calibrate on history](#6-calibrate-on-history) | Confidence that findings mean something in your codebase |
| [7. Run in CI](#7-run-in-ci) | Every pull request checked against the repositories that depend on it |

Before relying on Lemma, read the [pre-release checklist](pre-release.md): it lists what this release does not do yet.

## 1. Install

Releases ship `lemma`, and `lm` for `.lm` modules, for macOS and Linux on arm64 and x86_64:

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
```

The script installs the newest release, pre-releases included, into `~/.local/bin`. Set `LEMMA_VERSION=v0.1.0-rc1` to pin a version, `LEMMA_INSTALL_DIR` to change where it goes, and `LEMMA_SKILLS=1` to also install the agent skills into `~/.claude/skills` (see [Agent skills](#agent-skills)). If GitHub's API rate limit stops the script finding the newest release, set `LEMMA_VERSION` or `GITHUB_TOKEN`.

From source instead: `cargo install --path crates/lemma-cli --locked`, which installs `lemma` and `lm` (Rust 1.85 or later).

Optional but recommended for `lemma check`: [Soufflé](https://souffle-lang.github.io), `brew install souffle` on macOS. Without it Lemma uses its built-in engine, which gives the same results about six times slower.

Check the install:

```sh
lemma --version
lemma providers      # which language servers Lemma found
```

## 2. Code intelligence

Lemma asks each language's own server, so install the ones your code needs:

| Language | Install | Command Lemma runs |
| --- | --- | --- |
| TypeScript, TSX, JavaScript | `npm install -g typescript typescript-language-server` | `typescript-language-server` |
| Python | `uv tool install ty` (fastest) or `npm install -g pyright` | `ty` or `pyright-langserver` |
| Go | `go install golang.org/x/tools/gopls@latest` | `gopls` |
| Rust | `rustup component add rust-analyzer` | `rust-analyzer` |

Servers answer best when the project's dependencies are installed (`npm install`, a Python virtual environment, `go mod download`, `cargo fetch`).

Give each repository a `mod.lm` at its root naming its services and the code each is made of (`index: { include: [...] }`; see [Checking links](check.md#descriptions)), then index it once; the background daemon keeps it current as files change:

```sh
cd ~/code/billing
lemma index .                  # writes .lemma/index.db
lemma symbols Router           # find symbols by name
lemma refs Router.delete       # every reference, from the language server
lemma definition src/app.ts:12:5
lemma implementations Shape.area
lemma signature Router.delete
```

Add `.lemma/` to each repository's `.gitignore`. Every command and option is in [Commands](commands.md); server-specific notes are in [Language servers](providers.md).

## 3. Agents

`lemma mcp` serves the same questions as MCP tools. In Claude Code, from the repository:

```sh
claude mcp add lemma -- lemma mcp
```

Other MCP clients take the same command (`lemma mcp`, over stdin and stdout). The server tells agents when its tools beat text search; in Lemma's benchmark, agents used it mostly when guided to, so the `lemma-usage` skill (below) or a line in your agent instructions helps: "Use Lemma's tools for references, definitions and impact; use text search for strings and one-off lookups."

### Agent skills

The release includes four skills for agents that support them (Claude Code reads `~/.claude/skills` and a project's `.claude/skills`):

| Skill | For |
| --- | --- |
| `lemma-usage` | Choosing between Lemma and text search, navigating code, reading `lemma check` findings |
| `lemma-onboarding` | Setting Lemma up for a set of repositories: install, index, write the descriptions, add CI |
| `lemma-grammar` | Writing and changing relations, rules and requirements |
| `lemma-audit` | Auditing a description against the code and closing the gaps |

Install them with the binary (`LEMMA_SKILLS=1`), or copy `skills/*` from this repository into `.claude/skills/` of the repository where agents work.

## 4. Describe the system

`lemma check` needs a description: which repositories make up the system and what each one is. Each repository describes itself in its `mod.lm`; a workspace module, in a repository every service can reach, composes them, with clones of the others beside it.

Build the description one repository at a time. The examples below describe a billing service, a users service that calls it, a generated SDK client and a web app.

**Start with one service.** Name the OpenAPI files it publishes. If they are generated, commit them, or a change cannot be read at both commits. `billing/mod.lm`:

```lm
module billing

import * as http from lemma.http

export service api {
    index: { include: ["src"] }
    operations: http.openapi("openapi.json")
}
```

**Compose it.** The workspace's `mod.lm` names each repository:

```lm
module system

workspace {
    repo billing = local("../billing")
    repo web = local("../web")
    repo users = local("../users")
    repo billing_sdk = local("../billing-sdk")
    config: { level: check }
}
```

A repository's name is its root module's, so an identifier: the directory `billing-sdk` holds `module billing_sdk`, and findings print `#billing_sdk`. `lm check` reports any mistake in a module with its file, line and column:

```sh
lm check
```

**Read route code.** `index.include` points at the code the service is made of, which holds the routes; Lemma then reports routes the spec leaves out (`route_not_in_spec`, informational), and reads which request schemas are zod `.strict()`. If routes import shared schemas from elsewhere, include those directories too. Lemma derives which paths a service owns from its documents.

**Add the generated clients.** A client generated from a document says so with `http.generated_client`. Generators name functions differently, so say how with `call_names`: `{operationId}` as written, or recased as `{camel}`, `{pascal}` or `{snake}`, with any fixed text around it. Its code is read for what each method requests, and not searched for calls.

```lm
module billing_sdk

import * as http from lemma.http

export service client {
    index: { include: ["src"] }
    client: http.generated_client(generated_from: ["openapi.json"], call_names: ["{camel}"]) // createInvoice stays createInvoice; {snake} would be create_invoice
}
```

**Add the code that calls them.** Point `include` at source directories, not the whole repository, to keep tests, scripts and fixtures out. A service that also calls another service is one repository with one `mod.lm`; its own route declarations are not counted as calls to itself.

```lm
module web

export service web {
    index: { include: ["src"] }
}
```

Calls through clients need no declaration: a call through a client library reaches the client method its names spell, where the code imports the client's package. A string in code counts as a path only if it contains a leading segment some service in the workspace serves (`/v1/` here), so add every service a repository calls before checking its coverage. Imports through `tsconfig.json` `paths` aliases are followed.

**Declare what code does not show.** A service whose code rejects unknown fields without saying so in its documents or zod schemas (Go's `DisallowUnknownFields`, serde's `deny_unknown_fields`) gets `rejects_unknown_fields: true`, so sending a removed field is an error rather than a warning. Calls inference cannot see, such as over a queue or a function runtime, are declared as an operation on each side and a link: see [Checking links](check.md#descriptions).

**Check the description against the code.** This is what makes a description done:

```sh
lemma verify
```

It reports what the description says that the code does not bear out: an `include` matching nothing, a document that does not exist, an `implementation` that does not resolve, and the rest of [its findings](verify.md#findings). Fix each one, until there are none. It does not look for what a description leaves out, so also check coverage: Lemma should see the calls you know about.

```sh
lemma verify --relation calls --json | jq -r '.relations.calls[] | "\(.[1])\t\(.[2])"' | sort -u | less
```

Each row is an operation and the file calling it. Missing calls usually mean an `include` directory is missing, a generated client's `call_names` does not match the generator, or URLs are built at run time from values the code does not show (see the [pre-release checklist](pre-release.md)). The `lemma-audit` skill works through what a description may leave out.

## 5. Check a change

Say which repository changed, and its commits before and after:

```sh
lemma check --change billing --base main --head my-branch
lemma check --change billing --base main --head my-branch --explain     # with the facts behind each finding
```

Commits are read from Git without a checkout, so `--base` and `--head` can be any revisions: `main` and a branch, a merge commit and its parent (`abc123^`), or two tags. Without them, `lemma check --change billing` checks the work in progress since the branch left the default branch. Repositories other than the changed one are read at their `rev`, or their working directory if they have none.

The changed repository can be a service (what its change breaks for callers) or a caller (whether the calls it adds exist). Pinning a repository in the workspace to an older commit, `local("../web", rev: "...")`, asks how the change meets code from before it, as when a service deploys before its callers.

Findings come in three severities. Exit code 4 means at least one error.

| Severity | Means | Example |
| --- | --- | --- |
| error | A request will fail | Code calls a removed operation; sends a field a strict schema now rejects; never sends a newly required field |
| warning | Behaviour changes without failing | Code reads a removed response field, or sends a field the service now ignores |
| info | Worth knowing | Code calls an operation whose request changed but never names the changed field; a route missing from the spec |

Read a finding by its derivation (`--explain`): each line is a fact or rule, indented under what it supports, down to facts from documents and code. A wrong finding is usually one wrong fact near the bottom: a call matched to the wrong operation, or a common name counted as using a field.

## 6. Calibrate on history

Before gating anything, replay changes your team already made and compare the findings with what happened. For each commit that changed a service's spec:

```sh
git -C ../billing log --format=%h -- openapi.json | head -50
```

pin each other repository to the last commit before the change merged (`git rev-list -1 --before=<date> main`) in the workspace with `local("../web", rev: "...")`, and run `lemma check --change billing --base <commit>^ --head <commit>`. Check every error and warning by hand against the pull request and any follow-up in the callers. A sweep of a hundred changes takes a few minutes.

Keep the changes that found real breaks as regression cases for upgrades of Lemma or your requirements.

## 7. Run in CI

Run `lemma check` on pull requests to each service, with the other repositories at their default branches. A GitHub Actions job, in the service's repository:

```yaml
jobs:
  contracts:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
        with: { path: billing, fetch-depth: 0 }
      - uses: actions/checkout@v4
        with: { repository: your-org/workspace, path: workspace, token: ${{ secrets.READ_TOKEN }} }
      - uses: actions/checkout@v4
        with: { repository: your-org/web, path: web, token: ${{ secrets.READ_TOKEN }} }
      - name: Install lemma
        env: { LEMMA_VERSION: v0.1.0-rc1 }
        run: |
          curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - name: Check the change
        run: |
          lemma check workspace/mod.lm --change billing \
            --base "${{ github.event.pull_request.base.sha }}" \
            --head "${{ github.event.pull_request.head.sha }}"
```

The workspace's `local(...)` paths are relative to its `mod.lm`, so check out the repositories where it expects them. The service's clone needs the history between the commits (`fetch-depth: 0`). Repositories named by `git(...)` are cloned in CI, which has no one to ask.

Run `lemma verify` too, whenever a pull request touches a `.lm` file, and on a schedule, since code can drift from a description without touching it.

Start with the job reporting only (`continue-on-error: true`), read its findings on real pull requests for a few weeks, then let errors fail the check. Warnings should stay advisory in this release: they rest on name evidence that can be wrong for common names.

## When something looks wrong

| Symptom | Likely cause | What to do |
| --- | --- | --- |
| A call you know about is missing | Directory not in `include`, `call_names` does not match the generator, URL built at run time | `lemma verify --relation calls`, widen `include` |
| A finding names the wrong operation | Two operations share a path and the call shows no method | Check the call site; the method is taken from the call when written there |
| A warning about a common field name (`id`, `slug`) | The code uses the name for its own objects | Ignore it; see the [pre-release checklist](pre-release.md) |
| Every operation looks removed | A spec moved to a new file | Name the new file in `operations` |
| `no file ... at` | A document path is wrong at every commit read | `lemma verify` reports it as `document_missing`; fix the path |
| Slow runs | No Soufflé | `brew install souffle` |

More in [Troubleshooting](troubleshooting.md).
