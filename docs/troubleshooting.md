# Troubleshooting

## `no index found here or in any parent directory`

Run `lemma index <project>` first, then run other commands from inside that project, or pass `--db <project>/.lemma/index.db`.

## `no indexed symbol matches ...`

`lemma refs` matches names exactly. Search first with `lemma symbols <part of the name>`, then use the qualified name or `'#id'` it shows. If the symbol is missing altogether:

- check the file is a supported type and is not ignored by `.gitignore`;
- check whether `lemma index` reported syntax errors in that file;
- re-index if the symbol was added since the last run.

Only definitions are indexed: functions, methods, classes, interfaces, structs, enums, type aliases, and top-level constants and variables. Local variables and parameters are not.

## `matches N symbols`

Several definitions share the name. Narrow it with the qualified name (`Router.delete`), `--language`, `--kind`, `--path`, or pick one by id.

## `changed since it was indexed`

The symbol's file was edited after indexing. Run `lemma index` again.

## `no <language> provider is installed`

None of the configured commands for that language is on `PATH`. Install one (see [Getting started](getting-started.md#2-install-a-language-server)) and check with `lemma providers`. If the server is installed somewhere unusual, set its full path as `command` in `.lemma/providers.toml`.

## `lookup failed` or `skipped ...`

The message comes from the server or the process start-up. Common causes:

- **`Could not find a valid TypeScript installation`** from `typescript-language-server`: it needs TypeScript 6 or older, installed in the project or globally. With TypeScript 7, the `typescript-native` provider (`tsc --lsp`) is used instead.
- **Timed out**: large projects can take longer than 60 seconds to load the first time. Retry with `--timeout-secs 180`.
- **Server exited**: run the provider's command by hand to see its error output, for example `pyright-langserver --stdio`.

## `no references found`, but there are some

The server answered, but did not see the uses. Check that:

- the project's dependencies are installed, so the server can follow imports;
- the server is looking at the right project root, such as the directory with `tsconfig.json`, `pyproject.toml`, or `go.mod`. Index from that directory;
- the uses are not dynamic, such as `getattr(obj, "name")` or string-based dispatch, which no server can follow;
- for TypeScript, the "missing" ones are not import statements, which are not counted.

Then ask again with `lemma refs <symbol> --refresh`, because the earlier answer is cached.

## Results look out of date

Cached answers are reused until the project is re-indexed with changes or the provider's configuration changes. Use `--refresh` to bypass the cache for one lookup, or delete `.lemma/index.db` and index again to start fresh.
