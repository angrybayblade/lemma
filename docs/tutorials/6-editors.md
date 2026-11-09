# 6. Set up your editor

Set up your editor to get feedback as you write `.lm` modules: highlighting, mistakes underlined as you type (saved or not), the type or signature on hover, go-to-definition into other repositories and the standard library, an outline of the file's declarations, completion of the names in scope and after a `.` the members of a module or service, and formatting in the one standard layout.

All of it comes from `lm lsp`, a language server installed with `lm`. Every editor below runs that command.

## VS Code and Cursor

The extension is in Lemma's repository. Build and install it:

```sh
cd editors/vscode
npm ci
npm run package
code --install-extension lemma-lm-0.1.0.vsix      # or: cursor --install-extension ...
```

Or use **Extensions → ⋯ → Install from VSIX…** and pick the `.vsix`.

Open the quickstart's `shop` and then `billing/mod.lm`, and confirm the status bar's language reads **Lemma**. If the extension cannot start `lm`, which happens when an editor started from the dock does not see your shell's `PATH`, set the full path in your settings:

```json
"lemma.lmPath": "/Users/you/.cargo/bin/lm"
```

After installing a new `lm`, run **Lemma: Restart Language Server** from the command palette. Set `"lemma.trace.server": "verbose"` to see the messages between the editor and the server in the **Lemma** output panel.

## Neovim, Helix and Zed

Follow the setup for each in [editors/README.md](../../editors/README.md): a Tree-sitter grammar for highlighting, folding and indentation, and the language server configuration. Neovim needs 0.11 or later.

## Try it

In `billing/mod.lm`, misspell `openapi` as `openap`. The editor underlines it:

```text
module `lemma.http` exports no `openap`
```

Hover `api` in `web/mod.lm` after the [links tutorial](1-links.md) to see billing's service, or go to its definition to open `billing/mod.lm`. Run **Format Document**, or `lm fmt` from a terminal:

```sh
lm fmt            # formats every .lm file under this directory
lm fmt --check    # lists files that are not formatted, for CI
```

## Run in CI

Run these three commands in your pipeline:

```sh
lm check mod.lm                                                # every module loads and checks
lm fmt --check                                                 # every module is formatted
lemma check mod.lm --change billing --base "$BASE" --head "$HEAD"   # the change breaks nothing
```

`lemma check` exits with 4 when a finding is an error, and `lm check` and `lm fmt --check` with 1 when they find a problem.

Continue with the reference: [the `.lm` language](../language.md), [commands](../commands.md) and [the standard library](../standard-library.md).
