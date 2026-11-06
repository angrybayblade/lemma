# Introduction

Lemma checks the contracts between your services before a change merges, and answers questions about code for the people and agents changing it.

## The problem

In a system of several repositories, one repository's change breaks code in another. A service makes a request field required, renames a response field, or removes an operation. Its own tests pass, the type checker sees only its own repository, and the client that breaks lives somewhere else. The break shows up in staging, or in production.

Lemma reads every repository of the system, the changed one at its commits before and after the change and the others where you pin them, and reports what the change breaks, where, and what depends on that code further up.

## What Lemma checks

- **Calls found in code.** Lemma reads the services' OpenAPI documents and the code of the clients that call them, and reports a call the change breaks: an operation that no longer exists, a parameter or request field now required that the client never sends, a response field the client reads that is gone. Describing which repositories serve and call what is all it needs.
- **Links you declare.** A link names an operation that calls and the operation it calls. Lemma checks that both still exist, that their code anchors still resolve, and that the caller's types and the provider's schemas still agree, in both directions.
- **Requirements you write.** A requirement is a rule over facts about the system, such as "no code calls a deprecated operation". Lemma reports each match as a finding.
- **Behaviors you test.** A behavior runs statements that may ask a checker, a command that exercises your code, for evidence, such as "when billing says a failure may be retried, orders passes that on". Lemma runs the checker, keeps what it observed, and reuses it while the code is unchanged.

Known exceptions are allowed with a reason and, optionally, a date after which they count again.

## How you describe a system

Each repository describes itself in a module, `mod.lm`, at its root:

```lm
module billing

import * as http from lemma.http

export service api {
    operations: http.openapi("openapi.json")
}
```

A workspace module composes the repositories you check together:

```lm
module shop

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}
```

Modules are typed and checked: a misspelled operation, a link to something that does not exist, or a rule reading a relation that is not there is reported with its line before anything runs. Editors show those problems as you type.

## Two commands

| Command | For |
| --- | --- |
| `lm` | Writing modules: `lm check` checks them, `lm fmt` formats them, `lm convert` turns a `lemma.yaml` into one, and `lm lsp` serves them to editors. |
| `lemma` | Checking systems: `lemma check` checks a workspace and a change, and `lemma verify` checks a change against an explicit manifest. It also navigates code with language servers (`lemma refs`, `lemma definition` and more) and serves agents over MCP. |

## Words you will meet

| Word | Means |
| --- | --- |
| module | One `.lm` file, named by its place in its repository. A directory's module is its `mod.lm`. |
| service, operation | Something a repository runs, and what it serves or calls, with a request and a response. |
| link | A connection from an operation that calls to the operation it calls. |
| requirement | A property that must hold, written as a rule over facts. |
| behavior, checker, evidence | A property tested by running code; the command that runs it; what the command observed. |
| check | A named selection of requirements and behaviors to run; `check main` by default. |
| finding, outcome | One problem a check reports; whether a behavior was satisfied, violated, unknown or stale. |

## Where to go next

- [Quickstart](quickstart.md): two repositories, a breaking change, and Lemma catching it, in about ten minutes.
- [Tutorials](tutorials/README.md): links, your own requirements, allowances, behaviors, and editor setup.
- Reference: [the `.lm` language](language.md), [commands](commands.md), [the standard library](standard-library.md), and [spec 13](../.specs/13-lm-grammar.md) for the design.

Lemma is in release candidates; the [pre-release checklist](pre-release.md) says what is ready.
