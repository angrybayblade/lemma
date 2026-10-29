# Commands

```text
lemma [--db <file>] [--providers <file>] [--json] <command>
```

## Global options

These work with every command.

| Option | Meaning |
| --- | --- |
| `--db <file>` | Index database to use. By default, `lemma index` writes `<path>/.lemma/index.db`, and the other commands use the nearest `.lemma/index.db` in the current directory or a parent. |
| `--providers <file>` | Language server configuration to use instead of the project's `.lemma/providers.toml`. Built-in servers stay available. See [Language servers](providers.md). |
| `--json` | Print JSON instead of text. See [JSON output](json-output.md). |

## `lemma index [path]`

Reads the source files under `path` (default: the current directory) and records their definitions as a new snapshot.

- Supported files: `.ts`, `.mts`, `.cts`, `.tsx`, `.py`, `.pyi`, `.go`.
- Skipped: anything matched by `.gitignore`, hidden files and directories (including `.lemma/`), and `node_modules`.
- If no file changed since the last run, the existing snapshot is kept and reported as `unchanged`, and cached answers stay valid.
- Otherwise the new snapshot replaces the previous one for the same directory, and cached answers for it are dropped.

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

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. For `refs`, the server answered, including "no references found". |
| `1` | Error, such as no index, an unknown symbol, or an invalid configuration file. The message is printed to stderr. |
| `2` | `refs` only: the name matches several symbols. The candidates are printed. |
| `3` | `refs` only: no answer. The file is stale, no server is installed, the server failed, or it does not support references. |
