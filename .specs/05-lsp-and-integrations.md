# Language server and integrations

Status: feasibility assessment and proposed architecture, written before either surface existed. Both now exist in a first form: `lm lsp` serves `.lm` modules ([spec 13](13-lm-grammar.md)), and `lemma lsp` is an experimental companion for source code, described under [Change diagnostics](#change-diagnostics-experimental). The sections after it are the original assessment.

This document concerns Lemma's **editor-facing** language server. The accepted starting architecture also uses Lemma as an **LSP client** to existing language-specific servers for semantic enrichment; see [Pluggable semantic providers and lean indexing](07-semantic-providers-and-indexing.md). These are separate interfaces over the same core model.

## Change diagnostics (experimental)

**Status: experimental, 2026-11-14.** `lemma lsp` is the second surface below in its smallest useful form: it shows what saved changes break, where they break it, in any repository of the workspace.

**What it does.**

- It watches every repository of the workspace with the watcher sentry uses ([spec 14](14-sentry.md)) and keeps what it read between checks, in its own process. It does not need the daemon.
- When a repository's changes settle, it checks that repository as `lemma check --change` would, against the same default base, so a branch's commits and uncommitted work are checked together.
- Each finding the change introduced becomes a diagnostic on the line of the code it names, in whichever repository that is. Each file the working directory changed gets a summary, linking to those lines outside it. Findings the change did not introduce are not shown.
- A repository that has uncommitted changes when the server starts is checked once, after any change that settles meanwhile.
- `lemma.checkOnChange.enabled` in the VS Code extension runs it. It is off by default, since it runs checks in the background.

**Placing a finding.** A finding names its code through the variables its requirement binds: the standard library's requirements bind the calling code's file as `f` (`#repository:service:path`, the path relative to the repository) and its line as `l`. A finding without them goes on the changed files. A requirement written with other names is placed the same way, until findings carry a location of their own.

**What it does not do yet.**

- Unsaved edits. Checks read files from disk, so it follows saves, by an editor or by an agent, not typing.
- Unwatched repositories. It does not ask Git about a repository it could not watch, as an ask to sentry does.
- Cancellation. A check runs to the end; a change made meanwhile gets a check of its own afterwards.

**Found while building it.** An adapter's document was kept under the state the working directory was in when the adapter started. A file saved while the adapter read the code was then in a document kept for the state before it: for a clean working directory, the commit's own key. Later checks compared the change against that document and found nothing. Adapters now keep a document only if the working directory's state is unchanged after the run. Separately, the adapter cache is keyed by the runtime as checks served from a long-lived process describe it, which holds the caller's environment, so the editor's server and the command line do not share kept documents.

## Two surfaces

1. A language server for relationship/rule declarations: completion, symbol linking, navigation, invalid-reference diagnostics, and rule explanations.
2. A graph-aware companion for TypeScript, Python, and Go: dependency context, affected consumers, and verification information beside source code.

The first is simpler. The second depends on the graph remaining correct and useful during active editing across repositories.

## Standard LSP mapping

| User experience | Mechanism | Graph requirement |
| --- | --- | --- |
| Complete a symbol in a declaration | Completion | Scoped symbol search |
| Navigate from a declaration to code | Definition/document links | Resolvable URI and source range |
| Show contracts and evidence | Hover | Concise context and freshness metadata |
| Flag invalid relationships or constraints | Diagnostics | Source location, violation details, related locations |
| Show consumer/check counts above a symbol | CodeLens | Dependency counts and outstanding obligations |
| Run required checks | Code actions/commands | Check selection and runner integration |

Arbitrary relations such as `must_match` should not be presented as ordinary function calls merely to fit call hierarchy UI. A relationship explorer or interactive impact view likely needs custom protocol methods and editor-specific UI.

## Framework candidate

[Langium](https://langium.org/docs/features/) supplies grammar-driven parsing, typed ASTs, linking, workspace management, and many language-server features. It is a candidate if Lemma adopts a custom declaration language.

Its resolver would need to consult Lemma's graph for external code symbols. Langium does not automatically resolve TypeScript/Python/Go symbols merely because it manages declaration-language references.

If YAML/JSON remains the authoring format, the implementation choice should be revisited rather than adopting a custom-language framework unnecessarily.

## Coexistence with existing tooling

The proposed server supplements existing language tooling. Type checking, ordinary completion, and language-specific refactoring remain with those tools. Provider coexistence, duplicate diagnostics, and presentation must be tested in each target editor.

LSP is an editor protocol, not a standardized way to federate other language servers or a substitute for an analysis engine.

## Hard parts

- **Unsaved edits:** committed graph state differs from open document contents. Support temporary overlays or clearly label stale results.
- **Revision composition:** multiple repositories/branches need an explicit combined snapshot.
- **Location mapping:** map stable symbol identity to current URI/ranges, respecting negotiated position encoding.
- **Incremental invalidation:** recompute affected facts and evidence efficiently.
- **Responsiveness:** cancellation, asynchronous checks, stale-response suppression, and bounded result sets.
- **Unavailable repositories:** navigate to accessible source or clearly report missing content/revisions.
- **Refactoring:** cross-repository rename touches code, declarations, and contracts; defer until correctness and transactional behavior are established.

## Proposed architecture

```text
Indexers + declarations + check results
                 |
     Versioned graph and rule engine
                 |
      Shared query / validation API
           /         |         \
         LSP     Agent tools    CI
```

Editor, agent, and CI adapters should return consistent explanations and verification outcomes for the same inputs.

## Preliminary estimates

Assuming an already working graph API and one experienced engineer:

| Scope | Discussion estimate |
| --- | --- |
| One-editor hover/navigation/basic-diagnostic demo | 1–2 weeks |
| Useful MVP with declarations, CodeLens, diagnostics, check commands | 4–8 weeks |
| Robust multi-repository editing, overlays, freshness, cancellation, packaging | Several months |

These are planning estimates, not measured delivery commitments. A batch-only graph backend could make live-editor support a substantial new subsystem.

## References

- [LSP 3.17 specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)
- [VS Code language-server guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)
- [Langium features](https://langium.org/docs/features/)
