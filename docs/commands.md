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

Every command runs in a background daemon that keeps the index open, watches indexed directories for changes, and keeps language servers warm; `lemma` itself only forwards the command. The first command starts the daemon. See [`lemma daemon`](#lemma-daemon).

## `lemma index [path]`

Reads the source files under `path` (default: the current directory) and records their definitions in a snapshot. Re-indexing is incremental: only new or changed files are read and parsed, so running it after every edit is cheap (see [How Lemma works](concepts.md#re-indexing-is-incremental)).

- Supported files: `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, `.pyi`, `.go`.
- Skipped: anything matched by `.gitignore`, hidden files and directories (including `.lemma/`), and `node_modules`.
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
| `--kind <kind>` | Only one kind: `function`, `method`, `class`, `interface`, `struct`, `enum`, `type_alias`, `constant`, `variable`. |
| `--language <language>` | Only one language: `typescript`, `tsx`, `python`, `go`. |
| `--path <text>` | Only symbols whose file path contains this text. |
| `--limit <n>` | At most this many results (default 50). |

Qualified names join enclosing definitions with dots: a method `delete` in class `Router` is `Router.delete`. For Go methods the receiver type is used: `func (s *Server) Start()` is `Server.Start`.

Ids (`#412`) are valid until the next `lemma index` that records a change.

## `lemma refs <symbol>`

Finds references to one symbol by asking a language server for its language.

`<symbol>` can be:

- a name, such as `delete`;
- a qualified name, such as `Router.delete`;
- an id from `lemma symbols`, such as `'#415'`. Quote it, because `#` starts a comment in most shells.

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
| changed since it was indexed | The file was edited after indexing, so the stored position may be wrong. Run `lemma index` again. |
| no provider is installed | No server for this language is on `PATH`. |
| lookup failed | A server started but failed, or none could start. The message says why. |

Lines starting with `skipped` name servers that could not start before one did.

## `lemma providers`

Lists every configured language server, whether its command was found, and the order they are tried in for each language. It reads the configuration of the indexed project if there is one, otherwise `.lemma/providers.toml` in the current directory.

## `lemma daemon`

Runs commands for `lemma` with warm state: open databases, watchers on indexed directories, running language servers, and symbol names in memory. Every command starts it automatically; you only need this command to run it in the foreground, check on it, or stop it.

| Option | Meaning |
| --- | --- |
| `--status` | List the daemon's language servers: starting, running or paused, how long idle, and memory. |
| `--stop` | Stop the running daemon. |
| `--idle-secs <n>` | Exit after this many seconds without a command, instead of the configured `idle_exit`. |

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
| `2` | `refs` only: the name matches several symbols. The candidates are printed. |
| `3` | `refs` only: no answer. The file is stale, no server is installed, the server failed, or it does not support references. |
