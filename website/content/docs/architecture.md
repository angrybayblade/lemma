# Architecture

Lemma answers questions about code: where a symbol is defined, used and implemented, and what it looks like. Its code has four layers, so each can change, or get faster, without affecting the others.

```text
 consumption   lemma CLI, its daemon, and the MCP server for agents
      │        one request/response shape, printed as text or JSON
      ▼
 query         the operations: symbols, outline, references, definition,
      │        implementations, signature
      ▼
 storage  ◄──► symbol generation
 FactStore      SemanticProvider (language servers) and the syntax index
 (index.ls)     (Tree-sitter)
```

## Operations

Every interface offers the same six questions, from spec 07's normalized operations:

| Operation | CLI | Answered by |
| --- | --- | --- |
| Search symbols by name | `lemma symbols` | Syntax index |
| Symbols in a file | `lemma outline` | Syntax index |
| Where a symbol is used | `lemma refs` | Provider |
| Where the symbol at a position is defined | `lemma definition` | Provider |
| What implements or overrides a symbol | `lemma implementations` | Provider |
| A symbol's declaration and description | `lemma signature` | Syntax index (declaration) and provider (hover text) |

Provider answers share one shape: an outcome with a `state` (`found`, `empty`, `unsupported`, `provider_unavailable`, `stale`, `provider_failed`), plus the provider that answered, whether the answer was stored, and providers skipped on the way. "Empty" means the provider looked and found nothing. It is never used for "could not ask". See [JSON output](json-output.md).

## Layers

### Consumption

`lemma-cli` turns commands into operations and prints their answers. The daemon (`daemon.rs`) and the MCP server (`mcp.rs`) deliver the same commands: the daemon keeps state warm between them, and the MCP server exposes each operation as a tool whose result is the command's text output, which carries the same facts and ids as its JSON in a fraction of the size. Output shapes are defined in `output/`, separately from internal types, so internals can change without breaking scripts or agents.

### Query

`lemma-query` holds the operations. Syntax-only operations read the store. Semantic ones go through one path (`lookup.rs`):

1. If the file changed since it was indexed, answer `stale`. Stored positions would point at the wrong code.
2. If no provider for the language is installed, answer `provider_unavailable`.
3. If a stored answer matches the snapshot, provider and configuration, return it.
4. Otherwise ask the installed providers in order. Skip one that cannot start, report one that fails, and store a definitive answer.

Each kind of question implements a small `Ask` trait: how to ask a provider, and how to load and save its answer. A new question means a new `Ask`, not a new path. The query layer depends only on the `FactStore` and `SemanticProvider` traits.

### Storage

`lemma-store` defines `FactStore`: the facts the query layer reads and records. These are symbols and files from the syntax index, and provider answers keyed by question, position, provider and configuration. `Store` implements it over `.lemma/index.ls`: each source's files and symbols in immutable segments, a full one with every file and small deltas with the files later commits changed, beside an attribute table for what is not about files (exports, answers, and what checks keep). Symbol ids are digests of each symbol's identity, so nothing is stored to keep them. The design is in `.specs/17-lemma-store.md`.

Writing snapshots (`replace_snapshot`, `update_snapshot`) stays on `Store` itself. Only the indexer writes snapshots, and each write is one commit of the store: a reader sees all of it or none of it.

### Symbol generation

Two producers fill the store:

- **Syntax** (`lemma-syntax`): Tree-sitter extracts definitions and their nesting, and gives a definition's declaration text without its body. It needs no server, so it always works.
- **Semantic providers** (`lemma-providers`): the `SemanticProvider` trait asks for locations (references, definitions, implementations) or display text (hover) at a position. `ProviderSession` implements it over a language server via the generic client in `lemma-lsp`. Other producers, such as a batch indexer, a resolver used as a library, or a SCIP import, can implement the same trait.

`lemma-index` walks and watches directories and decides which files to re-parse. `lemma-model` holds the types every layer shares and depends on nothing else.

## Optimization points

| Layer | Room to grow |
| --- | --- |
| Consumption | Batched requests, streaming large answers, several commands at once in the daemon |
| Query | Graph questions (callers of callers, impact of a change) over stored answers, picking the cheapest source per question |
| Storage | Memory-mapped tables, answers invalidated per file instead of per snapshot, a shared map across repositories |
| Symbol generation | Filling answers ahead of time while idle, batch producers for languages where language servers are slow |
