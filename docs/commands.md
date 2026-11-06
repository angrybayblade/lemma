# Commands

```text
lemma [--db <file>] [--providers <file>] [--json] [--no-daemon] <command>
```

## Global options

These work with every command.

| Option | Meaning |
| --- | --- |
| `--db <file>` | Index database to use. By default, `lemma index` writes `<path>/.lemma/index.db`, and the other commands use the nearest `.lemma/index.db` in the current directory or a parent. |
| `--providers <file>` | Language server configuration to use instead of the project's `.lemma/providers.toml`. Built-in servers stay available. See [Language servers](providers.md). |
| `--json` | Print JSON instead of text. See [JSON output](json-output.md). |
| `--no-daemon` | Run in this process instead of the background daemon. Slower, but nothing stays running. Setting `LEMMA_NO_DAEMON=1` does the same. |
| `-l`, `--logging <level>` | Log what Lemma is doing to stderr: `error`, `warn`, `info`, `debug` or `trace`. Off by default. `info` shows each step and the choice made in it: whether the command ran in this process or the daemon (and why), which repositories and commits were read, what came from a cache and what was run, and the engines and language servers used, with timings. A command the daemon runs sends its lines back, marked `daemon`. Logs never go to stdout, so `--json` output stays parseable. |

Every command runs in a background daemon that keeps the index open, watches indexed directories for changes, and keeps language servers warm; `lemma` itself only forwards the command. The first command starts the daemon. See [`lemma daemon`](#lemma-daemon).

## `lemma index [path]`

`path` is a directory holding a `lemma.yaml`, or a `lemma.yaml` itself.

Reads the source files that `path`'s `lemma.yaml` (default: the current directory's) says its services are made of, and records their definitions in a snapshot. Re-indexing is incremental: only new or changed files are read and parsed, so running it after every edit is cheap (see [How Lemma works](concepts.md#re-indexing-is-incremental)).

- Without a `lemma.yaml`, every supported file is indexed, with a warning; exports, links and workspaces need one. With one, each local service's `index` section selects files: under `include`, not matching `exclude`, in `languages` if given. A file any service selects is indexed. A service without `index.include` adds nothing; if no service has one, indexing fails. Changing the selection (adding, editing or removing `lemma.yaml`) re-indexes from scratch on the next run, dropping files no longer selected, and a removed `lemma.yaml` takes its exports and links with it.
- Each service's exports are stored with the snapshot: declared operations at their code anchors, operations its OpenAPI documents describe at their route handlers (the function a route's line names, or the function around the route), and the top-level definitions of its public modules. Links in `graph.links` are stored too. They are rebuilt when `lemma.yaml` or the code changes. The summary counts them, including how many documented operations a handler was found for; routes whose paths are built at run time are not found.
- A `lemma.yaml` with no services of its own, only repositories it composes (`#name`), is a workspace. Each repository is indexed into its own `.lemma/index.db`: at its pinned `ref` through a Git worktree in `~/.cache/lemma/worktrees` (kept beside its working-directory snapshot, so language servers read the pinned code), or as its working directory. A repository whose `lemma.yaml` selects nothing is skipped and says why. The workspace's own `.lemma/index.db` then records the members and every member's exports and links, with addresses made absolute (`#platform:services.worker.submit_task`). Symbols stay in the members' indexes; `lemma symbols` run from the workspace's directory searches them all, each path prefixed with its repository.
- A repository's index keeps its working directory's snapshot always, and snapshots of pinned commits while they are among the `--keep` most recently indexed (default 5) and were indexed within `--ttl` (default `14d`; `never` to keep them until `--keep` pushes them out). A dropped snapshot's worktree is removed too.
- Supported files: `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, `.pyi`, `.go`, `.rs`.
- Skipped even when selected: anything matched by `.gitignore`, hidden files and directories (including `.lemma/`), and `node_modules`.
- If no file's contents changed since the last run, the snapshot is reported as `unchanged` and cached answers stay valid.
- Otherwise only the changed files are rewritten, and cached answers for the snapshot are dropped.
- An index written by an older version of Lemma is rebuilt from scratch.

`revision` is the checked-out commit; "uncommitted changes" means supported source files differ from it, including new untracked ones.

The summary mentions files Lemma could only partly read: files with syntax errors (their symbols are still indexed, but some may be missing) and files that are not valid UTF-8 (skipped).

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

Ids (`#412`) name a definition for as long as it keeps its name and kind: editing its file, even moving it, keeps the id. In a workspace each repository numbers its own symbols, so ids are written with the repository, `#billing:412`; pass them as shown.

## `lemma outline <file>`

Lists the symbols defined in one file, in source order, indented by nesting. Needs no language server. `<file>` is relative to the current directory and must be inside the indexed directory.

## `lemma refs <symbol>`

Finds references to one symbol by asking a language server for its language.

`<symbol>` can be:

- a name, such as `delete`;
- a qualified name, such as `Router.delete`;
- an id from `lemma symbols`, such as `'#415'`. Quote it, because `#` starts a comment in most shells;
- an export's address from `lemma.yaml`, such as `services.worker.submit_task` (a declared operation) or `services.gateway.listProducts` (an `operationId`): the symbol implementing it. An address in another repository (`'#functions:services.serverless.submit_task'`) is resolved from inside a workspace's directory, through its index, in the member's index at its pin. From a workspace's directory, a plain name is looked up in every member: one match is used, and a name several members define is listed with each one's repository (`#platform:apps/...`); narrow it with `--path`.

When the symbol implements an export that `graph.links` links to or from, `refs` lists those links after the server's references (`linked by lemma.yaml`, and `linked` in JSON), resolved to the code on the other end when a workspace index knows it.

Names must match exactly. The same filters as `lemma symbols` narrow a name: `--kind`, `--language`, `--path`.

| Option | Meaning |
| --- | --- |
| `--include-declaration` | Also count the definition itself. |
| `--refresh` | Ask the language server again instead of using a cached answer. |
| `--timeout-secs <n>` | How long to wait for the server to start and to answer (default 60). Large projects can take a while on the first lookup. |

Lookups are answered by language servers the daemon keeps running, so only the first lookup per language pays the server's start-up cost, and `lemma index` starts them in the background ahead of that.

A TypeScript or Python import statement counts as a reference, since it names the symbol.

Each reference shows its location and, when it sits inside an indexed definition, the name of that definition (`in remove_route`). References outside the indexed directory, such as in installed dependencies, are marked `(external)`.

### Results

| Result | Meaning |
| --- | --- |
| *N* references | The server found these. |
| no references found | The server answered with nothing. This only covers what it could see, and does not prove the symbol is unused. |
| does not support reference lookup | The server answered but cannot find references. |
| changed since it was indexed | The file changed after indexing, so the stored position may be wrong. Queries re-index first, so this only appears with `--db`; run `lemma index` on that index. |
| no provider is installed | No server for this language is on `PATH`. |
| lookup failed | A server started but failed, or none could start. The message says why. |

Lines starting with `skipped` name servers that could not start before one did.

## `lemma definition <file:line:column>`

Finds where the symbol used at a position is defined, such as the function a call goes to. Lines and columns count from 1, as editors show them: `lemma definition src/app.py:12:5`.

Each definition shows its location and, when it is an indexed symbol, its qualified name and id, so you can ask about it next (`lemma refs '#415'`). It takes `--refresh` and `--timeout-secs` like `refs`, and exits with 3 when no server could answer.

## `lemma implementations <symbol>`

Finds what implements an interface, or an interface or abstract method, and what overrides a method. `<symbol>` and its filters work as for `refs`, and so do `--refresh` and `--timeout-secs`.

Interface methods are indexed as symbols qualified by their interface (`Shape.area`), so you can ask about them directly. Support varies by server: gopls, rust-analyzer and the TypeScript servers answer; pyright and ty say they do not support it (`does not support implementation lookup`), which is not the same as finding none.

## `lemma signature <symbol>`

Shows a symbol's declaration and what its language server says about it. These come from two places and are kept apart:

- The **declaration** is the definition's header as written, without its body, such as `def delete(self, id: str) -> None:`. It comes from the index and needs no server. It is left out if the file changed since it was indexed.
- The **description** is the server's hover text, usually Markdown: the signature with inferred types, and the documentation comment. It is display text and is not parsed into types.

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
| `--source <source>` | Where relationships come from. `lsp` (the default) asks the language server: callers are its references, callees are where its definitions of the names in the body point. Answers are cached in the index, so a second run is fast. `names` matches names in the indexed code: instant and needs no server, but a name several symbols share can join unrelated ones. |
| `--format <format>` | `tree` (the default), `dot`, or `mermaid`. With `--json`, the graph is printed as data: `nodes`, `relationships` (`from` uses `to`, and the `source` it was found by), `notes` and `truncated`. |
| `--max-nodes <n>` | The most nodes to include. Default: 200. |
| `--workspace <lemma.yaml>` | Follow callers out of the repository: through the operations whose handlers they reach, to the code in other repositories that calls those operations, and on up. See below. |
| `--refresh`, `--timeout-secs <n>` | As for `lemma refs`. |

References from a file's top-level code appear as the file; imports are left out. A symbol implementing an export that `graph.links` links to another operation uses that operation, which appears by address (`linked operation`). Questions the server could not answer, such as a stale file or a missing server, are listed as notes. Exits with 2 when the name matches several symbols, and 3 when nothing could be answered.

### Across repositories

Run from a workspace's directory, a graph does this without being asked. With `--workspace`, the callers found in the symbol's repository are followed on to the routes they handle, using the workspace `lemma.yaml` (see [Checking links](check.md)): each route's operation becomes a node, the code in other repositories that calls it comes next, then the operations that code serves, and so on, up to four operations away.

```text
$ lemma graph DispatchTask --direction callers --workspace ../workspace/lemma.yaml
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

Code reached in other repositories is a `caller`: it was found by the requests it sends, without reading what kind of definition it is. The last line counts relationships by how they were found when there is more than one way: `lsp` or `names` for the symbol's own repository, `route` from an operation to the code serving it, `call` from code to an operation it requests, `name` from one function to another whose name its body mentions (an approximation, as in [Checking links](check.md#what-a-problem-affects)), `type` when the body names only a method's type (it may call another method of that type; the tree marks these `through its type`), and `declared` for links a `lemma.yaml` declares.

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

Each tool returns the command's [JSON output](json-output.md) as text. A command that ran but found no answer (an ambiguous name, a stale file, no server) is a normal result whose JSON says why; only a command that failed, such as a missing index, is returned as an error.

To add it to Claude Code for a project:

```sh
claude mcp add lemma -- lemma mcp
```

Other clients take the same command, `lemma mcp`, started in the project directory.

## `lemma verify <manifest>`

Checks a change across the repositories listed in a manifest and reports what it breaks. See [Verifying changes](verify.md).

| Option | Meaning |
| --- | --- |
| `--explain` | Show why each finding holds, down to the facts it rests on. |
| `--engine <name>` | `souffle` or `reference`. Default: Soufflé if installed, otherwise the reference engine. |
| `--all` | List informational findings too, not just their count. |
| `--base <commit>`, `--head <commit>` | Check these commits instead of the manifest's `change.base` and `change.head`, as in CI. |
| `--relation <name>` | Also print every row of a relation the grammar derives, such as `calls`. Repeatable. |
| `--evaluation <how>` | `demand` reads only the consumer code a finding can rest on; `full` reads everything; `both` runs the two and fails if their findings differ. Default: `demand`, or `full` with `--relation`. |

Exits with 0 when no finding is an error, 4 when one is, and 1 on failure.

## `lemma check [lemma.yaml]`

Checks a change against every repository composed by a parent `lemma.yaml`: declared links (persistence and schema agreement), links inferred from OpenAPI documents and code, and what each problem affects further up. See [Checking links](check.md). The parent may be a `.lm` workspace module instead, and without an argument, `mod.lm` is read when there is no `lemma.yaml`; a repository with only `mod.lm` is read too. A module parent's own requirements run beside the HTTP contracts, its behaviors run (with their checkers), and its allowances apply, as its `check main` selects them. See [The `.lm` language](language.md).

| Option | Meaning |
| --- | --- |
| `--change <name>` | The changed repository, as the workspace names it (`billing`, or `#billing` as a `lemma.yaml` writes it), or `self`. Needs `--base` and `--head`. |
| `--base <commit>`, `--head <commit>` | The commits before and with the change. |
| `--level <level>` | `audit` (problems are warnings), `check` (problems and violated behaviors are errors) or `strict` (error-severity behaviors that are unknown or stale are errors too). Overrides the graph's level. |
| `--all` | List informational findings too, not just their count. |
| `--select <check>` | Run this check of a parent `mod.lm` instead of its `check main`. |
| `--no-run` | Run no checkers: behaviors use the evidence already kept in Lemma's cache. A case whose inputs changed since is `stale`; one never run is `unknown`. |
| `--engine <name>` | The rule engine for inferred links: `souffle` or `reference`. Default: Soufflé if installed. |
| `--evaluation <how>` | `demand` (the default) reads only the consumer code inferred findings can rest on, asking for more callers as traces reach further operations; `full` reads everything. |
| `--links` | Also list every link found: those declared, then the calls inference found from code to documented operations, by calling repository and the service called, with each group's count and first ten calls (`--all` lists every call). Inference then reads all consumer code, and, without `--change`, runs once at head without adding findings. With `--json`, the list is `link_list`: `declared` (`caller -> provider`) and `inferred` (`repository`, `location`, `operation`, `service`). |
| `--anchors` | Also ask language servers who calls each link's caller anchor, and warn when nothing calls an anchored method while a sibling method is called. Uses the running daemon's warm servers, or starts its own without one. |

Each behavior is printed as `behavior <identity>: <outcome> (<held> of <cases> cases held, <reused> of <n> evidence reused)`, evidence reused being what checkers kept from an earlier run with the same inputs, and each case that did not hold is a finding naming the expression, its file and line, each operand's value and the `because` text. A finding an allowance covers ends with `(allowed: <reason>)` and is informational. With `--json`, `behaviors` lists every behavior with its cases (`identity`, `outcome`, `problem`, `introduced`, `allowed`) and the evidence each used (`checker`, `case`, `key`, `reused`, `unavailable`); `requirement_changes` lists what a change added, removed or changed; findings carry `allowed`.

Exits with 0 when no finding is an error, 4 when one is, and 1 on failure.

## `lemma daemon`

Runs commands for `lemma` with warm state: open databases, watchers on indexed directories, running language servers, and symbol names in memory. Every command starts it automatically; `lemma daemon` on its own runs it in the foreground. Language servers start when a lookup needs one, when `lemma index` runs (in the background), or with `lemma daemon lsp start`; there is one per indexed root and language, and idle ones are paused and stopped by the settings below.

| Command | Meaning |
| --- | --- |
| `lemma daemon status` | Whether it runs as a login service, and its language servers: starting, running or paused, how long idle, and memory. |
| `lemma daemon stop` | Stop the running daemon. |
| `lemma daemon install` | Run the daemon as a login service (launchd on macOS, systemd on Linux), so it is already up when an agent asks its first question. It is restarted only if it crashes. |
| `lemma daemon uninstall` | Remove the login service and stop its daemon. |
| `lemma daemon lsp [path]` | The language servers for the code indexed at `path` (a repository, or a workspace's members; the current directory by default): for each language, the server it uses and what it is doing (`stopped`, `starting`, `running`, `paused`, `failed` and why, or `not installed` and how to install one), then servers running elsewhere. `--json` prints the rows. |
| `lemma daemon lsp start [path] [--language <l>] [--provider <id>] [--wait]` | Start those servers in the daemon, in the background, or with `--wait` until each has loaded its project, reporting how long it took or why it failed (exit 1). A language's providers are tried in order, as a lookup tries them: when the first cannot start (typescript-language-server without a TypeScript it can use), the next serves. |
| `lemma daemon lsp stop [path] [--language <l>] [--provider <id>]` | Stop those servers. |

On Linux, `install` uses a systemd user unit. On a server you reach over SSH there may be no user session to run it in; `sudo loginctl enable-linger $USER` gives you one that starts at boot and outlives your logins.
| `lemma daemon --idle-secs <n>` | Run in the foreground, exiting after this many seconds without a command instead of the configured `idle_exit`. |

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
```

Durations take `s`, `m`, `h` or `d`, or `never`; sizes take `MB` or `GB`, or `none`. The values above are the defaults. Unknown settings are rejected, so a typo shows up instead of being ignored; a file that stops parsing while the daemon runs leaves the previous settings in force.

A paused server, and every process it started, is stopped with `SIGSTOP`: it uses no CPU, and the operating system can compress or swap out its memory under pressure. The next lookup resumes it, which took about 45 ms for gopls in testing, against seconds to start a server again.

There is one daemon per user. It talks to `lemma` over a Unix socket in the temporary directory that only your user can open, and finds language servers on the `PATH` of each command it runs. A daemon started by a different build of Lemma is replaced automatically. The daemon is not available on Windows; commands there always run in-process.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. For `refs`, the server answered, including "no references found". |
| `1` | Error, such as no index, an unknown symbol, or an invalid configuration file. The message is printed to stderr. |
| `2` | `refs`, `implementations`, `signature` and `graph`: the name matches several symbols. The candidates are printed. |
| `3` | `refs`, `definition`, `implementations`, `signature` and `graph`: no answer. The file is stale, no server is installed, the server failed, or it does not support the question. |
| `4` | `verify` and `check` only: a finding is an error. |

## `lm`

`.lm` modules have their own command, `lm`, installed beside `lemma`. It checks, formats and converts modules and serves them to editors; `lemma check` and `lemma verify` run what they declare. Every `lm` command runs in its own process, without Lemma's daemon or index.

```text
lm [--json] [-l <level>] <command>
```

See [The `.lm` language](language.md).

| Command | Meaning |
| --- | --- |
| `lm check [mod.lm]` | Loads a module and everything it imports, and prints every problem as `file:line:column: severity: message`, then a summary. With `--json`, `diagnostics` lists each with its `module`, `path`, `line` and `column`. Exits with 1 if any is an error. |
| `lm fmt [paths...]` | Rewrites `.lm` files in the canonical layout ([spec 13, Layout](../.specs/13-lm-grammar.md#layout)): four-space indentation by brackets and continued lines, one space around operators and after `,` and `:`, at most one blank line in a row. Line breaks and comments are kept. Each path is a file or a directory to search; with none, the current directory is searched, skipping hidden directories, `target` and `node_modules`. A file with syntax errors is left alone and its errors are printed; a fragment without a `module` line is formatted. `--check` lists the files that are not formatted instead of rewriting them. Exits with 1 if a file has syntax errors or, with `--check`, is not formatted. |
| `lm convert [lemma.yaml]` | Writes a `lemma.yaml` as an `.lm` root module. `--module <name>` names it (the directory's name by default); `--write` writes `mod.lm` beside the file, refusing to replace one. |
| `lm lsp` | Serves `.lm` modules to an editor over the Language Server Protocol, on standard input and output. |
