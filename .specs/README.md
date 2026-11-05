# Lemma specification notes

Captured: 2026-10-28

Updated 2026-11-04. The intended authoring surface is a modular `.lm` language. [Document 10](10-native-datastore.md) proposes a native primary datastore for agent workspaces, including versioned code facts, organisation-defined relations and verification evidence.

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
