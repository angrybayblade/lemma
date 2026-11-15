# Language servers

Lemma calls a language server a *provider*: a source of facts about code that Lemma cannot work out from syntax alone, such as where a symbol is used. Lemma works with any server that speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) over stdio.

## Built-in providers

| id | Languages | Command |
| --- | --- | --- |
| `typescript-language-server` | typescript, tsx | `typescript-language-server --stdio` |
| `typescript-native` | typescript, tsx | `tsc --lsp --stdio` |
| `pyright` | python | `pyright-langserver --stdio` |
| `ty` | python | `ty server` |
| `basedpyright` | python | `basedpyright-langserver --stdio` |
| `gopls` | go | `gopls` |
| `rust-analyzer` | rust | `rust-analyzer` |

Install instructions are in [Getting started](getting-started.md#2-install-a-language-server).

`typescript-language-server` needs TypeScript 6 or older. TypeScript 7 replaced it with a native server built into `tsc`, which is the `typescript-native` provider. Whichever matches your TypeScript installation starts, and the other is skipped.

## Provider selection

For each lookup, Lemma takes the providers configured for the symbol's language, keeps those whose command is on `PATH`, and tries them in order:

1. Project providers, in file order.
2. Built-in providers, in the order above.

If a provider fails to start, Lemma tries the next one and reports the skipped one. It does not start that provider again for the rest of the command, or for a minute in the daemon: a graph asks dozens of questions, and each failed start takes a tenth of a second. If a provider starts but then fails while answering, Lemma reports the failure instead of trying another. Switching quietly would hide a real problem, such as a broken project setup. One kind of failure can be handed to the next provider instead, if you ask for it: a server that does not answer in time (see [When a server is too slow](#when-a-server-is-too-slow)).

`lemma providers` shows the order for each language.

Starting a server on a large project can take several seconds, so a background daemon keeps servers running after the first lookup (see [Concepts](concepts.md#the-daemon-and-warm-language-servers)). Lemma also caches each answer until the next content change.

## Add or override a provider

Create `.lemma/providers.toml` in the indexed project, or pass any file with `--providers <file>`. It uses the same format as the built-in list:

```toml
# Try jedi before the built-in Python servers.
[[provider]]
id = "jedi"
languages = ["python"]
command = "jedi-language-server"

# Replace the built-in pyright entry: same id, different settings.
[[provider]]
id = "pyright"
languages = ["python"]
command = "pyright-langserver"
args = ["--stdio"]

[provider.settings.python.analysis]
diagnosticMode = "openFilesOnly"
extraPaths = ["src"]
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Unique name, shown in output. An entry with the id of a built-in replaces that built-in. |
| `languages` | yes | Languages it answers for: `typescript`, `tsx`, `python`, `go`, `rust`. |
| `command` | yes | Executable name on `PATH`, or a path to it. |
| `args` | no | Arguments. Most servers need `--stdio`. |
| `initialization_options` | no | A table sent to the server when it starts, as LSP `initializationOptions`. Check the server's documentation for what it accepts. |
| `settings` | no | A table the server can read back through `workspace/configuration`. Write keys the way the server documents them, for example `[provider.settings.python.analysis]` for pyright's `python.analysis` section. |
| `ready_messages` | no | Patterns for a log message, or the title of a progress report that finishes, that the server sends once it has loaded the project. `*` matches any text. Lemma waits for one before asking, instead of guessing from diagnostics and quiet periods. If none arrives within 10 seconds, a quiet spell counts instead. |

Lemma rejects unknown fields, so a typo such as `comand` causes an error instead of being ignored.

`--providers` replaces the project file; it does not add to it. Built-in providers are always available.

## When a server is too slow

A lookup waits `--timeout-secs` for a server to answer, 60 seconds unless you say. On most repositories no server comes near that, so the default is to treat a timeout like any other failure: the lookup says so and exits 3.

A very large repository is different. A server can be too slow there for some questions every time they are asked. In one Python repository of tens of thousands of files, pyright could not list the uses of 11 of 120 sampled symbols within 60 seconds, widely used names such as a method called `get`. Each of those lookups failed after about 72 seconds, kept nothing, and waited again the next time. A graph that asks about such a symbol has no callers under it.

`[servers] when_slow` in [the settings](commands.md#settings) says what a lookup does then:

```toml
[servers]
when_slow = "next"         # "report" (the default) or "next"
give_way_after = "20s"     # with "next": how long a server gets before the next is asked
try_again_after = "30m"    # with "next": how long the slow server is passed over
```

| `when_slow` | When a server does not answer in time |
| --- | --- |
| `"report"` | The lookup fails and says which server timed out. Nothing else is asked. |
| `"next"` | The next installed server for the language is asked the same question. Once one answers, the slow server is passed over in that repository: lookups there go straight to the next server, until `try_again_after` has gone by. |

Two times set where between the two servers a repository sits. `give_way_after` is how long a server that has another after it may take over one question:

| `give_way_after` | What it means | What it costs |
| --- | --- | --- |
| not set (the default) | As long as the lookup waits for any server: `--timeout-secs`, 60 seconds unless given. | The longest wait before the next server is asked. The first server answers everything it can answer in a minute. |
| a time, such as `"20s"` | That long, whatever the command says and whoever started the server. | A shorter wait, and the first server loses the questions it would have answered between that time and a minute. |

It is a time and nothing else: `when_slow = "report"` is the setting that never asks the next server, and a server to be asked first belongs in `.lemma/providers.toml`. The last server installed for a language, having none after it, still gets the whole `--timeout-secs`. Set or not, it applies to a server the daemon started on its own when files changed, which otherwise waits up to five minutes for an answer.

`try_again_after` is how long the slow server is then left alone, and trades waiting against going back to the server you listed first:

| `try_again_after` | What it means | What it costs |
| --- | --- | --- |
| `"0s"` | The slow server is asked first every time. | Every question it is too slow for waits `give_way_after` before the next server is asked. Its answers are used wherever it is fast enough. |
| `"30m"` (the default) | It is passed over for half an hour, then asked first again. | One wait of `give_way_after` every half hour, if it is still too slow. |
| `"never"` | It is not asked again in that repository while the daemon runs. | No waiting after the first. Its answers are not used again there, apart from those already kept. |

On the repository above, with `when_slow = "next"` and ty installed, all 11 of those graphs were answered. The first took 109 seconds: pyright's 59, then ty starting and reading the repository. With `give_way_after = "20s"` it took 66. With pyright passed over, the other ten took 0.7 to 7.8 seconds, and any of them asked again took a second or less. Eight graphs pyright does answer were asked in that half hour too, and came back from ty with as many nodes or more, in 0.1 to 1.2 seconds where pyright had taken 4 to 47.

What to know before turning it on:

- **Memory.** The next server may hold more than the first. ty reads the whole repository into memory: 12.3 GB on that repository. The slow server is stopped when it times out and is not started again while it is passed over, so the two do not run side by side for long.
- **Answers differ between servers.** ty does not count import statements as references, and pyright found one reference ty missed (see the notes below). A graph can hold answers from both: the ones pyright gave in time and kept, and the ones ty gave after.
- **Only a timeout is handed on.** A server that crashes, or answers with an error, is still reported, whatever the setting.
- **Only when there is a next server.** If the slow server is the last one installed for the language, its timeout is reported as before, and it is not passed over: the slowest answer is better than none. `lemma providers` shows what is installed.
- **Only once the next server has answered** is the slow one passed over, so a machine too busy for any server does not lose its first choice.

Lemma says when this happens. An answer names the server that gave it and the one it stood in for:

```console
$ lemma refs Client.get
#6d0a51c3e98f2b47 method     Client.get  api/client.py:41:9
via ty 0.0.98
in place of pyright: textDocument/references timed out after 59 seconds
```

JSON output lists the same under `slow_providers`. A graph adds a note, `ty answered in place of pyright: ...`, once however many of its symbols that was true for. `lemma daemon status` lists each server passed over, in which repository, and when it is tried again. Answers read from the cache name only the server that gave them.

The setting applies to every lookup: `refs`, `definition`, `implementations`, `signature` and `graph`, from the command line, the daemon and the MCP server. A command run in its own process (`LEMMA_NO_DAEMON=1`) passes a slow server over for the rest of that command. Changing any of the three settings, or restarting the daemon, has every server tried again.

If the first server is too slow for most questions in a repository, not a few, list the faster one in that repository's `.lemma/providers.toml` instead (see above): it is then tried first and nothing waits. `when_slow = "next"` suits the repository where the first server is right for most symbols and too slow for the widely used ones.

## Caching and configuration changes

Lemma caches answers per snapshot, provider, and provider configuration. Changing a provider's `command`, `args`, `initialization_options`, `settings` or `ready_messages` changes its fingerprint, so its old answers are no longer used and the next lookup asks the server again.

Upgrading a server binary without changing its configuration does not invalidate the cache. `lemma refs --refresh` asks again.

## Server notes

- **pyright** is tried first for Python. Compared with ty on a large repository, it found every reference ty did plus one ty missed (a classmethod called through an instance of a generic subclass of a metaclass-built class), and it counts import statements and `__all__` entries as references. It used less memory there (2.7 GB against 7.3 GB), but each lookup took seconds where ty took a fraction of one.
- **ty** comes next: much faster on large projects (on a 73,000-file repository a cold lookup takes about 2 seconds and a warm one under 20 milliseconds), but it reads the whole repository into memory and, like the TypeScript servers, does not count import statements as references. To prefer it, list it in `.lemma/providers.toml`, which is tried before the built-in order. Install it with `uv tool install ty` or `pip install ty`.
- **pyright and basedpyright** are configured to check only open files (`diagnosticMode = "openFilesOnly"`), which keeps start-up fast. Reference lookups still search the whole project, and pyright re-reads it for every lookup: on very large projects each lookup takes seconds even with a warm server. They count import statements as references. On a repository of a few thousand files, setting `diagnosticMode = "workspace"` makes lookups several times faster after a slower start (about a minute on django); on very large ones it does not help (see [Performance](performance.md#python-servers-measured)). Lemma waits for their "Found N source files" message before asking, since earlier answers miss most of the project. Pyright does not report its version, so output names it without one.
- **TypeScript servers** find the project through `tsconfig.json`. Imports from packages need `node_modules`. A TypeScript import statement is not counted as a reference to the imported symbol, only its uses are. Lemma waits for typescript-language-server's "Initializing JS/TS language features" progress to finish before asking.
- **gopls** works from the module containing the indexed files, so index from inside a directory with a `go.mod`. In a repository with several Go modules and no `go.work`, references from other modules are not found.
- **rust-analyzer** loads the Cargo workspace with `cargo metadata` and indexes it before it can answer; Lemma waits for its "Indexing" progress to finish, and asks again while it answers "content modified". On this repository a cold lookup takes about 5 seconds.
