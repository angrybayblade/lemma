# Workspace daemons: one daemon per workspace, its sentry and its clients

**Status: designed, 2026-11-15; steps 1 to 6 of [Build order](#build-order) built, 2026-11-16.** One daemon serves each workspace, found by walking up and started at the workspace's root, and it refuses directories it does not serve; `[daemon] autostart` says who may start one; the daemon answers `check`, through sentry or by itself, and `verify` and `graph --workspace`. Sentry is attached when a client asks and let go when it leaves, and it owns graph preparation and restarting servers. Lookups answered from the index, status and checks no longer wait behind a lookup that waits for a language server. Not built: a lane per language server, the names of the three `lsp` commands, and settings in `.lm`; the settings below are TOML keys under `[daemon]`, `[servers]` and `[sentry]` until then. The rows marked *decided* under [Decisions](#decisions) were settled in the design discussion of 2026-11-15; rows marked *proposal* follow from them and still need the same review. Measurements are from Lemma's own repository and from the six-repository bench workspace, on Linux.

## Problem

Lemma has a daemon, a sentry and three clients (the command line, the MCP server and the editor's server), and how they relate cannot be said in a sentence. A user cannot tell from a command whether its answer will be warm.

What the code did when this was written:

| Operation | Where it runs |
| --- | --- |
| Lookups (`symbols`, `refs`, `graph` in one repository) | In the daemon, which the command line starts on first use |
| `lemma check`, and the MCP `check` tool | In the client's own process. It borrows the daemon's warm runtimes ([spec 20](20-warm-runtimes.md)) if a daemon runs, and is answered by the daemon only when sentry is attached to that workspace |
| `lemma graph --workspace` | In the client's own process, borrowing what the daemon has kept |
| The editor's server (`lemma lsp`) | Asks sentry, starting the daemon and attaching sentry by itself; watches the workspace in its own process when sentry is attached to another |

Sentry is reached by three different rules:

- The command line and MCP use it if it is attached, and never attach it unless `[sentry] auto_attach` is set.
- `lemma daemon sentry start` attaches it by hand, and fails when no daemon runs, though every lookup starts one.
- The editor's server starts the daemon and attaches sentry without being asked.

So whether `lemma check` is warm depends on whether an editor happens to be open on the same workspace.

There are four more sources of confusion:

- **One daemon serves every directory on the machine**, but sentry attaches to one workspace at a time. A second workspace gets nothing prepared.
- **Watching and preparing ahead has three names.** Sentry prepares a check's inputs. `[graph] prepare` has the daemon re-answer recent graphs after an edit. `[servers] warm_on_activity` has it restart a project's language servers when files change. Only the first is called sentry.
- **The daemon answers one lookup at a time, for every workspace.** Its loop takes a request, finishes it, and takes the next; only a check handed to sentry is answered on a thread of its own. A first `lemma refs` on a large Go type (36 s on the bench) holds up every other lookup on the machine for that long.
- **Settings are in three TOML files** (`~/.config/lemma/config.toml`, `.lemma/config.toml`, `.lemma/providers.toml`) beside the `.lm` description, which already holds one setting, `workspace { config: { level: check } }`. Two formats describe one workspace, and only one of them is checked by `lm check` and completed in editors.

## Goal

Three roles, and one sentence for each:

| Role | What it is | Never does |
| --- | --- | --- |
| **Daemon** | The process for one workspace. It runs the language servers and interpreters, holds the index, and runs every lookup, graph and check | Serve code outside its workspace |
| **Client** | The command line, the MCP server, or the editor's server. It sends a request and shows the answer | Run a lookup or a check itself while a daemon is reachable |
| **Sentry** | The daemon's optional watcher for its workspace. It listens to file changes and prepares what the next request will need | Answer anything, or change what an answer says |

```text
workspace A ── daemon A ── (sentry A, optional) ◀── command line · MCP · editor's server, in A
workspace B ── daemon B ── (sentry B, optional) ◀── command line · MCP · editor's server, in B
```

With sentry off, every client gets the same answers from the daemon, without the preparation ahead. With no daemon, the command line does the same work in its own process: the slowest path, and the same answers.

**Not goals:**

- Faster evaluation. This spec moves where work runs and who may start it; it does not change what a check costs once it runs.
- A machine-wide supervisor. Nothing coordinates the daemons of different workspaces.
- Keeping TOML working. Lemma has one user; the old files stop being read, with no migration.

## Decisions

| Decision | Choice | Status |
| --- | --- | --- |
| How many daemons | One per workspace | decided |
| How many sentries | At most one per daemon, so one per workspace | decided |
| Where sentry runs | Inside its workspace's daemon | decided |
| Who does the work | The daemon. `check` and `graph --workspace` move into it | decided |
| What a client does without a daemon | The command line does the work itself. MCP and the editor's server start a daemon by default | decided |
| Who may start a daemon | `daemon.autostart`: `always` (the default), `clients`, `never` | decided |
| What sentry owns | All watching and preparing ahead: check inputs, graph preparation, restarting language servers on activity | decided |
| How sentry turns on | A client asks for it: the command line, MCP or the editor's server | decided |
| What a workspace is | A Git repository, or a Git repository with its service repositories as submodules | decided |
| How a client finds its daemon | It walks up from its directory for a running daemon that serves it; if none, it starts one at the workspace's root, not in its own directory | decided |
| Code a workspace names outside the daemon's root | A hard warning at start and in status, naming the directory to start from instead | decided |
| A repository reached from two workspaces | Accepted for now: each daemon serves it, and status says so | decided |
| When a daemon exits | Once it holds nothing warm. A starting default, to adjust on feedback | decided |
| Guardrails against wrong answers | The four under [Guardrails](#guardrails) | decided, for now |
| Where settings live | In `.lm`: a `config` declaration in the workspace's root module. TOML is removed | decided |
| Personal settings | `config.lm` beside the root module, ignored by Git: a module declaring its own `config`, brought in with `load` | decided |
| How the two combine | `config { … } \| load("config.lm")`: a merge, key by key, the right side winning | decided |
| Language server configuration | Part of `config`, replacing `.lemma/providers.toml` | decided |
| Memory limits | Per workspace, since each daemon has its own settings | decided |
| Requests inside one daemon | A check is handed off as sentry's are, so lookups go on; lookups answered from the index, status and the handing over of checks do not wait behind a lookup that waits for a language server ([Lanes](#lanes)); `lemma daemon stop` waits for a lookup in flight | decided |
| When sentry lets go | When the client that asked disconnects, on `sentry stop`, or when its daemon exits | proposal |

## The workspace

A workspace is the unit everything else hangs from: one daemon, one sentry, one `config`.

A client finds its workspace's root from the directory it runs in:

1. The nearest `mod.lm` above it that declares a `workspace`.
2. Otherwise, the root of the Git checkout it is in.
3. Otherwise, its own directory.

The expected layout is a repository at the root with the service repositories under it, as submodules or plain clones. A workspace's `mod.lm` may still name a repository outside the root (`local("../billing")`). Clients in that repository walk up and never reach the root, so they would not find the daemon. The daemon warns at start and in `lemma daemon status`:

```text
warning: billing (../billing) is outside this workspace's root (/code/system).
  Commands run there will not find this daemon. Start Lemma from /code, or move billing under /code/system.
```

## Finding and starting a daemon

A client:

1. Walks up from its directory. At each directory it looks for a running daemon rooted there.
2. Asks the first one it finds whether it serves the client's checkout (see [Guardrails](#guardrails)). If it does not, the client keeps walking.
3. If no daemon serves it, and `daemon.autostart` allows this client to start one, starts a daemon at the workspace's root as found above. Otherwise it does the work in its own process.

Starting at the workspace's root, and not in the client's directory, is what makes the result independent of who asked first. An agent whose first `lemma refs` runs in `ws/repos/billing/src` gets the daemon for `ws`, as a later command run in `ws` does.

| `daemon.autostart` | Command line with no daemon | MCP and the editor's server with no daemon |
| --- | --- | --- |
| `always` (default) | Starts one | Start one |
| `clients` | Does the work itself | Start one |
| `never` | Does the work itself | Do the work themselves |

A daemon that is already running is used by every client under all three, and `lemma daemon start` starts one under all three. A command that finds a daemon of another build replaces it whatever the setting, since a daemon was wanted there. `--no-daemon` and `LEMMA_NO_DAEMON=1` keep their meaning: this command runs in its own process even if a daemon is running. `never` is for a machine where nothing should stay in the background, such as CI.

Each daemon listens on a socket in the user's runtime directory (`$XDG_RUNTIME_DIR/lemma`, or `/tmp/lemma-<uid>` without one), named by a digest of its root's canonical path, so a client can test each directory on the way up with one lookup and no file in the repository. It is not in the directory `TMPDIR` names: a daemon does not keep the `TMPDIR` of the command that started it, and the two must look in the same place.

`LEMMA_DAEMON_SOCKET` still names one daemon's socket outright. That daemon serves every directory, as before this spec, which is what the gates and the benchmarks run.

### Commands

`lemma daemon status` and `lemma daemon stop` act on the daemon serving the current directory, and take `--all` for every daemon of the user. Status says first which workspace that daemon serves. A daemon of another build replaces itself when a command of the new build reaches it, so nothing has to be stopped after installing a new `lemma`.

`lemma daemon install` and `uninstall` are removed (decided 2026-11-16): with no one daemon there is no one service to keep up at login. A daemon starts on a workspace's first request, or with `lemma daemon start` run in the workspace, which starts its daemon in the background and returns once it listens. `lemma daemon` alone still runs one in the foreground.

### When a daemon exits

A daemon exits by itself once it holds nothing warm (no language server running or starting, no warm runtime, no sentry attached, no workspace watched for graphs) and no command has come for as long as `[servers] stop_after`, the time an idle server is kept. `[servers] stop_after` and `[runtime] stop_after` already let servers and runtimes go when idle, so a workspace nobody uses reaches that state without another setting. With `stop_after = "never"` servers are kept, and so is the daemon. `daemon.idle_exit` stays as a limit of its own: the daemon exits after that long without a command, whatever it holds.

A client that connects for one request cannot be told from one that has gone, so "no client is connected" is not part of the rule; the time since the last command stands in for it.

This replaces the default of never exiting. It is a starting point: the cost of a daemon that exited too early is its language servers loading again (seconds, or over a minute for a large Python repository), and feedback decides whether the default should hold longer. Until sentry owns restarting servers on activity (step 4), a daemon that has exited no longer restarts them when files change; the next command does.

## What runs in the daemon

Every operation, when a daemon serves the client:

| Operation | Before | After |
| --- | --- | --- |
| `symbols`, `outline`, `refs`, `definition`, `implementations`, `signature`, `graph` | Daemon | Daemon |
| `graph --workspace` | Client's process | Daemon |
| `check` | Client's process; daemon only with sentry attached | Daemon |
| `verify` | Client's process | Daemon |
| The editor's server's checks | Sentry, or the server's own process | Daemon, on each save the editor reports, or as sentry reports changes. Not built: it still watches the workspace in its own process when sentry is not attached to it (step 4) |

A check sends the client's directory, arguments and environment, as an ask to sentry does today, so an adapter or checker runs with the toolchain of the shell that asked. The daemon gives the check to sentry when sentry is attached to its workspace. Otherwise it runs the check in a short-lived process of its own, from the asker's directory with the asker's environment, and passes on what it prints: the report a check gives when its asker runs it. `verify` is answered the same way.

That process is deliberate. Answered on a thread of the daemon's instead, a check's memory stayed in the daemon: after 214 checks of the gates' workspace the daemon held 1.35 GB, and after the 100,000-file workspace 614 MB (2026-11-16). A process gives everything back when it ends. Sentry keeps what it reads on purpose, and is the one way a check's state stays in the daemon.

A check that asks for logs runs in the client's process, since the daemon does not pass one command's log lines on. So does one the daemon cannot run, because the connection then closes unanswered and the client does the work itself.

The path without a daemon is the same code run in the client's process. It is a fallback, not a second behaviour: [Guardrails](#guardrails) requires its answers to equal the daemon's.

### A request does not wait behind another

A check handed to sentry is answered on sentry's thread, and any other check or verify in a process of its own. So a lookup never waited behind a check.

What did wait, until step 6, was everything behind a lookup. The daemon read one request at a time and answered it before reading the next, so while a lookup waited for a language server no other request was even read: not a symbol search, not `lemma daemon status`, not a check being handed to sentry, and not the question every command asks first, whether this daemon serves its directory. Measured on 2026-11-16 with a language server that never answers, a symbol search that takes 0.01 s by itself took 299 s behind a `refs` in the same daemon.

#### Lanes

*Built in step 6.* One thread only reads requests. It answers what needs no engine itself, and passes every other request to a **lane**: a thread that answers its requests one at a time, in the order they came.

| Who answers | What |
| --- | --- |
| The reading thread | Whether the daemon serves a directory; handing a check to sentry or to its own process; attaching and letting go of sentry; `lemma daemon status` while the provider lane is answering a command |
| The index lane | `symbols` and `outline`: lookups answered from the index alone |
| The provider lane | Every lookup that may ask a language server (`refs`, `definition`, `implementations`, `signature`, `graph`, the editor's lookups), `index`, the daemon's upkeep, `lemma daemon stop`, and `lemma daemon status` when it is idle |

Three rules keep the answers what they were:

- **An index lookup is never stale.** `symbols` and `outline` still bring the index up to date before they answer. Each lane has its own engine, and the two share the indexes they keep up to date and the in-memory name search, behind a lock held only while an index is being brought up to date, never while a provider is awaited. A directory is still watched once and read once. Reading needs nothing new: the store already takes one writer and any number of readers.
- **The provider lane is as it was.** Its requests are answered one at a time in the order they came, so nothing about a language server's session, the graphs kept ready or the upkeep changes. Getting ready for a graph gives way to a request waiting for this lane, as it gave way to one waiting to be read.
- **Stopping waits.** `lemma daemon stop` goes through the provider lane, so a lookup in flight is answered before the daemon exits (decided 2026-11-16).

While the provider lane answers a command, `lemma daemon status` says so in place of the lines about its language servers, which only that lane can give:

```text
busy with `lemma refs price` for 42s, 2 waiting behind it; its language servers are listed once it is answered
```

A command's log lines (`--logging`) are kept for its caller per lane: the provider lane's command keeps the lines of every thread but the index lane's, since providers log from threads of their own, and an index lookup keeps the lines of the thread it runs on.

Measured on 2026-11-16, on Lemma's development machine:

| Case | Before | With lanes |
| --- | --- | --- |
| `symbols` while `refs` waits on a server that never answers | 299 s | 0.01 s |
| `symbols`, median, while a real pyright answers `refs` again and again, each answer held back 2 s (60 s run) | 2.15 s | 0.012 s |
| Index lookups answered in those 60 s, with a file added before each | 13 | 3,038 |
| Wrong answers among them (a name just added not found, one just removed still found) | 0 | 0 |
| `refs` answers equal to the one a process of its own gives | 29 of 29 | 27 of 27 |
| `symbols` by itself, median of 900 | 3.98 ms | 4.24 ms |
| `refs` from a stored answer, median of 900 | 4.97 ms | 5.09 ms |
| The daemon's memory after the same commands | 12 to 13 MB | 14 MB |

The cost is the hand-over between threads: about 0.2 ms a command, and two more threads.

Not built: a lane per language server, so that a Python lookup is answered while a Go server loads. The provider lane's requests still wait for each other. It needs the engine's sessions split by provider, and waits for a measured need.

One thing still runs on the reading thread that can take time: attaching sentry to a workspace for the first command that asks, which sets up its watches. It happens once per daemon.

#### A lookup's own wait

*Fixed in step 6.* A lookup waits `--timeout-secs` (60 unless given) for a provider. A session started ahead of any lookup, as `lemma index` does through the daemon, is started with the five minutes that loading a large workspace may take, and a lookup that found such a session waited those five minutes instead of its own: `refs --timeout-secs 12` took 300 s. Every ask now gets what its lookup allows.

A provider that does not answer in time is stopped, as it always was after its own wait: it may be stuck, and the next lookup starts it again. For a repository whose server needs longer than a minute for some names, that is what `[servers] when_slow = "next"` and a longer `--timeout-secs` are for.

## Sentry

Sentry is the daemon's watcher for its workspace. It is off until a client asks for it.

| Client | How it asks |
| --- | --- |
| Command line | Any command, unless `[sentry] autostart` says otherwise; `lemma daemon sentry start` by hand. Either starts the daemon first when `daemon.autostart` allows |
| MCP server | When it starts, unless `[sentry] autostart` is `never`. Not in CI, with `LEMMA_SENTRY=off`, or with no root module above its directory |
| Editor's server | When it starts, for the workspace it was opened on |

What sentry owns, all of it under one name:

| Preparation | Was |
| --- | --- |
| A check's inputs: files listed, hashed and parsed in batches as they change ([spec 14](14-sentry.md)) | Sentry |
| Recent graphs answered again after an edit settles | `[graph] prepare`, `[graph] kept_ready` |
| A project's language servers started again when its files change | `[servers] warm_on_activity` |
| The limit on directories watched | `[graph] watches` |

With sentry off, none of these happen. The answers are the same; the first one after an edit pays for what changed, and a graph across the workspace asks Git about each repository.

One watcher is not sentry's (decided 2026-11-16): the daemon watches each indexed directory so that a lookup knows what changed without walking the tree. It does no work ahead of a request, and without it every lookup would check the files itself. It stays with the daemon whether sentry is attached or not.

Checks and graphs still watch a workspace's repositories with a set of watchers each (decided 2026-11-16: merged later, as its own change, since it shares state between sentry's thread and the daemon's loop). A repository without a root module above it has no sentry, since sentry attaches to an entry module.

**Who asks unasked (decided 2026-11-16).** `[sentry] autostart` takes the values `[daemon] autostart` does: `always` (the default: any command asks, and so do the MCP server and the editor's server), `clients` (those two only) and `never` (by hand only). The default is `always` because of what going without costs. On the six-repository bench workspace a graph asked after an edit took 0.03 to 0.10 s with sentry attached and 3.2 to 4.6 s without: unwatched, the daemon learns only that a repository changed and reads all of it again. Agents run `lemma` commands as often as they call MCP tools, so `clients` as the default would have made most of them some eighty times slower at this than they were. Reading only the files Git names as changed would bring the cost without sentry down; that is left to the next round of performance work.

Sentry lets go when the last client that asked for it disconnects (an MCP server or an editor's server that exits), on `lemma daemon sentry stop`, and when its daemon exits. A client asks by holding a connection open, which the daemon sees close however the client ends. Asked for by a command, sentry stays until stopped, or until no client holds it and no command has used it for `[servers] stop_after`, the time an idle language server is kept: no setting of its own.

Spec 14's rule that sentry never evaluates rules, runs resolvers or runs checkers ahead of an ask stands. Its `auto_attach` setting goes: a client asking is the one way sentry turns on.

## Guardrails

Walking up to find a daemon can reach one that does not serve the client's code. An agent's Git worktree is often placed inside the main checkout (under `.claude/worktrees/`, for one); a daemon may have been started in a home directory. Either would answer about different files than the client is looking at, and the answer would look right. A slow answer is acceptable; a wrong one is not.

1. **Match by checkout, not by folder.** A daemon serves a client only if the root of the client's Git checkout is one of the daemon's own repositories. A worktree nested in a checkout has a different root, so it does not match the daemon above it. *Built.*
2. **Refuse, never substitute.** A command about a directory the daemon does not serve is refused by the daemon and goes to that directory's own. A file named from inside one checkout that lies in another is an error naming the file, the directory indexed, and where to run the command instead. The daemon never answers about a file of the same name elsewhere. *Built.*
3. **Every answer names its source.** *Built in a lighter form, the fuller one open.* With `--logging info` a command says which workspace's daemon it asks, and `lemma daemon status` names the workspace serving a directory, or the one a daemon would be started for. As first written, JSON output would carry the root of the workspace that answered and text output would say so when that root is not the client's own checkout. That changes the JSON of every command, makes a daemon's answer differ from the same command's in its own process, and adds a line to every lookup in a member repository; whether it is worth that is to be decided.
4. **A gate that tries to fool it.** *Built, as tests of the command itself* (`crates/lemma-cli/tests/daemons.rs`). With the daemon of a checkout running, the same commands are asked from the checkout, a subdirectory, a sibling checkout, a worktree inside it and a subdirectory of that, each defining a function of its own: every answer must equal the one the command gives in its own process, and name that place's function. `check` and `verify` are held to the same, with sentry and without.

The check that a file has not changed since it was indexed stays underneath these.

## Configuration in `.lm`

A workspace's root module declares its settings:

```lm
module shop

config {
    level: check
    daemon: { autostart: always }
    servers: { python: [pyright, ty], stop_after: "2h" }
} | load("config.lm")
```

`config.lm`, beside it and ignored by Git, is a module like any other. It declares a `config` with only what a person wants different:

```lm
module shop.config

config {
    servers: { python: [ty], memory_budget: "8GB" }
}
```

Its `module` line follows the rule for every module ([spec 13](13-lm-grammar.md#modules)): the name says where the file is.

| Setting | Comes from | Value |
| --- | --- | --- |
| `level` | the root module | `check` |
| `daemon.autostart` | the root module | `always` |
| `servers.python` | `config.lm` | `[ty]` |
| `servers.stop_after` | the root module | `"2h"` |
| `servers.memory_budget` | `config.lm` | `"8GB"` |

The rules:

- **`|` merges two records, and the right side wins**, as `|` does for Python's dictionaries. Nested records merge key by key. A list replaces a list; lists are not joined.
- **`load(path)`** gives the `config` that the module in a file beside this one declares. A file that is not there contributes nothing, silently. A file with an error, or a module that declares no `config`, is an error from `lm check`, with its file and line; it does not count as missing.
- **A module declares at most one `config`.** The root module's is the workspace's settings. Another module's counts only when the root module's `config` loads it, and it is a record alone: it does not load a further module. `config { … }` with no `load` is valid, for a team that wants no personal settings.
- **Settings are typed.** A key that does not exist is reported with the keys that do, as any record's is, and editors complete them.
- **A root module that fails to load does not change settings.** The daemon keeps the last settings that loaded, and status says so.
- **A repository with no `mod.lm` runs on defaults.** To change one, it adds a `mod.lm` with a `module` line and a `config`.
- **There is no file for the user or the machine.** What is true of a machine and not of a workspace stays an environment variable (`LEMMA_NO_DAEMON`).

A personal `config.lm` can set `level`, and so make a check pass locally that CI fails. CI has no `config.lm`, so the team's level holds where it gates. The documentation says this rather than the language forbidding it.

*Proposal: how values are written.* A choice among names is written as a name, as `level: check` already is: `autostart: always`, `when_slow: next`. Times and sizes stay strings: `"2h"`, `"8GB"`.

### The settings

`workspace { config: { level: … } }` moves into `config`. The sections of `config.toml` keep their names and keys, with these changes:

| Was | Becomes |
| --- | --- |
| `[daemon] idle_exit` | `daemon.idle_exit`, an upper bound; a daemon also exits once nothing is warm |
| new | `daemon.autostart` |
| `[servers] warm_on_activity` | `sentry.servers` |
| `[graph] prepare`, `kept_ready`, `watches` | `sentry.graphs`, `sentry.kept_ready`, `sentry.watches` |
| `[sentry] auto_attach` | removed |
| `[servers] memory_budget` | the same key, now a limit for one workspace |
| `[servers] pause_after`, `stop_after`, `when_slow`, `give_way_after`, `try_again_after`; `[runtime] memory`, `stop_after` | unchanged |

### Language servers

*Proposal.* `.lemma/providers.toml` becomes two keys of `config`. `servers.<language>` is the order servers are tried in, and `providers` defines or replaces one:

```lm
config {
    servers: { python: [jedi, pyright] }
    providers: {
        jedi: { languages: [python], command: "jedi-language-server" }
        pyright: { settings: { python: { analysis: { diagnosticMode: "openFilesOnly" } } } }
    }
}
```

A workspace's repositories sometimes want different servers (`ty` for one large Python repository, `pyright` for the rest). `repos.<name>` holds the keys that differ for one repository:

```lm
config {
    servers: { python: [pyright, ty] }
    repos: { ledger: { servers: { python: [ty] } } }
}
```

The built-in list stays compiled in, and `--providers <file>` goes with the TOML format.

## What changes in other specs

| Spec | Change |
| --- | --- |
| [7, providers](07-semantic-providers-and-indexing.md) | Provider configuration is in `config`; the daemon is per workspace |
| [13, `.lm` grammar](13-lm-grammar.md) | The `config` declaration, `load`, and `\|` on records |
| [14, sentry](14-sentry.md) | Sentry stops answering checks and only prepares; one per workspace's daemon; turned on by a client asking; owns graph preparation and server restarts |
| [20, warm runtimes](20-warm-runtimes.md) | A check runs in the daemon, so it no longer asks a daemon from outside; the rule that no daemon is started for a runtime gives way to `daemon.autostart` |

## Open questions

1. **What the three things called "lsp" are named.** `lemma lsp` is Lemma serving an editor, `lemma daemon lsp` manages the language servers Lemma uses, and `lm lsp` serves `.lm` modules. One name per thing is wanted; this spec calls them the editor's server, language servers, and the `.lm` server.
2. **Whether Lemma says anything when `config.lm` is tracked by Git.** It is meant to be ignored; Lemma does not edit `.gitignore`.
3. **Whether `sentry.stop_after` is needed**, or whether a sentry asked for from the command line simply lives as long as its daemon.
4. **How an agent turns sentry on through MCP** when the workspace's `config` does not: a tool, or not at all.
5. **What a second daemon does about a repository another daemon already serves**, beyond saying so. Two daemons watching one checkout were measured slowing each other (one lookup took 3.5 s that takes 40 ms alone).

## Build order

Each step leaves every gate passing.

1. **One daemon per workspace.** Roots, discovery by walking up, the socket per root, starting at the workspace's root, `status` and `stop` with `--all`, exit when nothing is warm, and the first two guardrails.
2. **`daemon.autostart`,** and the command line's own-process path for every operation.
3. **Checks in the daemon.** `check`, `verify` and `graph --workspace` move in, handed off as sentry's asks are; sentry stops being what answers a check.
4. **Sentry asked for by clients,** owning graph preparation and server restarts.
5. **The remaining guardrails:** the source in every answer, and the differential gate.
6. **Lookups side by side.** Measured first: a symbol search waited 299 s behind a lookup whose server never answered. Built as [lanes](#lanes); a lane per language server is left until it is measured to matter.
7. **Documentation and names:** one page that states the three roles, and the outcome of open question 1.
8. **`config` in `.lm`:** the declaration, `load`, `|`, the typed settings, language servers; TOML reading removed. Last: the settings above are TOML keys until then.

## What to measure

- A lookup's time while a check runs in the same daemon, and while a language server loads for another lookup.
- `lemma check` through the daemon against its own process, cold and after an edit, with sentry off and on.
- A daemon's memory with sentry off and on, per workspace.
- Time from a workspace's first request to its first answer, with no daemon running.
- The differential gate of step 5: no case differs.
