# Manifest reference

## Top level

| Field | Required | Meaning |
| --- | --- | --- |
| `change` | yes | `{ repository, base, head }`: the repository that changed and the commits before and with the change. Any Git revision works: branches, `abc123^`, tags. `--base` and `--head` override the commits. |
| `repositories` | yes | Every repository, by name. |
| `grammar` | no | A grammar file, relative to the manifest. The built-in HTTP contracts grammar if absent. |

## Repository fields

| Field | Roles | Meaning |
| --- | --- | --- |
| `path` | all | The clone, relative to the manifest. Files are read from Git, without a checkout. |
| `role` | all | `service`, `client` or `consumer`. |
| `commit` | all | The commit to read for repositories other than the changed one; the working directory if absent. On an entry sharing the changed repository's clone, pins it to that commit instead of the change's. |
| `service` | service, consumer | On a service: the name used in findings. On a consumer: the service its code belongs to; its references to that service's own operations (route declarations) are not calls. |
| `specs` | service, client | OpenAPI JSON documents: a service's contract, or a client's copy of it. |
| `routes` | service | Directories with route code. Checks the spec against the code and reads which request schemas are zod `.strict()`. |
| `schemas` | service | Further directories with schemas the routes import. |
| `route_aliases` | service | Map of path prefixes to the prefixes routes are also served under, such as `{ /v1/: /v2/ }`. Rarely needed: inferred when one spec's operations are routed under another spec's prefix. |
| `rejects_unknown_fields` | service | `true` if requests with unknown fields fail even where spec and zod do not say so (Go `DisallowUnknownFields`, serde `deny_unknown_fields`). |
| `generated_from` | client | Specs a client is generated from; one function per operation. |
| `call_names` | client | How the client names an operation's function from its `operationId`: templates with `{operationId}`, `{camel}`, `{pascal}`, `{snake}`, such as `["{camel}"]` or `["{pascal}.Invoke"]`. The `operationId` as written if absent. |
| `path_prefixes` | service | Path prefixes the service owns; a changed consumer's new call to an unserved path under one is reported. Derived from each spec if absent (the prefix its paths share, narrowed where another service shares it); rarely needed. |
| `include` | consumer | Directories with the consumer's code; the whole repository if absent. |
| `client_receivers` | consumer | Variable names the code calls its client through: `client`, `_client`, `billing`. |
| `client_modules` | consumer | Module specifiers (suffixes) of generated clients; functions imported from them are client calls. |

## What Lemma reads

| Language | Routes | Calls | Field names |
| --- | --- | --- | --- |
| TypeScript, JavaScript | objects with `method` and `path` (`createRoute`), zod `.strict()` | path strings, `fetch(url, { method })`, typed fetch `client.POST("/x")`, calls through clients | object keys, members, destructuring, subscripts |
| Python | — | path strings, `requests.post(url)`, calls through clients | keyword arguments, attributes, dictionary keys |
| Go | `net/http` (`"GET /x"` patterns), gorilla/mux with subrouters, chi, gin, echo | `http.NewRequest`, `http.Get`, URLs from `fmt.Sprintf` | JSON struct tags, string map keys |
| Rust | axum `.route()`, actix-web and Rocket `#[get("/x")]` | reqwest `client.post(url)`, `request(Method::POST, url)`, `format!` URLs | struct fields, serde renames, `json!` keys |

A string counts as a path only if it contains a leading segment some service in the manifest serves (such as `/v1/`); paths beside a method (route declarations) and in specs only need to start with `/`. Path parameters (`{id}`, `${id}`, `:id`, `[id]`, `%s`) compare equal. Imports through `tsconfig.json` `paths` are followed.

## Pitfalls

| Symptom | Cause | Fix |
| --- | --- | --- |
| `has no file X at A or B` | A spec or table path is wrong at every commit read | Fix the path; check `git -C <repo> ls-tree -r <commit> --name-only` |
| A whole spec's operations look removed | The spec moved | List the new file too |
| Consumers' calls are missing | `include` too narrow, receiver not named, URLs built at run time | Widen `include`, add `client_receivers` or `client_modules` |
| Many `route_not_in_spec` notes | Internal routes, or routes published under another prefix | Informational; add `route_aliases` if routes are published under another prefix Lemma could not infer |
| Removed fields only ever warn | The service's strictness is not visible | `rejects_unknown_fields: true` if it rejects unknown fields |

## CI

GitHub Actions job for a service repository, with the manifest in a workspace repository and consumers checked out beside it (paths in the manifest must match):

```yaml
name: contracts
on: pull_request
jobs:
  verify:
    runs-on: ubuntu-22.04
    continue-on-error: true            # remove once findings are trusted
    steps:
      - uses: actions/checkout@v4
        with: { path: billing, fetch-depth: 0 }
      - uses: actions/checkout@v4
        with: { repository: your-org/workspace, path: workspace, token: ${{ secrets.READ_TOKEN }} }
      - uses: actions/checkout@v4
        with: { repository: your-org/web, path: web, token: ${{ secrets.READ_TOKEN }} }
      - name: Install lemma
        env: { LEMMA_VERSION: v0.1.0-rc0 }
        run: |
          curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - name: Verify the change
        run: |
          lemma verify workspace/lemma/manifest.yaml \
            --base "${{ github.event.pull_request.base.sha }}" \
            --head "${{ github.event.pull_request.head.sha }}"
```
