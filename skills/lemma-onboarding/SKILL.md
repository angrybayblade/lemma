---
name: lemma-onboarding
description: Set Lemma up for a team's repositories - install the binary and language servers, index each repository, connect agents over MCP, write the `lemma verify` manifest that maps services, generated clients and consumers, calibrate it on past changes, and add it to CI. Use when asked to install, configure or roll out Lemma, write or fix a Lemma manifest, or add contract checks across repositories.
---

# Onboarding a team to Lemma

Work through these steps in order and confirm each before the next. Ask the user only for what you cannot find in the repositories: which repositories make up the system, and where clones live.

## 1. Install

```sh
command -v lemma && lemma --version
```

If missing, install the newest release:

```sh
curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | LEMMA_SKILLS=1 sh
```

It installs into `~/.local/bin`; make sure that is on `PATH`. Suggest `brew install souffle` (macOS) for faster verification; it is optional.

## 2. Language servers and indexes

Run `lemma providers` and install what the repositories need:

| Language | Install |
| --- | --- |
| TypeScript, JavaScript | `npm install -g typescript typescript-language-server` |
| Python | `uv tool install ty` or `npm install -g pyright` |
| Go | `go install golang.org/x/tools/gopls@latest` |
| Rust | `rustup component add rust-analyzer` |

In each repository: install its dependencies, run `lemma index .`, add `.lemma/` to `.gitignore`, and try `lemma symbols <a known name>` and `lemma refs <it>`.

## 3. Agents

In each repository agents work in: `claude mcp add lemma -- lemma mcp` (Claude Code), or the same command in another MCP client. Add one line to the agent instructions (`CLAUDE.md`, `AGENTS.md`): "Use Lemma's tools for references, definitions, implementations and impact; use text search for strings and one-off lookups."

## 4. Write the manifest

Find the system's parts before writing anything:

1. **Services**: repositories with committed OpenAPI documents. `git ls-files | grep -iE 'openapi|swagger' | grep -E '\.json$'`. Note the route directories (`pages/api`, `routes`, `handlers`, `router`).
2. **Generated clients**: clients generated from a service's spec, committed or built at build time (look for `openapi-ts`, `openapi-generator`, Stainless or similar in build scripts, and gitignored `generated/` directories). Note how they name functions from `operationId`s: as written, camel case, Pascal case or snake case, with any fixed suffix.
3. **Consumers**: code calling the services: request paths, `fetch`, `requests`, `http.NewRequest`, `reqwest`, or calls through a client object (`this.client.x.y()`).

Then write `manifest.yaml` from [manifest-template.yaml](manifest-template.yaml), one repository at a time, running `lemma verify manifest.yaml --all` after each addition. Field meanings and pitfalls are in [manifest-reference.md](manifest-reference.md).

Check coverage before trusting findings:

```sh
lemma verify manifest.yaml --relation calls --json | jq -r '.relations.calls[] | "\(.[1])\t\(.[2])"' | sort -u
```

Pick three or four calls you can see in the code and confirm they appear. If one is missing: is its directory in `include`? Is the client variable in `client_receivers`? Is the URL built at run time (not supported)?

## 4a. Or: composable lemma.yaml files

For a system with calls inference cannot see (function runtimes, queues, RPC), or to compare schemas field by field, describe each repository in its own `lemma.yaml` and compose them from a parent in a workspace repository; check with `lemma check` instead of `lemma verify`. The format and checks are in [Checking links](https://github.com/angrybayblade/lemma/blob/main/docs/check.md).

1. In each repository serving an API, add a service with its `openapi` documents, `routes` and `code`; calls to it are then inferred everywhere.
2. In each repository calling others, add a service with its `code` (or nothing, if it is the repository's only service: the whole repository is read).
3. For each call inference cannot see, declare an operation on both sides with `schema` sources, and a link in the caller's `graph.links`.
4. Write the parent: one `#name` service per repository with `path` or `git` and a pinned `ref`, and `graph.level`.
5. Run the `lemma-audit` skill to find what the files miss, then replay history as in step 5 with `lemma check --change '#repo' --base <commit>^ --head <commit>`.

## 5. Calibrate on history

Replay 20-50 past changes to a service's spec before gating anything:

```sh
git -C <service> log --format='%h %cI %s' -- <spec file> | head -50
```

For each: copy the manifest, set `change` to `<commit>^..<commit>` (or pass `--base <commit>^ --head <commit>`), pin every consumer's `commit` to `git -C <consumer> rev-list -1 --before=<commit date> HEAD`, and run `lemma verify`. For every error and warning, read the pull request and the consumer's history: was it a real break, fixed by a follow-up? Report a table of change, findings, and verdict to the user. False errors are worth a bug report; false warnings on common names are a known limitation.

## 6. CI

Add a job to each service repository that checks out the workspace repository holding the manifest and every consumer, installs Lemma, and runs:

```sh
lemma verify manifest.yaml --base "$BASE_SHA" --head "$HEAD_SHA"
```

The service checkout needs full history (`fetch-depth: 0`). Start with the job not failing the build (`continue-on-error: true`); after a few weeks of correct findings, let exit code 4 (an error finding) fail it. Keep warnings advisory. A complete GitHub Actions example is in [manifest-reference.md](manifest-reference.md#ci).

## Done when

- `lemma refs` answers in every repository.
- Agents have the MCP server and the instruction line.
- The manifest lists every service, client and consumer, and a coverage check finds the calls you sampled.
- At least 20 historical changes were replayed and every error was confirmed real.
- CI runs `lemma verify` on pull requests to each service.

Tell the user about the limitations that matter for their code: field types and enum values are not compared, run-time URLs are not matched, warnings about common field names can be wrong, and only HTTP contracts are checked.
