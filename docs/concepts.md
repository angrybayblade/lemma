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

The daemon goes further. It watches each indexed directory, so it knows which paths changed without walking the tree, and keeps the indexed files in memory:

- Before indexing it confirms that every change made so far has been reported, by writing a marker file to a watched directory and waiting for its event. If that fails, or the operating system dropped events, it walks everything.
- Only directories with changes are listed again, with the usual ignore rules. A changed `.gitignore` means a full walk.
- If nothing changed and the commit has not moved, the stored snapshot is returned without reading or writing anything.

On a 73,000-file repository a re-index with nothing changed takes about 32 ms, and one after a small edit about 85 ms. See [Performance](performance.md).

## Stale results

Positions in the index refer to the files as they were when indexed. Before asking a language server about a symbol, Lemma checks that the symbol's file still has the same contents. If it changed, the result is *stale* and no server is asked, because the stored position could point at the wrong code. Queries re-index before they run, as the MCP server does, so this only happens with an index given by `--db`.

Only the symbol's own file is checked. A server started for the lookup reads other files as they are on disk. A warm server sees edits to them once they are indexed, which every query does first.

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

## The daemon and warm language servers

Starting a language server and letting it load a large project takes seconds; asking one that has loaded takes milliseconds. So every command runs in a per-user background daemon, which keeps servers running between lookups. After `lemma index` it starts the server for each indexed language in the background, opening a file from the densest part of the tree so the server loads the project most lookups will need. A lookup that arrives while a server is still starting waits for it.

Before reusing a server, the daemon tells it which files were created, changed or deleted since its last lookup, using the differences between snapshots. Edits become visible to a warm server when you run `lemma index`. Files a server has open for earlier lookups are kept open, and updated or closed when they change.

The daemon also keeps every symbol name in memory for substring search, rebuilt in the background after each index.

The daemon keeps running, and looks after idle servers itself: a server nobody has used for 10 minutes is paused, so it uses no CPU and its memory can be compressed, and one idle for 2 hours is stopped. When files change in a project whose servers were stopped, the daemon starts them again, so they are usually ready by the next lookup. All of this is configurable; see [`lemma daemon`](commands.md#lemma-daemon). `lemma daemon stop` stops the daemon, and `--no-daemon` runs a command in-process.

## The index file

`.lemma/index.db` is a SQLite database. It holds the snapshot, files, symbols, cached answers, and each service's exports and links, and is safe to delete; `lemma index` recreates it. Indexing a workspace adds snapshots of the commits it pins to each repository's database, beside its working directory's, until retention drops them (see [`lemma index`](commands.md#lemma-index-path)); the workspace's own database records its members and their links. It contains absolute paths from your machine, so do not commit it.

## Limits of this version

- Supported languages are TypeScript, TSX, Python, Go, and Rust.
- The semantic questions are references, definitions, implementations, and hover text. Which of them a server supports varies; an unsupported question is reported as such.
- Lemma waits for a server to finish loading the project before asking. Servers that announce it (pyright, basedpyright, typescript-language-server) are waited for exactly; for others Lemma uses signals such as diagnostics and quiet periods. A server that is still loading when it answers can give an incomplete result; `--refresh` asks again.
- The daemon runs one command at a time; parallel calls queue.
- Language servers keep their memory while paused; the operating system reclaims it only under memory pressure. Set `memory_budget` or a shorter `stop_after` to bound it.
