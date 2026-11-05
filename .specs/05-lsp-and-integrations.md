# Language server and integrations

Status: feasibility assessment and proposed architecture. No language server has been implemented.

This document concerns Lemma's **editor-facing** language server. The accepted starting architecture also uses Lemma as an **LSP client** to existing language-specific servers for semantic enrichment; see [Pluggable semantic providers and lean indexing](07-semantic-providers-and-indexing.md). These are separate interfaces over the same core model.

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
