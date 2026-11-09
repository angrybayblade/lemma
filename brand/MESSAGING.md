# Lemma messaging and launch copy

The active landing page uses **“Move fast. See what breaks.”** Agents using Lemma were more often correct and cheaper in the benchmark, not faster, so do not lead with agent speed. The product explanation and claim boundaries below still apply.

## Message order

1. **Benefit:** Move fast. See what breaks.
2. **What it does:** Catch breaking changes between services before code merges.
3. **Who it helps:** People and coding agents changing connected software.
4. **How:** Read repositories, check service contracts, and show affected code.
5. **Next step:** Get started or read the quickstart.

Aim for comprehension before technical depth. The first screen should need one headline, one short paragraph, and one action.

## Primary copy

**Headline**

> Move fast. See what breaks.

**Supporting line**

> Lemma checks how your services connect, so people and coding agents can catch breaking changes before code merges.

**Primary action:** Get started

**Secondary action:** Read the docs

**Product descriptor:** Breaking-change checks across services.

**Short brand line:** Build with clarity.

**Maturity label, when relevant:** Release candidate

## Short descriptions

**One sentence**

Lemma catches breaking changes between services before code merges.

**Repository or profile description**

Catch breaking changes across services. Code context for people and coding agents. CLI + MCP.

**Expanded description**

A service changes. Code in another repository still depends on it. Lemma checks those connections before the change merges and shows the affected code. Use it from the command line, CI, or a coding agent through MCP.

**Technical description**

Lemma reads repositories at specified commits, checks service contracts and declared requirements, and reports findings with affected code. It also queries symbols, references, definitions, and implementations through language servers, exposing its commands to agents over MCP.

## Campaign headlines

| Headline | Use |
| --- | --- |
| Move fast. See what breaks. | Main introduction; speed and clarity |
| Build with clarity. | Trust and product overview |
| Your change has company. | Expressive social post; explain the connection in its caption |
| One change. A wider view. | Relationship illustration |
| Confidence for the next change. | Supporting editorial statement |

Use the headline with the product descriptor. An expressive phrase alone does not explain the product.

## Ready-to-use launch post

**Short**

Introducing Lemma: catch breaking changes between services before you merge.

A change in one repository can break code in another. Lemma checks those connections and shows the affected code, for both people and coding agents.

Available as a release candidate, with a CLI and MCP tools.

Get started: https://github.com/angrybayblade/lemma

**Longer**

Your service's tests pass. The code that breaks might live in another repository.

Lemma checks the contracts between services before a change merges. It reads the repositories you specify and shows what the change breaks and where. People can inspect findings in the CLI; coding agents can use the same tools through MCP.

It also helps navigate code: find references, definitions, and implementations with language servers.

Lemma is in release candidates. The quickstart walks through a change in billing that breaks a caller in web.

Try it: https://github.com/angrybayblade/lemma

## Feature copy

- **See the connections.** Read the services and repositories involved in a change.
- **Catch the break.** Find affected callers before the change merges.
- **Follow the code.** Query symbols and references from the CLI or MCP.

## Explain with one example

Billing removes a response field. The web app still reads it. Lemma can identify that broken connection before the billing change merges.

This example follows the repository's quickstart. Use it as an illustration; do not present it as captured output from a user's system.

## Words to use carefully

“Fast” is an experience goal; numerical speed claims need a linked benchmark with its scope. “At any scale” describes the ambition, not a demonstrated guarantee. “Confidence” comes from clear findings and visible limits. “Safe” must never imply that all failures have been ruled out.

Prefer “finds affected code” to “understands your whole codebase.” Prefer “checks service contracts” to “proves your system correct.” Use “unknown,” “stale,” and “unsupported” when those are the actual outcomes.

Do not invent testimonials, adoption numbers, product availability, hosted features, prices, or a production-readiness claim. Entail remains a potential hosted product, separate from this Lemma launch copy.
