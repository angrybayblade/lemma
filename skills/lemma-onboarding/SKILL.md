---
name: lemma-onboarding
description: Set Lemma up for a team's repositories - install the binary and language servers, index each repository, connect agents over MCP, write the `mod.lm` descriptions that map services, generated clients and calling code, calibrate them on past changes, and add `lemma check` to CI. Use when asked to install, configure or roll out Lemma, write or fix a `mod.lm`, or add contract checks across repositories.
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

It installs `lemma` and `lm` into `~/.local/bin`; make sure that is on `PATH`. Suggest `brew install souffle` (macOS) for faster checks; it is optional.

## 2. Language servers and indexes

Run `lemma providers` and install what the repositories need:

| Language | Install |
| --- | --- |
| TypeScript, JavaScript | `npm install -g typescript typescript-language-server` |
| Python | `uv tool install ty` or `npm install -g pyright` |
| Go | `go install golang.org/x/tools/gopls@latest` |
| Rust | `rustup component add rust-analyzer` |

In each repository: install its dependencies, write a `mod.lm` with a service per part of the repository and the code each is made of (`index: { include: [...] }`), run `lemma index .`, add `.lemma/` to `.gitignore`, and try `lemma symbols <a known name>` and `lemma refs <it>`. A repository's name is its root module's, an identifier: the directory `web-app` holds `module web_app`.

## 3. Agents

In each repository agents work in: `claude mcp add lemma -- lemma mcp` (Claude Code), or the same command in another MCP client. Add one line to the agent instructions (`CLAUDE.md`, `AGENTS.md`): "Use Lemma's tools for references, definitions, implementations and impact; use text search for strings and one-off lookups."

## 4. Write the description

Find the system's parts before writing anything:

1. **Services**: repositories with committed OpenAPI documents. `git ls-files | grep -iE 'openapi|swagger' | grep -E '\.(json|ya?ml)$'`. Note the route directories (`pages/api`, `routes`, `handlers`, `router`).
2. **Generated clients**: clients generated from a service's spec, committed or built at build time (look for `openapi-ts`, `openapi-generator`, Stainless or similar in build scripts, and gitignored `generated/` directories). Note how they name functions from `operationId`s: as written, camel case, Pascal case or snake case, with any fixed suffix.
3. **Calling code**: code calling the services: request paths, `fetch`, `requests`, `http.NewRequest`, `reqwest`, or calls through a client object (`this.client.x.y()`).

Then write them, one repository at a time, running `lm check` after each addition. `lm` is the `.lm` command installed beside `lemma`; it reports mistakes (a link to an operation that does not exist, an unknown field) with their line, and editors get the same from `lm lsp`. Field meanings are in [the `.lm` reference](../lemma-audit/reference.md).

```lm
module billing

import * as http from lemma.http

export service api {
    index: { include: ["src"] }                  // its code: routes, and calls it makes
    operations: http.openapi("openapi.json")     // calls to these are inferred everywhere
}
```

- In each repository serving an API, add a service with what it is made of and its OpenAPI documents; calls to it are then inferred everywhere. A service reading its operations from code uses an adapter: `hono.app(...)`, `fastapi.app(...)` or `http.adapter(...)`.
- In each repository calling others, add a service with its `index`.
- A generated client is a service with `client: http.generated_client(generated_from: [...], call_names: [...])`.
- For a call inference cannot see (function runtimes, queues, RPC), declare an operation on each side (`export operation`, and `export client operation` on the caller) with `implementation` and schemas, and a link in the caller's `export graph links : Link { ... }`.
- Write the workspace, in a repository of its own, with a `repo` per repository and a pin for each `git(...)`:

  ```lm
  module system

  workspace {
      repo billing = local("../billing")
      repo web = local("../web")
      config: { level: check }
  }
  ```

Check it against the code, and fix every finding:

```sh
lemma verify
```

This is what makes a description done: it finds an `include` matching nothing, a document that does not exist, an `implementation` that does not resolve, and the rest. It compares what the description says with the code; it does not look for what the description leaves out. Run the `lemma-audit` skill for that.

Check coverage before trusting findings:

```sh
lemma verify --relation calls --json | jq -r '.relations.calls[] | "\(.[1])\t\(.[2])"' | sort -u
```

Pick three or four calls you can see in the code and confirm they appear. If one is missing: is its directory in `include`? Do `call_names` match the generator? Is the URL built at run time (not supported)?

## 5. Calibrate on history

Replay 20-50 past changes to a service's spec before gating anything:

```sh
git -C <service> log --format='%h %cI %s' -- <spec file> | head -50
```

For each: pin every other repository in the workspace to `local("../<repo>", rev: "<commit>")`, the commit given by `git -C <repo> rev-list -1 --before=<commit date> HEAD`, and run `lemma check --change <repo> --base <commit>^ --head <commit>`. For every error and warning, read the pull request and the calling code's history: was it a real break, fixed by a follow-up? Report a table of change, findings, and whether each was real to the user. False errors are worth a bug report; false warnings on common names are a known limitation.

## 6. CI

Add a job to each service repository that checks out the workspace repository and every other repository where the workspace expects them, installs Lemma, and runs:

```sh
lemma check workspace/mod.lm --change <repo> --base "$BASE_SHA" --head "$HEAD_SHA"
```

The service checkout needs full history (`fetch-depth: 0`). Start with the job not failing the build (`continue-on-error: true`); after a few weeks of correct findings, let exit code 4 (an error finding) fail it. Keep warnings advisory. Run `lemma verify` when a pull request touches a `.lm` file and on a schedule. A complete GitHub Actions example is in [Onboarding](https://github.com/angrybayblade/lemma/blob/main/docs/onboarding.md#7-run-in-ci).

## Done when

- `lemma refs` answers in every repository.
- Agents have the MCP server and the instruction line.
- `lemma verify` is clean, and a coverage check finds the calls you sampled.
- At least 20 historical changes were replayed and every error was confirmed real.
- CI runs `lemma check` on pull requests to each service.

Tell the user about the limitations that matter for their code: field types and enum values are not compared, run-time URLs are not matched, warnings about common field names can be wrong, and only HTTP contracts are inferred.
