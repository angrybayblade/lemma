# Commands

```text
lemma [--db <file>] [--providers <file>] [--json] [--no-daemon] <command>
```

## Global options

These work with every command.

| Option | Meaning |
| --- | --- |
| `--db <file>` | Index file to use. By default, `lemma index` writes `<path>/.lemma/index.ls`, and the other commands use the nearest `.lemma/index.ls` in the current directory or a parent. |
| `--providers <file>` | Language server configuration to use instead of the project's `.lemma/providers.toml`. Built-in servers stay available. See [Language servers](providers.md). |
| `--json` | Print JSON instead of text. See [JSON output](json-output.md). |
| `--no-daemon` | Run in this process instead of the background daemon. Slower, but nothing stays running. Setting `LEMMA_NO_DAEMON=1` does the same. |
| `-l`, `--logging <level>` | Log what Lemma is doing to stderr: `error`, `warn`, `info`, `debug` or `trace`. Off by default. `info` shows each step and the choice made in it: whether the command ran in this process or the daemon (and why), which repositories and commits were read, what came from a cache and what was run, and the engines and language servers used, with timings. A command the daemon runs sends its lines back, marked `daemon`. Logs never go to stdout, so `--json` output stays parseable. |

Every command runs in a background daemon that keeps the index open, watches indexed directories for changes, and keeps language servers warm; `lemma` itself only forwards the command. The first command starts the daemon. See [`lemma daemon`](#lemma-daemon).

## `lemma index [path]`

`path` is a directory holding a `mod.lm`, or a module itself. Reads the source files that `path`'s description (default: the current directory's `mod.lm`) says its services are made of, and records their definitions in a snapshot. Re-indexing is incremental: it reads and parses only new or changed files, so running it after every edit is cheap (see [Concepts](concepts.md#re-indexing-is-incremental)).

- Without a `mod.lm`, the command indexes every supported file and warns; exports, links and workspaces need one. With one, each service's `index` selects files: under `include`, not matching `exclude`, in `languages` if given. A file any service selects is indexed. A description where no service names its code, or with no services at all, has every supported file indexed, keeps its exports, and warns; a workspace reports that warning under the member. Changing the selection (adding, editing or removing `mod.lm`) re-indexes from scratch on the next run, dropping files no longer selected, and a removed `mod.lm` takes its exports and links with it.
- The command stores each service's exports with the snapshot: declared operations at their code anchors, operations its OpenAPI documents describe at their route handlers (the function a route's line names, or the function around the route), and the top-level definitions of its public modules (`symbols`). It stores links in a graph of `Link` too, and rebuilds them when the description or the code changes, including an edit to a module the root module imports. The summary counts them, including how many documented operations a handler was found for; routes whose paths are built at run time are not found.
- A `mod.lm` with a `workspace` block composes repositories. Each repository is indexed into its own `.lemma/index.ls`: at its pinned `rev` through a Git worktree in `~/.cache/lemma/worktrees` (the cache keeps the 8 most recently used worktrees, and checks at past commits use the same ones; kept beside its working-directory snapshot, so language servers read the pinned code), or as its working directory. The workspace's own `.lemma/index.ls` then records the members and every member's exports and links, with addresses made absolute (`#platform:services.worker.submit_task`). Symbols stay in the members' indexes; `lemma symbols` run from the workspace's directory searches them all, each path prefixed with its repository.
- A repository's index keeps its working directory's snapshot always, and snapshots of pinned commits while they are among the `--keep` most recently indexed (default 5) and were indexed within `--ttl` (default `14d`; `never` to keep them until `--keep` pushes them out). A dropped snapshot's worktree is removed too.
- Supported files: `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, `.pyi`, `.go`, `.rs`.
- Skipped even when selected: anything matched by `.gitignore`, hidden files and directories (including `.lemma/`), and `node_modules`.
- If no file's contents changed since the last run, the command reports the snapshot as `unchanged` and cached answers stay valid.
- Otherwise the command rewrites only the changed files and drops cached answers for the snapshot.
- Lemma rebuilds an index written by an older version from scratch.

`revision` is the checked-out commit; "uncommitted changes" means supported source files differ from it, including new untracked ones.

The summary mentions files Lemma could only partly read: files with syntax errors (their symbols are still indexed, but some may be missing) and files that are not valid UTF-8 (skipped).

A `mod.lm` that does not load fails `lemma index` with exit 1. Queries index first, and a query whose refresh fails answers from the last index and prints a warning, so a half-edited description does not stop navigation. With `--json`, and `lemma graph --format dot` or `mermaid`, the warning goes to the log instead (`-l warn`). `lemma graph` run from a workspace follows callers across it and turns a failure there into a note on the graph; with `--workspace`, it is an error.

`--no-clone` never clones a repository: see [Repositories that are not cloned](#repositories-that-are-not-cloned).

## `lemma symbols <query>`

Searches indexed symbols by name or qualified name.

| Option | Meaning |
| --- | --- |
| `--exact` | Match the whole name or qualified name instead of any part of it. |
| `--kind <kind>` | Only one kind: `function`, `method`, `class`, `interface`, `struct`, `enum`, `type_alias`, `constant`, `variable`, `field`. |
| `--language <language>` | Only one language: `typescript`, `tsx`, `python`, `go`, `rust`. |
| `--path <text>` | Only symbols whose file path contains this text. |
| `--limit <n>` | At most this many results (default 50). |

Qualified names join enclosing definitions with dots: a method `delete` in class `Router` is `Router.delete`. For Go methods the receiver type is used: `func (s *Server) Start()` is `Server.Start`.

Ids (`#31919f572763121e`) name a definition for as long as it keeps its file, name and kind: editing its file, even moving it within the file, keeps the id. Any unique prefix of six or more digits names it too (`#31919f57`). In a workspace each repository's ids are its own, so ids are written with the repository, `#billing:31919f572763121e`; pass them as shown.

## `lemma outline <file>`

Lists the symbols defined in one file, in source order, indented by nesting. Needs no language server. `<file>` is relative to the current directory and must be inside the indexed directory.

## `lemma refs <symbol>`

Finds references to one symbol by asking a language server for its language.

`<symbol>` is one of:

- a name, such as `delete`;
- a qualified name, such as `Router.delete`;
- an id from `lemma symbols`, such as `'#f256e26e20067a8c'`, or a unique prefix of it, such as `'#f256e26e'`. Quote it, because `#` starts a comment in most shells;
- an export's address from a description, such as `services.worker.submit_task` (a declared operation) or `services.gateway.listProducts` (an `operationId`): the symbol implementing it. An address in another repository (`'#functions:services.serverless.submit_task'`) is resolved from inside a workspace's directory, through its index, in the member's index at its pin. From a workspace's directory, a plain name is looked up in every member: one match is used, and a name several members define is listed with each one's repository (`#platform:apps/...`); narrow it with `--path`.

When the symbol implements an export that a graph of `Link` links to or from, `refs` lists those links after the server's references (`linked by the description`, and `linked` in JSON), resolved to the code on the other end when a workspace index knows it.

Names must match exactly. The same filters as `lemma symbols` narrow a name: `--kind`, `--language`, `--path`.

| Option | Meaning |
| --- | --- |
| `--include-declaration` | Also count the definition itself. |
| `--refresh` | Ask the language server again instead of using a cached answer. |
| `--timeout-secs <n>` | How long to wait for the server to start and to answer (default 60). Large projects can take a while on the first lookup. |

Language servers the daemon keeps running answer lookups, so only the first lookup per language pays the server's start-up cost, and `lemma index` starts them in the background ahead of that.

A TypeScript or Python import statement counts as a reference, since it names the symbol.

Each reference shows its location and, when it sits inside an indexed definition, the name of that definition (`in remove_route`). `refs` marks references outside the indexed directory, such as in installed dependencies, as `(external)`.

### Results

| Result | Meaning |
| --- | --- |
| *N* references | The server found these. |
| no references found | The server answered with nothing. This covers only what it could see and does not prove the symbol is unused. |
| does not support reference lookup | The server answered but cannot find references. |
| changed since it was indexed | The file changed after indexing, so the stored position may be wrong. Queries re-index first, so this only appears with `--db`; run `lemma index` on that index. |
| no provider is installed | No server for this language is on `PATH`. |
| lookup failed | A server started but failed, or none could start. The message says why. |

Lines starting with `skipped` name servers that could not start before one did.

## `lemma definition <file:line:column>`

Finds where the symbol used at a position is defined, such as the function a call goes to. Lines and columns count from 1, as editors show them: `lemma definition src/app.py:12:5`. From a workspace's directory, a file inside a member (`repos/platform/apps/api/handler.go:229:43`) is looked up in that member's index, at its pin when it has one.

Each definition shows its location and, when it is an indexed symbol, its qualified name and id, so you can ask about it next (`lemma refs '#f256e26e'`). It takes `--refresh` and `--timeout-secs` like `refs`, and exits with 3 when no server could answer.

## `lemma implementations <symbol>`

Finds what implements an interface, or an interface or abstract method, and what overrides a method. `<symbol>` and its filters work as for `refs`, and so do `--refresh` and `--timeout-secs`. When a name is ambiguous, only symbols that can have implementations count (interfaces, classes, structs, enums, type aliases and methods), so a field named after its type (`store Store`) does not make `Store` ambiguous. Implementations in the repository are listed before those in libraries.

Lemma indexes interface methods as symbols qualified by their interface (`Shape.area`), so `<symbol>` names them directly. Support varies by server: gopls, rust-analyzer and the TypeScript servers answer; pyright and ty say they do not support it (`does not support implementation lookup`), which is not the same as finding none.

## `lemma signature <symbol>`

Shows a symbol's declaration and what its language server says about it. These come from two places and are kept apart:

- The **declaration** is the definition's header as written, without its body, such as `def delete(self, id: str) -> None:`. It comes from the index and needs no server. It is left out if the file changed since it was indexed.
- The **description** is the server's hover text, usually Markdown: the signature with inferred types, and the documentation comment. It is display text; Lemma does not parse it into types.

It exits with 0 if either part was found, and 3 if neither was.

## `lemma graph <symbol>`

Shows what uses a symbol and what it uses, followed to a depth: as indented trees to read in a terminal, or as Graphviz DOT or Mermaid to draw.

```text
$ lemma graph find_definitions --direction callers --depth 3
find_definitions  (function, lemma-query/src/navigation.rs:38)
  used by
  ├─ Engine.definition  (method, lemma-cli/src/engine.rs:311)
  │  └─ Engine.execute  (method, lemma-cli/src/engine.rs:158)
  │     ├─ run  (function, lemma-cli/src/daemon.rs:253)
  │     └─ run_in_process  (function, lemma-cli/src/main.rs:71)
  └─ ServerNeighbours.defined_at  (method, lemma-query/src/graph.rs:483)
6 nodes, 5 relationships (lsp)
```

| Option | Meaning |
| --- | --- |
| `--direction <way>` | `callers` (what uses it), `callees` (what it uses), or `both` (the default). |
| `--depth <n>` | How many uses away to go. Default: 2. |
| `--source <source>` | Where relationships come from. `lsp` (the default) asks the language server: callers are its references, callees are where its definitions of the names in the body point. Lemma caches answers in the index, so a second run is fast. `names` matches names in the indexed code: instant and needs no server, but a name several symbols share can join unrelated ones, and files outside the snapshot, such as tests left out of `include`, are not read; an empty graph says so. With `lsp`, a caller in a file outside the snapshot is shown as `path (outside the snapshot)`. |
| `--format <format>` | `tree` (the default), `dot`, or `mermaid`. With `--json`, the graph is printed as data: `nodes`, `relationships` (`from` uses `to`, and the `source` it was found by), `notes` and `truncated`. |
| `--max-nodes <n>` | The most nodes to include. Default: 200. |
| `--only <kinds>` | Only symbols of these kinds besides the symbol itself, comma-separated: `--only function,method` shows the calls without the fields, types and constants a body uses. The top level of a file always counts as a caller. |
| `--workspace <mod.lm>` | Follow callers out of the repository: through the operations whose handlers they reach, to the code in other repositories that calls those operations. See below. |
| `--across <calls>` | Across a workspace, how many calls from other code to follow: `1` (the default) stops at the code calling the operations the handlers serve; each more goes on through that code to its own operations and their callers. |
| `--refresh`, `--timeout-secs <n>` | As for `lemma refs`. |

References from a file's top-level code appear as the file; imports are left out. A symbol implementing an export that a graph of `Link` links to another operation uses that operation, which appears by address (`linked operation`). The graph lists questions the server could not answer, such as a stale file or a missing server, as notes. Exits with 2 when the name matches several symbols, and 3 when nothing could be answered.

### Across repositories

Run from a workspace's directory, `lemma graph` follows callers across repositories without `--workspace`. With `--workspace`, the callers found in the symbol's repository are followed on to the routes they handle, using the workspace's `mod.lm` (see [Check links](check.md)): each route's operation becomes a node and the code in other repositories that calls it comes next. There the tree stops; `--across 2` goes on to the operations that code serves and their callers, and so on.

```text
$ lemma graph DispatchTask --direction callers --workspace ../workspace/mod.lm --across 2
DispatchTask  (function, lib/tools/execute.go:50)
  used by
  └─ DispatchTaskHandler.handleTaskDispatch  (method, handlers/tools/execute.go:98)
     └─ DispatchTaskHandler.handle  (method, handlers/tools/execute.go:78)
        └─ NewDispatchTaskHandler  (function, handlers/tools/execute.go:214)
           └─ POST /api/tasks_/{}/execute  (operation, #platform:worker)
              └─ dispatchTask  (caller, #platform:apps/gateway/src/lib/task_dispatch/dispatch_task.ts)
                 └─ POST /api/v3/task_router/session/{}/execute  (operation, #platform:gateway)
                    └─ executeOnce  (caller, #dashboard:src/server/clients/task-router/build-session-tasks.ts)
```

Code reached in other repositories is a `caller`: it was found by the requests it sends, without reading what kind of definition it is. The last line counts relationships by how they were found when there is more than one way: `lsp` or `names` for the symbol's own repository, `route` from an operation to the code serving it, `call` from code to an operation it requests, `name` from one function to another whose name its body mentions (an approximation, as in [Check links](check.md#impact)), `type` when the body names only a method's type (it may call another method of that type; the tree marks these `through its type`, and a use found only by `name` `by name`), and `declared` for links a description declares.

- The index's directory must be inside one of the repositories the workspace composes.
- Within the symbol's repository, callers come from `--source`. From there to a route's handler (a constructor registered for a handler type, say), and within every other repository, they are followed by name: approximate, within one language, skipping names several functions share.
- Operations and their callers come from inference over the whole system, as `lemma check` finds them, so the command runs in its own process and takes about as long. Routes whose paths are built at run time are not found, and the trace stops there.

Render a drawing with Graphviz (`lemma graph X --format dot | dot -Tsvg > graph.svg`) or paste Mermaid into a Markdown file.

## `lemma providers`

Lists every configured language server, whether its command was found, and the order they are tried in for each language. It reads the configuration of the indexed project if there is one, otherwise `.lemma/providers.toml` in the current directory. A server that is not found says how to install it; one installed where `PATH` does not reach (`~/go/bin`, `~/.cargo/bin`, `~/.local/bin`, `~/.bun/bin`, `~/.npm-global/bin`, `/opt/homebrew/bin`, `/usr/local/bin`) says where, and which directory to add to `PATH`. A project's own providers can give `install` too.

## `lemma mcp`

Serves Lemma to agents as an [MCP](https://modelcontextprotocol.io) server over stdin and stdout. Run it from the project directory, or have the agent's client start it there. Tool calls run in the daemon like any other command, so language servers stay warm across tool calls and terminal use.

| Tool | Command it runs | Arguments |
| --- | --- | --- |
| `index` | `lemma index` | `path` (optional) |
| `search_symbols` | `lemma symbols` | `query`, `exact`, `kind`, `language`, `path`, `limit` |
| `outline` | `lemma outline` | `file` |
| `references` | `lemma refs` | `symbol`, `kind`, `language`, `path`, `include_declaration`, `refresh` |
| `definition` | `lemma definition` | `file`, `line`, `column` (from 1), `refresh` |
| `implementations` | `lemma implementations` | `symbol`, `kind`, `language`, `path`, `refresh` |
| `signature` | `lemma signature` | `symbol`, `kind`, `language`, `path`, `refresh` |
| `graph` | `lemma graph` | `symbol`, `kind`, `language`, `path`, `refresh`, `direction`, `depth`, `source`, `max_nodes`, `only`, `workspace`, `across` |
| `check` | `lemma check` | `manifest` (the entry module), `change`, `base`, `head`, `level`, `no_run`, `select`, `all` |
| `verify` | `lemma verify` | `module`, `all`, `links` |

Each tool returns the command's text output, as the terminal shows it: the same facts and ids as its [JSON output](json-output.md), in a fraction of the size. A command that ran but found no answer (an ambiguous name, a stale file, no server) is a normal result that says why, and so is a check that found errors; only a command that failed, such as a missing index, is returned as an error. `check` can run behaviors' checkers, which run code, so it is not marked read-only.

To add it to Claude Code for a project:

```sh
claude mcp add lemma -- lemma mcp
```

Other clients take the same command, `lemma mcp`, started in the project directory.

## `lemma verify [mod.lm]`

Compares a description with the code at one snapshot: patterns that match no file, documents that do not exist, anchors that do not resolve, links that no longer hold. It does not compare commits or report contract disagreements. See [Verifying descriptions](verify.md).

| Option | Meaning |
| --- | --- |
| `--all` | List every informational finding, not just the first ten. |
| `--links` | Also list every link: those declared, then the calls inference found from code to documented operations, by calling repository and the service called, with each group's count and first ten calls (`--all` lists every call). With `--json`, the list is `link_list`: `declared` (`caller -> provider`) and `inferred` (`repository`, `location`, `operation`, `service`). |
| `--relation <name>` | Also print every row of a derived relation, such as `calls`. Repeatable. |

Exits with 0 when no finding is an error, 4 when one is, and 1 on failure.

## `lemma check [mod.lm]`

Checks a change against every repository the entry module composes: declared links (persistence and schema agreement), links inferred from OpenAPI documents and code, and what each problem affects further up. See [Check links](check.md). The entry module's own requirements run beside the HTTP contracts, its behaviors run (with their checkers), and its allowances apply, as its `check main` selects them. See [The `.lm` language](language.md).

| Option | Meaning |
| --- | --- |
| `--change <name>` | The changed repository: its name (`billing`), its directory (`web-app`, for the repository named `web_app`), or `self`. |
| `--base <commit>`, `--head <commit>` | The commits before and with the change. Without `--head`, the working directory, uncommitted edits included. Without `--base`, where the checked-out commit left the repository's default branch (the branch a remote's `HEAD` names, else `main`, `master`, `develop`, `trunk` or `init.defaultBranch` on a remote, `origin` first, then locally): on a feature branch, everything the branch adds; on the default branch, only uncommitted edits. When no such branch shares the commit's history, as in a shallow CI checkout, a merge commit's first parent is the base (CI checks a pull request out as a merge); otherwise a clean checkout is an error naming `--base`, since the change would be empty and pass unchecked. So `lemma check --change billing` checks the work in progress. |
| `--level <level>` | `audit` (problems are warnings), `check` (problems and violated behaviors are errors) or `strict` (error-severity behaviors that are unknown or stale are errors too, and so is code an adapter or schema resolver could not read, whole or in part: in CI, a job without the dependencies an adapter or resolver needs fails instead of checking committed documents only; below `strict` that is a warning, `[adapter]` or `[resolver]`). Overrides the workspace's `level`. |
| `--all` | List informational findings too, not just their count. |
| `--select <name>` | Run this check of a parent `mod.lm` instead of its `check main`; or, when no check has the name, just the behavior or requirement it names, by address (`checks::retries`, `lemma.http.operation_without_consumer`) or by name alone when only one thing has it. A behavior that takes arguments runs only from a check. From a description with no program, it names one requirement of the built-in HTTP checks and lists only its findings. |
| `--since <token>` | With the daemon's sentry attached to the workspace: list only the findings that came and went since the answer the token names (the `sentry token:` line under an earlier answer with the same options), then the counts. A token sentry no longer keeps gets the whole report, saying so. |
| `--no-run` | Run no checkers: behaviors use the evidence already kept in Lemma's cache. A case whose inputs changed since is `stale`; one never run is `unknown`. |
| `--run-base` | With `--change`, also run checkers at the base, so a case that ended violated before the change and holds after it is reported as fixed: the behavior's line ends with `N fixed by this change`, and each fixed case follows with how it ended at the base, its operands' values there included. Without it, the base uses kept evidence only and a fix is reported only when that evidence was kept. Doubles the checkers' runs when nothing is kept. |
| `--engine <name>` | The rule engine for inferred links: `souffle` or `reference`. Default: Soufflé if installed. |
| `--evaluation <how>` | `demand` (the default) reads only the consumer code inferred findings can rest on, asking for more callers as traces reach further operations; `full` reads everything; `both` runs the two and fails if their findings differ. |
| `--explain` | Show why each inferred finding holds, down to the facts it rests on. |
| `--no-clone` | Never clone a repository: check those that are here, and exit 5 if that is not all of them. |

Prints each behavior as `behavior <identity>: <outcome> (<held> of <cases> cases held, <reused> of <n> evidence reused)`, evidence reused being what checkers kept from an earlier run with the same inputs, and each case that did not hold is a finding naming the expression, its file and line, each operand's value and the `because` text. A finding an allowance covers ends with `(allowed: <reason>)` and is informational. With `--json`, `behaviors` lists every behavior with its cases (`identity`, `outcome`, `problem`, `introduced`, `fixed`, `at_base` for a fixed case, `allowed`) and the evidence each used (`checker`, `case`, `key`, `reused`, `unavailable`); `requirement_changes` lists what a change added, removed or changed; findings carry `allowed`.

An operation a link still names that the change removed is a `link` finding (introduced, exit 4), and the other repositories are still checked. The command reports a change that deletes a document a service exports, or leaves an `include` matching nothing (`document_missing`, `include_matches_nothing`).

Exits with 0 when no finding is an error, 4 when one is, 5 when it finished but a `git(...)` repository was not cloned, and 1 on failure.

## `lemma daemon`

Runs commands for `lemma` with warm state: open databases, watchers on indexed directories, running language servers, and symbol names in memory. Every command starts it automatically; `lemma daemon` on its own runs it in the foreground. Language servers start when a lookup needs one, when `lemma index` runs (in the background), or with `lemma daemon lsp start`; there is one per indexed root and language, and the settings below pause and stop idle ones.

| Command | Meaning |
| --- | --- |
| `lemma daemon status` | Whether it runs as a login service; its language servers: starting, running or paused, how long idle, and memory; and for each watched directory, how many directories it watches, or why every index walks it instead (such as the limit on watches). On Linux only the directories Lemma indexes are watched, one by one, not `node_modules`, hidden or Git-ignored ones. |
| `lemma daemon stop` | Stop the running daemon; it returns once the daemon and its language servers have stopped. |
| `lemma daemon install` | Run the daemon as a login service (launchd on macOS, systemd on Linux), so it is already up when an agent asks its first question. It is restarted only if it crashes, and a command that finds it stopped starts it through the service. The service keeps the `PATH` of the shell that installed it; run `install` again after installing a toolchain elsewhere. |
| `lemma daemon uninstall` | Remove the login service and stop its daemon. |
| `lemma daemon lsp [path]` | The language servers for the code indexed at `path` (a repository, or a workspace's members; the current directory by default): for each language, the server it uses and what it is doing (`stopped`, `starting`, `running`, `paused`, `failed` and why, or `not installed` and how to install one), then servers running elsewhere. `--json` prints the rows. |
| `lemma daemon lsp start [path] [--language <l>] [--provider <id>] [--wait]` | Start those servers in the daemon, in the background, or with `--wait` until each has loaded its project, reporting how long it took or why it failed (exit 1). A language's providers are tried in order, as a lookup tries them: when the first cannot start (typescript-language-server without a TypeScript it can use), the next serves. |
| `lemma daemon lsp stop [path] [--language <l>] [--provider <id>]` | Stop those servers. |
| `lemma daemon sentry start [manifest]` | Attach the daemon's sentry to a workspace (its `mod.lm`, by default the one in this directory). Sentry watches the workspace's repositories, and `lemma check` on that workspace is then answered by the daemon: what earlier checks read is kept and only what changed is read again, and a check is answered again at once when nothing it read changed. The report is the same as a check run on its own. It attaches only to a daemon already running, to one workspace at a time. |
| `lemma daemon sentry stop` | Detach sentry. It also detaches when the workspace's directory is gone and when the daemon exits. |
| `lemma daemon sentry status` | The workspace, the directories watched in each repository, and how many checks sentry answered and reused. |
| `lemma daemon sentry events [--since <token>] [--wait <duration>]` | What happened since a token, a line each: repositories whose files changed, and checks answered (reused or checked, how long, and their counts), then the latest token. With `--wait`, it waits that long for an event when there is none yet, for a hook or a script to follow sentry. `--json` prints the events. |
| `lemma daemon --idle-secs <n>` | Run in the foreground, exiting after this many seconds without a command instead of the configured `idle_exit`. |

Sentry is experimental and off unless you start it: its answers match a fresh check, but it can change or go away between releases.

On Linux, `install` uses a systemd user unit. On a server you reach over SSH there may be no user session to run it in; `sudo loginctl enable-linger $USER` gives you one that starts at boot and outlives your logins.

The daemon serves every later command, so it does not keep the environment of the command that happened to start it. Without a service, it keeps only `HOME`, `USER`, `LOGNAME`, `PATH`, `SHELL`, the locale (`LANG`, `LANGUAGE`, `LC_*`), `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, `SSH_AUTH_SOCK`, the proxy variables and `LEMMA_*`. A shell's `GOCACHE`, `XDG_CACHE_HOME` or `TMPDIR` would otherwise redirect every later build and cache, and break them once that shell's directories are gone. Adapters and checkers that need a variable get it from their `env` settings. Set `LEMMA_NO_DAEMON=1` to run a command in its own process, with its own environment.

### Settings

The daemon reads `~/.config/lemma/config.toml` (or `$XDG_CONFIG_HOME/lemma/config.toml`, or the file named by `LEMMA_CONFIG`), and reads it again whenever it changes. Every setting is optional:

```toml
[daemon]
idle_exit = "never"        # stop the daemon after this long without a command

[servers]
pause_after = "10m"        # pause idle language servers
stop_after = "2h"          # stop idle language servers
memory_budget = "none"     # e.g. "2GB": stop the least recently used servers beyond this
warm_on_activity = true    # start a project's servers again when its files change

[sentry]
auto_attach = false        # attach sentry to a workspace on its first `lemma check`

[runtime]
memory = "512MB"           # what warm runtimes may hold: "0" keeps none, "none" sets no limit
stop_after = "30m"         # stop an idle warm runtime
```

**Warm runtimes.** While the daemon runs, `lemma check` asks it to run a repository's Python (a `json.command("{python} -m …")` or `{python} script.py` resolver, or a type read with Pydantic) in a process that has already imported the repository's dependencies, so a run pays only for the repository's own modules. It prints what a new interpreter would, and anything that goes wrong runs the command as before. `memory` is the trade: the more a runtime may hold, the less a run waits. On one service with about 1,700 dependency modules, a 1.6 s resolver ran in 1.2 s with `"24MB"`, 0.7 s with `"96MB"` and 0.2 s once it could hold all 140 MB. A project's `.lemma/config.toml` may set `[runtime]` too, and overrides this file setting by setting. `LEMMA_RUNTIME=off` keeps none warm. `lemma daemon status` lists each one with the memory it holds. A check never starts the daemon for this.

Under each report it answers, sentry adds a line `sentry token: <token>`, and JSON output a `sentry` object; the findings and the exit code are the same as a check run on its own. A token stays the same while nothing a check reads changes. Sentry answers a check only from a daemon already running; `lemma check` never starts one for it. A check that asks for logs (`-l`) or for anchors (`--anchors`) runs in its own process, and `LEMMA_SENTRY=off` keeps every check there. A check sentry answers runs resolvers, adapters and checkers with the environment of the process that asked, as a check run on its own does. `auto_attach` applies only in the user's own settings, and never when `CI` is set. `LEMMA_DAEMON_SOCKET` names another socket for the daemon, to run a separate one.

Durations take `s`, `m`, `h` or `d`, or `never`; sizes take `MB` or `GB`, or `none`. The values above are the defaults. The daemon rejects unknown settings, so a typo shows up instead of being ignored; a file that stops parsing while the daemon runs leaves the previous settings in force.

The daemon pauses a server, and every process it started, with `SIGSTOP`: it uses no CPU, and the operating system can compress or swap out its memory under pressure. The next lookup resumes it, which took about 45 ms for gopls in testing, against seconds to start a server again.

There is one daemon per user. It talks to `lemma` over a Unix socket in the temporary directory that only your user can open, and finds language servers on the `PATH` of each command it runs. A daemon started by a different build of Lemma is replaced automatically. The daemon is not available on Windows; commands there always run in-process.

### Repositories that are not cloned

`lemma index` and `lemma check` need the repositories a description names. A `local(...)` repository that is missing is an error. A `git(...)` repository that is not cloned follows the `clone` setting, read from `.lemma/config.toml` beside the description and then from the file above:

```toml
[repositories]
clone = "ask"              # "ask", "always" or "never"
```

`ask`, the default, asks in an interactive terminal and clones otherwise. `never`, or `--no-clone` on the command, or answering no, stops at the repositories that are here: `lemma index` warns, and `lemma check` lists what it could not reach and exits `5`. Queries and MCP tools never clone.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. For `refs`, the server answered, including "no references found". |
| `1` | Error, such as no index, an unknown symbol, or an invalid configuration file. The message is printed to stderr. |
| `2` | `refs`, `implementations`, `signature` and `graph`: the name matches several symbols. The candidates are printed. |
| `3` | `refs`, `definition`, `implementations`, `signature` and `graph`: no answer. The file is stale, no server is installed, the server failed, or it does not support the question. |
| `4` | `verify` and `check` only: a finding is an error. |
| `5` | `check` only: it finished, but a `git(...)` repository was not cloned, so what reaches it was not checked. A finding that is an error exits `4` instead. |

## `lm`

`.lm` modules have their own command, `lm`, installed beside `lemma`. It checks and formats modules and serves them to editors; `lemma check` and `lemma verify` use what they declare. Every `lm` command runs in its own process, without Lemma's daemon or index.

```text
lm [--json] [-l <level>] <command>
```

See [The `.lm` language](language.md).

| Command | Meaning |
| --- | --- |
| `lm check [mod.lm]` | Loads a module and everything it imports, and prints every problem as `file:line:column: severity: message`, then a summary. With `--json`, `diagnostics` lists each with its `module`, `path`, `line` and `column`. Exits with 1 if any is an error. |
| `lm fmt [paths...]` | Rewrites `.lm` files in the canonical layout ([spec 13, Layout](../.specs/13-lm-grammar.md#layout)): four-space indentation by brackets and continued lines, one space around operators and after `,` and `:`, at most one blank line in a row. Line breaks and comments are kept. Each path is a file or a directory to search; with none, the current directory is searched, skipping hidden directories, `target` and `node_modules`. A file with syntax errors is left alone and its errors are printed; a fragment without a `module` line is formatted. `--check` lists the files that are not formatted instead of rewriting them. Exits with 1 if a file has syntax errors or, with `--check`, is not formatted. |
| `lm new behavior <name> --handler <operation> [--through <link>] [--module mod.lm]` | Beta. Adds a checker named `<name>_run` and a behavior named `<name>` to the module, and writes the checker's script under the module's `tools/`. `--handler` is one of the module's operations with an `implementation`, as the module names it (`orders.place_order`); `--through` is a link a graph of the module holds (`"orders.charge -> api.createCharge"`), whose call the script replaces. The script follows the handler's language: Python runs the handler in place with the call replaced, with the service's `runtime.python` when it declares one; TypeScript runs it with Bun; Go gets a script saying which test to write. What a case sends and what the replaced call answers are marked `TODO`, and the behavior starts at `severity: warning`. Refuses a name already declared, a script that exists, and a module with errors, and leaves the module as it was if the result does not check. |
| `lm lsp` | Serves `.lm` modules to an editor over the Language Server Protocol, on standard input and output. |
