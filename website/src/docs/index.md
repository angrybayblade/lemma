# Documentation

Connect your agent. Model your system. Check the next change.

## Start with a working example

The quickstart walks through two repositories and a breaking change. Describe the system, see the finding, and make the next change with evidence.

- **[Quickstart](quickstart.md)**: build a small workspace in about ten minutes.
- **[Introduction](introduction.md)**: understand what Lemma checks and how the pieces fit.
- **[Team setup](onboarding.md)**: connect your repositories, agents, and CI.

## Follow the development loop

1. **Model the system.** Describe services and the operations they provide or call in `.lm` modules.
2. **Trace the change.** Follow callers, contracts, and requirements across the repositories you declare.
3. **Exercise the behavior.** Run declared cases through your checkers and capture what happened.
4. **Check what must hold.** Give your agent findings and evidence for its next iteration.

The [tutorials](tutorials/README.md) take you through links, requirements, allowances, and behavior checks using runnable examples.

## Connect a coding agent

Run Lemma as an MCP server:

```sh
lemma mcp
```

Add that command to your agent's MCP configuration. [Agent skills](../skills/README.md) teach the workflows; [code navigation](getting-started.md) covers definitions, references, and implementations.

## Keep the reference close

| When you need to… | Read |
| --- | --- |
| Write a module or a requirement | [The .lm language](language.md) |
| Look up a built-in function | [Standard library](standard-library.md) |
| Check a change across repositories | [Checking links](check.md) |
| See whether a description still matches the code | [Verifying a description](verify.md) |
| Find a command or an output field | [CLI commands](commands.md) · [JSON output](json-output.md) |
| Understand an unexpected result | [Troubleshooting](troubleshooting.md) |

Lemma is in release candidates. [Release readiness](pre-release.md) describes the current scope and limitations.
