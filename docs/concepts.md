# How Lemma works

Lemma separates two kinds of facts about code:

- **What is defined where.** Lemma works this out itself by parsing each file with [Tree-sitter](https://tree-sitter.github.io/). This is fast, needs no setup, and always gives the same result for the same files.
- **What refers to what.** This depends on imports, types, and project configuration, so Lemma asks a language server. These answers are only as complete as the server's view of the project.

## Snapshots

`lemma index` records a *snapshot*: the indexed files, a hash of each file's contents, and the definitions found in them. A snapshot also records the Git commit and whether supported source files differ from it, but it is identified by its file contents, not by Git. Two runs over identical files produce the same snapshot fingerprint.

The index keeps one snapshot per indexed directory, and indexing again updates it in place.

## Re-indexing is incremental

Agents re-index after every edit, so a re-index costs roughly what changed, not the size of the repository:

- Files are parsed on every core.
- A file whose size, modification time, change time and inode are the same as last time is not read again. Files written while the previous index was running are always re-read, so an edit within the same timestamp tick cannot be missed.
- A file that was touched but has the same contents is not parsed again.
- Only new, changed and deleted files are rewritten in the index. Symbols in other files keep their ids.
- Git is asked about every source file only on the first index or after the commit changes. Otherwise it is asked only about the files that changed.

On a 73,000-file repository a re-index with nothing changed takes about 0.15 s, and one after a small edit about 0.2 s. See [Performance](performance.md).

## Stale results

Positions in the index refer to the files as they were when indexed. Before asking a language server about a symbol, Lemma checks that the symbol's file still has the same contents. If it changed, the result is *stale* and no server is asked, because the stored position could point at the wrong code. Run `lemma index` again to fix it.

Only the symbol's own file is checked. A server started for the lookup reads other files as they are on disk. A warm server sees edits to them once you run `lemma index`.

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

A cached answer is reused only when all of these match. Re-indexing after a content change, or changing a provider's configuration, makes the next lookup ask again: an edit in one file can change the references to a symbol defined in another, so every cached answer for the snapshot is dropped. Re-indexing with no content change keeps them. `--refresh` always asks again.

## Warm language servers

Starting a language server and letting it load a large project takes seconds; asking one that has loaded takes milliseconds. So the first `lemma refs` that needs a server starts a background daemon for the index database, and later lookups reuse its servers. Cached answers never involve the daemon.

Before reusing a server, the daemon tells it which files were created, changed or deleted since its last lookup, using the differences between snapshots. Edits become visible to a warm server when you run `lemma index`.

The daemon exits after 30 minutes without a lookup. `lemma daemon --stop` stops it, and `lemma refs --no-daemon` starts a server just for one lookup.

## The index file

`.lemma/index.db` is a SQLite database. It holds the snapshot, files, symbols, and cached answers, and is safe to delete; `lemma index` recreates it. It contains absolute paths from your machine, so do not commit it.

## Limits of this version

- Supported languages are TypeScript, TSX, Python, and Go.
- The only semantic question is "where is this symbol referenced".
- Lemma waits for a server to finish loading the project before asking. Servers that announce it (pyright, basedpyright, typescript-language-server) are waited for exactly; for others Lemma uses signals such as diagnostics and quiet periods. A server that is still loading when it answers can give an incomplete result; `--refresh` asks again.
- The daemon answers one lookup at a time; parallel calls queue.
- The daemon uses the environment, including `PATH`, of the command that started it.
