---
name: lemma-usage
description: Use Lemma to navigate and change code safely - find references, definitions, implementations and signatures through language servers instead of text search, and read `lemma verify` findings about what a change breaks in other repositories. Use whenever a task involves where a symbol is used, the impact of renaming or changing something, or checking a service change against its callers, in TypeScript, JavaScript, Python, Go or Rust.
---

# Using Lemma

Lemma answers code questions through each language's own server (references, definitions, implementations, signatures) and checks changes to HTTP contracts across repositories (`lemma verify`). It is installed as the `lemma` command and, when configured, as MCP tools.

## Choose the tool

| Question | Use | Why |
| --- | --- | --- |
| Where is this symbol used? What breaks if I change it? | `lemma refs <symbol>` | Semantic references: no matches in comments, strings or unrelated same-named symbols |
| Where is the thing used at this line defined? | `lemma definition <file>:<line>:<column>` | Follows imports and re-exports |
| What implements this interface or overrides this method? | `lemma implementations <symbol>` | Text search cannot tell implementations from mentions |
| What is this symbol's declaration and documentation? | `lemma signature <symbol>` | Header and the server's hover text, without opening files |
| What does this file define? | `lemma outline <file>` | Symbols in source order, nested |
| Which symbols are named like X? | `lemma symbols <name>` | Searches the index by name or qualified name |
| Does my service change break callers in other repositories? | `lemma verify <manifest> --base <commit> --head <commit>` | Checks every consumer the manifest lists |
| Strings, log messages, config keys, one-off lookups in small repositories | `rg` / grep | Text is the question |

Prefer Lemma's MCP tools when they are available (`references`, `definition`, `implementations`, `signature`, `outline`, `search_symbols`, `index`); they answer the same questions. Before deleting something "unused", cross-check with text search: "no references found" means the server saw none, not that nothing uses it dynamically.

## Name symbols precisely

- A name: `Router`. A qualified name: `Router.delete` (class or type, then member; Go methods by receiver type, Rust methods by impl type).
- An id from `lemma symbols`: `'#415'`.
- Narrow an ambiguous name with `--language python` or by qualifying it.
- `lemma refs` exits with 2 when the name matched several symbols: the output lists them; pick one.
- `lemma refs` exits with 3 when no language server could answer (stale file, server missing or failed): run `lemma providers` to see which servers are installed.

## Read `lemma verify` output

```text
change platform fa87ef98a7^..fa87ef98a7  (56208 facts, souffle engine)
error   sdk:ts/packages/core/src/models/SessionOptions.ts::SessionOptions.list calls GET /api/v3.1/session_options and uses `search`, a query parameter that was removed and that GET /api/v3.1/session_options now rejects (sdk:ts/packages/core/src/models/SessionOptions.ts:84)
        [consumer_sends_removed_parameter]
2 errors, 0 warnings, 228 informational (--all lists them)
```

- `error`: a request will fail. Exit code 4. Fix the consumer or keep the contract.
- `warning`: behaviour changes without failing (a response field the consumer reads is gone, or a field it sends is now ignored). Check by hand; warnings about very common field names (`id`, `slug`) can be wrong.
- `info`: worth knowing; hidden unless `--all`.
- Each finding names the requirement in brackets and the consumer as `repository:file::Function`, with `file:line`.

To check a finding, rerun with `--explain`: it prints the facts behind it, indented. Read from the bottom: input facts from specs and code. A wrong finding is usually one wrong fact there, such as a call matched to the wrong operation or a common name counted as a use.

To see what Lemma concluded, print a relation: `--relation calls` (which consumer calls which operation), `--relation removed_request_field`, `--relation uses`.

## When changing a service's API

1. Before editing: `lemma refs` on the handler or schema to see local uses.
2. After editing, with the change committed on a branch: `lemma verify <manifest> --base main --head HEAD`.
3. For each error: update the consumer in the same change, or keep the old field or parameter accepted.
4. For each warning: decide whether the consumer still works; say so in the pull request.

More detail: [reference.md](reference.md).
