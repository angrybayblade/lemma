# Lemma

Lemma indexes source code into a shared, versioned store of symbols and asks language servers about them. This first iteration covers TypeScript, Python, and Go: syntax indexing with Tree-sitter, pluggable language servers for semantic answers, and reference lookups for people and agents.

## Build

Requires a Rust toolchain (1.85 or newer).

```sh
cargo build --release
# binary: target/release/lemma
```

## Use

```sh
lemma index ~/code/project              # writes ~/code/project/.lemma/index.db
cd ~/code/project
lemma symbols ToolRouter                # search by name or qualified name
lemma refs ToolRouter.delete            # references via the language's server
lemma refs delete --language python     # narrow an ambiguous name
lemma refs '#10610'                     # or pick by id from `lemma symbols`
lemma providers                         # which servers are installed and tried
```

Add `--json` to any command for machine-readable output. JSON positions are zero-based and columns count UTF-8 bytes; text output uses editor-style 1-based `line:column`.

`lemma refs` exits with `0` when the provider answered (including "no references"), `2` when the name matches several symbols, and `3` when no provider could answer.

### Reading results

Lemma keeps "no answer" separate from "no references":

- **no references found**: the provider answered with nothing. That covers only what it could see; it does not prove the symbol is unused.
- **stale**: the file changed since indexing. Run `lemma index` again.
- **no provider installed** or **lookup failed**: nothing was learned about the code.

Answers are cached per snapshot, provider, and provider configuration. Use `--refresh` to ask again.

## Language servers

Lemma does not bundle language servers. Built-in definitions, tried in order:

| Language | Providers |
| --- | --- |
| TypeScript / TSX | `typescript-language-server` (TypeScript ≤ 6), then `tsc --lsp` (TypeScript 7) |
| Python | `pyright-langserver`, then `basedpyright-langserver` |
| Go | `gopls` |

If a provider fails to start, the next installed one is tried and the skipped one is reported.

Servers resolve imports from the project's installed dependencies. Without them (for example, no `node_modules`), results can be incomplete.

### Adding or overriding a provider

Create `.lemma/providers.toml` in the indexed project, or pass `--providers <file>`. Entries are tried before built-ins and replace a built-in with the same `id`:

```toml
[[provider]]
id = "jedi"
languages = ["python"]
command = "jedi-language-server"
args = []

[provider.initialization_options]
diagnostics = { enable = false }

# Returned when the server asks for workspace/configuration.
[provider.settings.python]
analysis = { extraPaths = ["src"] }
```

## Layout

| Crate | Responsibility |
| --- | --- |
| `lemma-model` | Domain types: snapshots, symbols, references, outcomes |
| `lemma-syntax` | Tree-sitter extraction of definitions |
| `lemma-lsp` | Generic LSP client: framing, lifecycle, readiness, encodings |
| `lemma-providers` | Provider registry and the LSP-to-Lemma adapter |
| `lemma-store` | SQLite storage format |
| `lemma-index` | Walking, hashing, and snapshotting a directory |
| `lemma-query` | Symbol resolution and reference lookups |
| `lemma-cli` | The `lemma` command |

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions.
