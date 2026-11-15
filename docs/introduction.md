# Introduction

Lemma gives a coding agent what an IDE gives a person, without an editor open. It finds code through language servers, shows how code connects, and catches what a change breaks in the code that depends on it, including code in other repositories. Answers come fast enough to ask on every step of a task, not once at the end.

- **On your own,** your agent stays precise and trustworthy on long tasks: it looks code up instead of guessing from text search, and checks its work before it calls the work done.
- **In a team,** services, repositories and teams move fast without breaking each other: every change is checked against the code that depends on it, wherever that code lives.

## What an agent does with it

1. **Find code.** `lemma refs`, `lemma definition` and `lemma implementations` answer from the language server, so they skip other symbols with the same name and mentions in comments and strings, and follow imports and aliases. A cached answer takes 1 to 3 ms and a symbol's first lookup tens of milliseconds ([performance](performance.md)).
2. **Trace it.** `lemma graph <symbol>` shows what calls a function and what it calls, each with its file and line, across the repositories of a workspace.
3. **Catch a break.** `lemma check --change <repository>` reports what a change breaks in the code that depends on it: a response field a client still reads, a parameter now required that a caller never sends, an operation that no longer exists. Those breaks cross HTTP calls and generated clients, where the compiler cannot see them. A check reads only the code the change can reach, so it fits in the loop.
4. **Gate the work.** A stop gate runs the check when the agent tries to finish, and sends it back if its change introduced errors (the [`lemma-verify`](../skills/lemma-verify/SKILL.md) skill). [Behaviors](tutorials/4-behaviors.md) (beta) add checks that run the real code.

Agents reach all of it as MCP tools (`lemma mcp`), and the [agent skills](../skills/README.md) teach them when to use which.

## What `lemma check` checks

In a system of several repositories, one repository's change breaks code in another. A service makes a request field required, renames a response field, or removes an operation. Its own tests pass, the type checker sees only its own repository, and the client that breaks lives somewhere else. Lemma reads every repository of the system, the changed one at its commits before and after the change and the others where you pin them, and reports what the change breaks, where, and what depends on that code further up.

- **Calls found in code.** Lemma reads the services' OpenAPI documents and the code of the clients that call them, and reports a call the change breaks: an operation that no longer exists, a parameter or request field now required that the client never sends, a response field the client reads that is gone. Describing which repositories serve and call what is all it needs.
- **Links you declare.** A link names an operation that calls and the operation it calls. Lemma checks that both still exist, that their code anchors still resolve, and that the caller's types and the provider's schemas still agree, in both directions.
- **Requirements you write.** A requirement is a rule over facts about the system, such as "no code calls a deprecated operation". Lemma reports each match as a finding.
- **Behaviors you test (beta).** A behavior runs statements that may ask a checker, a command that exercises your code, for evidence, such as "when billing says a failure may be retried, orders passes that on". Lemma runs the checker, keeps what it observed, and reuses it while the code is unchanged.

An allowance allows a known finding with a reason and, optionally, a date after which it counts again.

## Describe your system

Describe each repository in a module, `mod.lm`, at its root:

```lm
module billing

import * as http from lemma.http

export service api {
    operations: http.openapi("openapi.json")
}
```

Compose the repositories you check together in a workspace module:

```lm
module shop

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}
```

Modules are typed. Lemma checks them before anything runs and reports a misspelled operation, a link to something that does not exist, or a rule reading a missing relation, with its line. Editors show those problems as you type.

## Commands

| Command | For |
| --- | --- |
| `lm` | Writing modules: `lm check` checks them, `lm fmt` formats them, and `lm lsp` serves them to editors. |
| `lemma` | Working on code: `lemma refs`, `lemma definition`, `lemma graph` and more find code with language servers; `lemma check` checks a change against the code that depends on it, and `lemma verify` checks that a description still matches the code. `lemma mcp` serves all of it to agents. |

## Terms

| Word | Means |
| --- | --- |
| module | One `.lm` file, named by its place in its repository. A directory's module is its `mod.lm`. |
| service, operation | Something a repository runs, and what it serves or calls, with a request and a response. |
| link | A relationship from an operation that calls to the operation it calls. |
| requirement | A property that must hold, written as a rule over facts. |
| behavior, checker, evidence | A property tested by running code; the command that runs it; what the command observed. |
| check | A named selection of requirements and behaviors to run; `check main` by default. |
| finding, outcome | One problem a check reports; whether a behavior was satisfied, violated, unknown or stale. |

## Next steps

- [Getting started](getting-started.md): install Lemma, connect your agent, and look up your first references, in about five minutes.
- [Quickstart](quickstart.md): two repositories, a breaking change, and Lemma catching it, in about ten minutes.
- [Onboarding](onboarding.md): set Lemma up for a team's repositories, from code navigation to checks in CI.
- [Tutorials](tutorials/README.md): links, your own requirements, allowances, behaviors, and editor setup.
- Reference: [the `.lm` language](language.md), [commands](commands.md), [the standard library](standard-library.md), and [spec 13](../.specs/13-lm-grammar.md) for the design.

Lemma is in release candidates; the [pre-release checklist](pre-release.md) says what is ready.
