# How Lemma works

Lemma separates two kinds of facts about code:

- **What is defined where.** Lemma works this out itself by parsing each file with [Tree-sitter](https://tree-sitter.github.io/). This is fast, needs no setup, and always gives the same result for the same files.
- **What refers to what.** This depends on imports, types, and project configuration, so Lemma asks a language server. These answers are only as complete as the server's view of the project.

## Snapshots

`lemma index` records a *snapshot*: the indexed files, a hash of each file's contents, and the definitions found in them. A snapshot also records the Git commit and whether there were uncommitted changes, but it is identified by its file contents, not by Git. Two runs over identical files produce the same snapshot fingerprint.

The index keeps one snapshot per indexed directory. Indexing again after a change replaces it.

## Stale results

Positions in the index refer to the files as they were when indexed. Before asking a language server about a symbol, Lemma checks that the symbol's file still has the same contents. If it changed, the result is *stale* and no server is asked, because the stored position could point at the wrong code. Run `lemma index` again to fix it.

Only the symbol's own file is checked. If other files changed since indexing, the server sees the current versions, and their reference positions are current too.

## "No references" is not "unused"

Lemma keeps four situations apart:

| Situation | What it tells you |
| --- | --- |
| References found | The server found these places. There may be more it cannot see, such as dynamic calls, reflection, or code outside the project. |
| No references found | The server looked and found nothing within its view. |
| Unsupported | The server cannot answer this kind of question. |
| No answer (stale, no server, server failed) | Nothing was learned about the code. |

Only the first three are real answers from a server, and only those are cached. A failed lookup is never stored as "no references", so it cannot later be mistaken for one.

## Caching

Each answer is stored with:

- the snapshot it was computed against;
- the provider that produced it, and the version the server reported;
- a fingerprint of the provider's configuration;
- whether the declaration was included.

A cached answer is reused only when all of these match. Re-indexing after a change, or changing a provider's configuration, makes the next lookup ask again. `--refresh` always asks again.

## The index file

`.lemma/index.db` is a SQLite database. It holds the snapshot, files, symbols, and cached answers, and is safe to delete; `lemma index` recreates it. It contains absolute paths from your machine, so do not commit it.

## Limits of this version

- Supported languages are TypeScript, TSX, Python, and Go.
- The only semantic question is "where is this symbol referenced".
- A language server is started for each lookup that is not cached, rather than kept running.
- Lemma waits for a server to finish loading the project before asking, using signals the server sends. A server that is still loading when it answers can give an incomplete result; `--refresh` asks again.
