# JSON output

Add `--json` to any command for output meant for scripts and agents. The shapes below are the output contract. New fields may be added; existing fields keep their meaning.

## Positions

JSON positions differ from text output:

- `line` and `column` count from **0**.
- `column` counts **UTF-8 bytes** from the start of the line, not characters.
- A `span` runs from `start` (inclusive) to `end` (exclusive).
- `path` is relative to the indexed directory, with `/` separators, unless it is outside that directory (then it is absolute).

```json
{ "start": { "line": 0, "column": 4 }, "end": { "line": 0, "column": 9 } }
```

## Symbol

Used by `symbols`, `refs`, and ambiguous results.

```json
{
  "id": 1,
  "name": "greet",
  "qualified_name": "greet",
  "kind": "function",
  "language": "python",
  "path": "lib.py",
  "span": { "start": { "line": 0, "column": 0 }, "end": { "line": 1, "column": 23 } },
  "name_span": { "start": { "line": 0, "column": 4 }, "end": { "line": 0, "column": 9 } }
}
```

`span` covers the whole definition, `name_span` only its name. `kind` is one of `function`, `method`, `class`, `interface`, `struct`, `enum`, `type_alias`, `constant`, `variable`.

## `lemma --json index`

```json
{
  "database": "./.lemma/index.db",
  "snapshot_id": 1,
  "root": "/Users/me/code/my-project",
  "revision": "4f2c1e9a...",
  "is_dirty": false,
  "fingerprint": "a8bef98b...",
  "is_unchanged": false,
  "files_by_language": { "python": 2 },
  "symbol_count": 1,
  "files_with_syntax_errors": [],
  "unreadable_files": []
}
```

`revision` is `null` outside a Git repository. `is_dirty` is `true` when supported source files differ from `revision`, including untracked ones. `is_unchanged` is `true` when no file's contents changed since the previous index.

## `lemma --json symbols`

An array of [symbols](#symbol), possibly empty.

## `lemma --json outline`

```json
{ "path": "src/router.py", "symbols": [ { "id": 412, "qualified_name": "Router", "...": "..." } ] }
```

`symbols` holds [symbols](#symbol) in source order.

## `lemma --json refs`

```json
{
  "symbol": { "id": 1, "name": "greet", "...": "..." },
  "provenance": {
    "provider_id": "pyright",
    "provider_version": null,
    "config_fingerprint": "fb4121bd..."
  },
  "from_cache": false,
  "skipped_providers": [],
  "outcome": {
    "state": "found",
    "references": [
      {
        "path": "app.py",
        "span": { "start": { "line": 1, "column": 6 }, "end": { "line": 1, "column": 11 } },
        "is_external": false,
        "enclosing_symbol": "main"
      }
    ]
  }
}
```

| Field | Meaning |
| --- | --- |
| `provenance` | Which server answered. `null` when none did. `provider_version` is `null` if the server did not report one. |
| `from_cache` | Whether this answer came from the cache. |
| `skipped_providers` | Servers that failed to start before one did, as `"<id>: <reason>"`. |
| `outcome.state` | One of the states below. |

References are sorted by path and position. `enclosing_symbol` is the qualified name of the innermost indexed definition containing the reference, or `null`. `is_external` is `true` for references outside the indexed directory.

### Outcome states

| `state` | Extra fields | Exit code |
| --- | --- | --- |
| `found` | `references` | 0 |
| `empty` | | 0 |
| `unsupported` | | 3 |
| `stale` | | 3 |
| `provider_unavailable` | `candidates`: provider ids configured for the language | 3 |
| `provider_failed` | `message` | 3 |

Treat only `found` and `empty` as answers. `empty` means the server found nothing it could see, not that the symbol is unused.

Every command that asks a language server (`refs`, `definition`, `implementations`, and the `hover` part of `signature`) shares this shape: `provenance`, `from_cache`, `skipped_providers`, and an `outcome` whose `state` is one of those above. When the state is `found`, the answer is under a field named for it: `references`, `definitions`, `implementations`, or `text`.

### Ambiguous names

When the name matches several symbols, `refs`, `implementations` and `signature` print this and exit with 2:

```json
{
  "state": "ambiguous",
  "query": "delete",
  "candidates": [ { "id": 415, "qualified_name": "Router.delete", "...": "..." } ]
}
```

Pick one and run `lemma --json refs '#415'`.

## `lemma --json definition`

```json
{
  "path": "main.py",
  "position": { "line": 2, "column": 6 },
  "provenance": { "provider_id": "pyright", "...": "..." },
  "from_cache": false,
  "skipped_providers": [],
  "outcome": {
    "state": "found",
    "definitions": [
      {
        "path": "shapes.py",
        "span": { "start": { "line": 17, "column": 4 }, "end": { "line": 17, "column": 14 } },
        "is_external": false,
        "symbol": { "id": 6, "qualified_name": "total_area", "...": "..." }
      }
    ]
  }
}
```

`position` is the zero-based position asked about (the command line counts from 1). Each definition's `symbol` is the indexed symbol whose name is at that location, or `null`, as for definitions in installed packages (`is_external: true`).

## `lemma --json implementations`

The same shape as `refs`, with `outcome.implementations` holding entries like `definitions` above.

## `lemma --json signature`

```json
{
  "symbol": { "id": 5, "qualified_name": "Square.area", "...": "..." },
  "declaration": "def area(self) -> float:",
  "hover": {
    "provenance": { "provider_id": "pyright", "...": "..." },
    "from_cache": false,
    "skipped_providers": [],
    "outcome": { "state": "found", "text": "```python\n(method) def area(self: Self@Square) -> float\n```" }
  }
}
```

`declaration` is `null` when the file changed since it was indexed. `hover.outcome.text` is the server's display text, usually Markdown.

## `lemma --json providers`

```json
[
  {
    "id": "pyright",
    "languages": ["python"],
    "command": "pyright-langserver",
    "executable": "/usr/local/bin/pyright-langserver"
  }
]
```

`executable` is `null` when the command is not on `PATH`. Entries are in the order they are tried.

## Errors

Errors such as a missing index or an unknown symbol are printed as text on stderr, even with `--json`, and the exit code is `1`.
