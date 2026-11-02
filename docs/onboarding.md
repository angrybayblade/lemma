# Onboarding

This guide takes a team from nothing to Lemma answering code questions for its agents and checking changes across its repositories in CI. It assumes several repositories that talk over HTTP: services with OpenAPI specs, generated clients, and the code that calls them.

| Step | Gets you |
| --- | --- |
| [1. Install](#1-install) | The `lemma` binary, and optionally the agent skills |
| [2. Code intelligence](#2-code-intelligence) | Symbols, references, definitions and implementations in each repository |
| [3. Agents](#3-agents) | The same questions as MCP tools for coding agents |
| [4. Map the system](#4-map-the-system) | A manifest naming your services, clients and consumers |
| [5. Verify a change](#5-verify-a-change) | Findings for one change, with evidence |
| [6. Calibrate on history](#6-calibrate-on-history) | Confidence that findings mean something in your codebase |
| [7. Run in CI](#7-run-in-ci) | Every pull request checked against the repositories that depend on it |

Before relying on Lemma, read the [pre-release checklist](pre-release.md): it lists what this release does not do yet.

## 1. Install

Releases ship `lemma` for macOS and Linux on arm64 and x86_64. While the repository is private, fetch the install script with the GitHub CLI:

```sh
gh auth login    # once
gh api -H 'Accept: application/vnd.github.raw' repos/angrybayblade/lemma/contents/install.sh | sh
```

Once it is public:

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
```

The script installs the newest release, pre-releases included, into `~/.local/bin`. Set `LEMMA_VERSION=v0.1.0-rc0` to pin a version, `LEMMA_INSTALL_DIR` to change where it goes, and `LEMMA_SKILLS=1` to also install the agent skills into `~/.claude/skills` (see [Agent skills](#agent-skills)). Without the GitHub CLI, set `GITHUB_TOKEN` to a token that can read the repository.

From source instead: `cargo install --path crates/lemma-cli --locked` (Rust 1.85 or later).

Optional but recommended for `lemma verify`: [Soufflé](https://souffle-lang.github.io), `brew install souffle` on macOS. Without it Lemma uses its built-in engine, which gives the same results about six times slower.

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

Index each repository once; the background daemon keeps it current as files change:

```sh
cd ~/code/platform
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

The release includes three skills for agents that support them (Claude Code reads `~/.claude/skills` and a project's `.claude/skills`):

| Skill | For |
| --- | --- |
| `lemma-usage` | Choosing between Lemma and text search, navigating code, reading `lemma verify` findings |
| `lemma-onboarding` | Setting Lemma up for a set of repositories: install, index, write the manifest, add CI |
| `lemma-grammar` | Writing and changing grammars: relations, rules, requirements |

Install them with the binary (`LEMMA_SKILLS=1`), or copy `skills/*` from this repository into `.claude/skills/` of the repository where agents work.

## 4. Map the system

`lemma verify` needs a manifest: which repositories make up the system, what each one is, and where its contracts and calls live. Keep it in a repository every service can reach, such as a workspace or infrastructure repository, with clones of the others beside it.

Each repository has a role:

| Role | Is | Lemma reads |
| --- | --- | --- |
| `service` | Serves HTTP operations | Its OpenAPI specs (`specs`), and route code (`routes`) to check the spec against the code and tell strict schemas |
| `client` | A generated client | Operation tables mapping methods to operations (`tables`), or the spec it was generated from (`generated_from`) |
| `consumer` | Calls services | Its code (`include`): paths in strings and calls through clients |

One clone can appear under several names. A monorepo holding a service and a caller of another service is two entries with the same `path`.

Build the manifest one repository at a time.

**Start with one service.** Name the OpenAPI files it publishes. If they are generated, use the committed copies.

```yaml
change: { repository: platform, base: main~1, head: main }
repositories:
  platform:
    path: ../platform
    role: service
    service: apollo                       # name used in findings
    specs: [apps/apollo/openapi-v3.1.json, apps/apollo/openapi-internal-dashboard.json]
```

```sh
lemma verify manifest.yaml --all
```

With only a service, findings are empty, but the run proves the specs parse. `--relation removed_operation` shows what the change removed.

**Add the route code.** `routes` points at the directories declaring routes; Lemma then reports routes the spec leaves out (`route_not_in_spec`, informational) and reads which request schemas are zod `.strict()`. If routes import shared schemas from elsewhere, add those directories as `schemas`. If the service publishes routes under a second prefix, as Apollo serves `/api/v3/` routes as `/api/v3.1/`, add `route_aliases`.

```yaml
    routes: [apps/apollo/src/pages/api]
    schemas: [apps/apollo/src/lib, apps/apollo/src/common]
    route_aliases: { /api/v3/: /api/v3.1/ }
```

**Add the generated clients.** A client with committed operation tables names them; a client generated at build time from a spec names the spec, and its functions are taken to be the camel-cased `operationId`s.

```yaml
  composio-client:
    path: ../composio-client
    commit: main
    role: client
    tables: [src/compat/generated-operations.ts, python/src/composio_client/_operations.py]
    specs: [spec/openapi.json]

  thermos-client:                         # Apollo's client for Thermos, generated at build time
    path: ../platform
    role: client
    generated_from: [apps/thermos/openapi.json]
```

**Add the consumers.** Point `include` at source directories, not the whole repository, to keep tests, scripts and fixtures out. Name the variables code calls its client through (`client_receivers`), and for generated clients imported as functions, the module they come from (`client_modules`).

```yaml
  composio:
    path: ../composio
    commit: main
    role: consumer
    include: [ts/packages/core/src, python/composio]
    client_receivers: [client, _client]

  dashboard:
    path: ../dashboard
    commit: main
    role: consumer
    include: [src]

  apollo:                                 # Apollo calling Thermos
    path: ../platform
    role: consumer
    include: [apps/apollo/src]
    client_receivers: [thermos]
    client_modules: [generated/thermos]
```

**Check coverage.** Before trusting findings, check Lemma sees the calls you know about:

```sh
lemma verify manifest.yaml --relation calls --json | jq -r '.relations.calls[] | "\(.[1])\t\(.[2])"' | sort -u | less
```

Each row is an operation and the file calling it. Missing calls usually mean an `include` directory is missing, a client receiver is not named, or URLs are built at run time from values the code does not show (see [pre-release checklist](pre-release.md)).

**Declare what code does not show.** A service whose code rejects unknown fields without saying so in its spec or zod schemas (Go's `DisallowUnknownFields`, serde's `deny_unknown_fields`) gets `rejects_unknown_fields: true`, so sending a removed field is an error rather than a warning.

The full field list is in [Verifying changes](verify.md#the-manifest).

## 5. Verify a change

Point `change` at the repository and commits to check, and run:

```sh
lemma verify manifest.yaml
lemma verify manifest.yaml --explain     # with the facts behind each finding
```

Commits are read from Git without a checkout, so `base` and `head` can be any revisions: `main` and a branch, a merge commit and its parent (`abc123^`), or two tags. Repositories other than the changed one are read at their `commit`, or their working directory if it is absent.

The changed repository can be a service (what its change breaks for callers) or a consumer (whether the calls it adds exist). Pinning another entry of the same clone to an older `commit` asks how the change meets code from before it, as when a service deploys before its callers.

Findings come in three severities. Exit code 4 means at least one error.

| Severity | Means | Example |
| --- | --- | --- |
| error | A request will fail | A consumer calls a removed operation; sends a field a strict schema now rejects; never sends a newly required field |
| warning | Behaviour changes without failing | A consumer reads a removed response field, or sends a field the service now ignores |
| info | Worth knowing | A consumer calls an operation whose request changed but never names the changed field; a route missing from the spec |

Read a finding by its derivation (`--explain`): each line is a fact or rule, indented under what it supports, down to facts from specs and code. A wrong finding is usually one wrong fact near the bottom: a call matched to the wrong operation, or a common name counted as using a field.

## 6. Calibrate on history

Before gating anything, replay changes your team already made and compare the findings with what happened. For each commit that changed a service's spec:

```sh
git -C ../platform log --format=%h -- apps/apollo/openapi-v3.1.json | head -50
```

set `change` to `<commit>^..<commit>`, pin each consumer's `commit` to the last commit before the change merged (`git rev-list -1 --before=<date> main`), and run `lemma verify`. Check every error and warning by hand against the pull request and any follow-up in the consumers. This is how Lemma was measured on Composio's repositories ([results](verify.md#how-well-it-works)); a sweep of a hundred changes takes a few minutes.

Keep the manifests that found real breaks as regression cases for upgrades of Lemma or your grammar.

## 7. Run in CI

Run `lemma verify` on pull requests to each service, with the consumers at their default branches. A GitHub Actions job, in the service's repository:

```yaml
jobs:
  contracts:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
        with: { path: platform, fetch-depth: 0 }
      - uses: actions/checkout@v4
        with: { repository: your-org/workspace, path: workspace, token: ${{ secrets.READ_TOKEN }} }
      - uses: actions/checkout@v4
        with: { repository: your-org/dashboard, path: dashboard, token: ${{ secrets.READ_TOKEN }} }
      - name: Install lemma
        env: { GH_TOKEN: ${{ secrets.READ_TOKEN }}, LEMMA_VERSION: v0.1.0-rc0 }
        run: |
          gh api -H 'Accept: application/vnd.github.raw' repos/angrybayblade/lemma/contents/install.sh | sh
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - name: Verify the change
        run: |
          lemma verify workspace/lemma/manifest.yaml \
            --base "${{ github.event.pull_request.base.sha }}" \
            --head "${{ github.event.pull_request.head.sha }}"
```

`--base` and `--head` replace the manifest's `change` commits, so one manifest serves every pull request. The service's clone needs the history between them (`fetch-depth: 0`).

Start with the job reporting only (`continue-on-error: true`), read its findings on real pull requests for a few weeks, then let errors fail the check. Warnings should stay advisory in this release: they rest on name evidence that can be wrong for common names.

## When something looks wrong

| Symptom | Likely cause | What to do |
| --- | --- | --- |
| A call you know about is missing | Directory not in `include`, receiver not in `client_receivers`, URL built at run time | `--relation calls`, widen `include`, name the receiver |
| A finding names the wrong operation | Two operations share a path and the call shows no method | Check the call site; the method is taken from the call when written there |
| A warning about a common field name (`id`, `slug`) | The consumer uses the name for its own objects | Ignore it; see the [pre-release checklist](pre-release.md) |
| Every operation looks removed | A spec moved to a new file | List the new file in `specs` |
| `no file ... at` | A spec or table path is wrong at every commit read | Fix the path in the manifest |
| Slow runs | No Soufflé | `brew install souffle` |

More in [Troubleshooting](troubleshooting.md).
