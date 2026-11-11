# Lemma specification notes

Captured: 2026-10-28

Updated 2026-11-10: [document 17](17-behavior-harnesses.md) proposes running behaviors in any language. Lemma core would define one interface for running a case, and harnesses would come in language packs that users install as they need them. It is parked with its open questions.

Updated 2026-11-08: [document 14](14-sentry.md) proposes sentry. The daemon, attached to one workspace, listens to file changes and keeps that workspace's check inputs prepared, so a check costs what the change costs. It records the decisions taken so far, the prior art reviewed, and a rollout gated on matching fresh checks.

Updated 2026-11-08: [document 15](15-one-description-format.md) removed Lemma's YAML formats, so a description is always `.lm`. [Document 16](16-verifying-descriptions.md) gave `lemma verify` a new job, checking that a description still matches the code; its first rollout step is implemented, and the rest is proposal.

Updated 2026-11-05: [document 13](13-lm-grammar.md) records the proposed `.lm` grammar and verification semantics (draft 0.2, revised 2026-11-06): modules named by their place in a repository, discovered and hand-written operations, typed `->` graphs, Datalog rules over a change's base and head, relational `require` and evidence-backed `behavior` requirements, and checkers. It is a design draft with no implementation changes. [Document 12](12-segments-and-shared-storage.md) proposes Lemma's storage format, segments keyed by Git tree that move between local machines, devboxes and CI. [Document 10](10-native-datastore.md), proposed 2026-11-04, describes a native primary datastore for agent workspaces, including versioned code facts, organisation-defined relations and verification evidence.

These notes consolidate the design discussion leading to Lemma. They include accepted architectural directions, research, experiments, proposals, and open questions. The accepted starting direction is a Rust core, a lightweight syntax index, pluggable language-server semantic providers, and Lemma-owned relationship semantics. Since 2026-11-01: rules are Datalog, evaluated by Soufflé first behind a pluggable engine interface, and relationships are declared in a user-definable grammar written in YAML or `.ls` files (see document 8). Since 2026-11-02: each repository has a composable `lemma.yaml` (services and a `graph` of links), parents compose repositories, and linked operations' schemas are compared as JSON Schema (see document 9).

## Naming

- **Lemma**: the open-source framework for defining, querying, and verifying software relationships.
- **Entail**: a potential hosted product built on Lemma, providing managed infrastructure and team workflows.
- “Entail, powered by Lemma” is the proposed relationship between the brands.
- Naming availability, package names, domains, and trademarks have not been checked.
- Earlier naming exploration included Mnemosyne and Seshat. Investigating the existing KSDaemon/seshat project led to a source review and a local generic platform pilot.

## Documents

1. [Vision and scope](01-vision-and-scope.md)
2. [Graph, relationships, and verification](02-graph-and-verification.md)
3. [Reusable languages and projects](03-foundations-and-research.md)
4. [Seshat review and generic platform pilot](04-seshat-and-platform-pilot.md)
5. [Language server and integrations](05-lsp-and-integrations.md)
6. [Proposed first milestone and open decisions](06-roadmap-and-open-decisions.md)
7. [Pluggable semantic providers and lean indexing](07-semantic-providers-and-indexing.md)
8. [Relational grammar and verification](08-relational-grammar-and-verification.md)
9. [Composable manifests, links and schema checks](09-composable-manifests.md)
10. [Native index and evidence store](10-native-datastore.md)
11. [Change-driven evaluation](11-change-driven-evaluation.md)
12. [Segments and shared storage](12-segments-and-shared-storage.md)
13. [The `.lm` grammar and verification semantics](13-lm-grammar.md)
14. [Sentry: warm paths for checks](14-sentry.md)
15. [One description format: removing YAML](15-one-description-format.md)
16. [`lemma verify`: does the description match the code?](16-verifying-descriptions.md)
17. [Behavior harnesses and language packs](17-behavior-harnesses.md)

## Status conventions

- **Established direction**: reflects the user's stated objective or accepted framing.
- **Proposal**: a suggested design that still requires evaluation.
- **Observed**: supported by the source review or pilot described here.
- **Unmeasured**: a hypothesis, estimate, or capability not exercised by the pilot.

## Central idea

Local symbol graphs compose into a versioned graph of software relationships. Engineers can declare relationships and requirements that source analysis cannot reliably infer. Rules over that graph select task context, identify affected consumers, and derive verification obligations. Check results provide evidence for specific revisions.

The same graph and rule engine should serve engineers in editors, AI agents through tools, and CI through reproducible validation.

## Scope of this record

This captures the substantive discussion and available research findings. Early requests for a broad survey of code-intelligence projects were part of the conversation; no additional project evaluations are invented here where detailed findings were not retained. The researched foundations and Seshat experiment documented below have concrete sources or tool outputs behind them.
