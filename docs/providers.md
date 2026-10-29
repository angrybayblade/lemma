# Language servers

Lemma calls a language server a *provider*: a source of facts about code that Lemma cannot work out from syntax alone, such as where a symbol is used. Any server that speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) over stdio can be used.

## Built-in providers

| id | Languages | Command |
| --- | --- | --- |
| `typescript-language-server` | typescript, tsx | `typescript-language-server --stdio` |
| `typescript-native` | typescript, tsx | `tsc --lsp --stdio` |
| `ty` | python | `ty server` |
| `pyright` | python | `pyright-langserver --stdio` |
| `basedpyright` | python | `basedpyright-langserver --stdio` |
| `gopls` | go | `gopls` |

Install instructions are in [Getting started](getting-started.md#2-install-a-language-server).

`typescript-language-server` needs TypeScript 6 or older. TypeScript 7 replaced it with a native server built into `tsc`, which is the `typescript-native` provider. Whichever matches your TypeScript installation starts, and the other is skipped.

## How a provider is chosen

For each lookup, Lemma takes the providers configured for the symbol's language, keeps those whose command is on `PATH`, and tries them in order:

1. Project providers, in file order.
2. Built-in providers, in the order above.

If a provider fails to start, Lemma tries the next one and reports the skipped one. If a provider starts but then fails while answering, Lemma reports the failure instead of trying another. Switching quietly would hide a real problem, such as a broken project setup.

Run `lemma providers` to see the order for each language.

Starting a server on a large project can take several seconds, so servers are kept running by a background daemon after the first lookup (see [How Lemma works](concepts.md#warm-language-servers)). Each answer is also cached until the next content change.

## Adding or overriding a provider

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
| `languages` | yes | Languages it answers for: `typescript`, `tsx`, `python`, `go`. |
| `command` | yes | Executable name on `PATH`, or a path to it. |
| `args` | no | Arguments. Most servers need `--stdio`. |
| `initialization_options` | no | A table sent to the server when it starts, as LSP `initializationOptions`. Check the server's documentation for what it accepts. |
| `settings` | no | A table the server can read back through `workspace/configuration`. Write keys the way the server documents them, for example `[provider.settings.python.analysis]` for pyright's `python.analysis` section. |
| `ready_messages` | no | Patterns for a log message, or the title of a progress report that finishes, that the server sends once it has loaded the project. `*` matches any text. Lemma waits for one before asking, instead of guessing from diagnostics and quiet periods. If none arrives within 10 seconds, a quiet spell counts instead. |

Unknown fields are rejected, so a typo such as `comand` causes an error instead of being ignored.

`--providers` replaces the project file; it does not add to it. Built-in providers are always available.

## Caching and configuration changes

Answers are cached per snapshot, provider, and provider configuration. Changing a provider's `command`, `args`, `initialization_options`, `settings` or `ready_messages` changes its fingerprint, so its old answers are no longer used and the next lookup asks the server again.

Upgrading a server binary without changing its configuration does not invalidate the cache. Use `lemma refs --refresh` to ask again.

## Notes on specific servers

- **ty** is tried first for Python because it is much faster on large projects: on a 73,000-file repository a cold lookup takes about 2 seconds and a warm one under 20 milliseconds. Like the TypeScript servers, it does not count import statements as references. Install it with `uv tool install ty` or `pip install ty`.
- **pyright and basedpyright** are configured to check only open files (`diagnosticMode = "openFilesOnly"`), which keeps start-up fast. Reference lookups still search the whole project, and pyright re-reads it for every lookup: on very large projects each lookup takes seconds even with a warm server. They count import statements as references. Lemma waits for their "Found N source files" message before asking, since earlier answers miss most of the project. Pyright does not report its version, so output shows `unknown version`.
- **TypeScript servers** find the project through `tsconfig.json`. Imports from packages need `node_modules`. A TypeScript import statement is not counted as a reference to the imported symbol, only its uses are. Lemma waits for typescript-language-server's "Initializing JS/TS language features" progress to finish before asking.
- **gopls** works from the module containing the indexed files, so index from inside a directory with a `go.mod`. In a repository with several Go modules and no `go.work`, references from other modules are not found.
