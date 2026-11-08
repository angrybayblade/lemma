---
name: lemma-impact
description: Map what a change reaches before making it - every use of a symbol, every caller of an API operation in every repository, and every place a contract is declared - with Lemma's language-server answers instead of text search. Use before changing an operation, a route, a schema, a generated client or a shared function, or when asked "what uses this", "who calls this endpoint" or "what could this break".
---

# Map the impact

The point is the list a text search would get wrong: callers in other repositories that build the path at run time or call a generated client, a common name used for unrelated things, and an endpoint whose path shares a prefix with others. Lemma answers from each language's server and follows API operations from their handlers to their callers.

## Steps

1. **Name the target precisely.** An operation as method and path (`POST /v1/charges`), or a symbol by qualified name (`Router.delete`). Find it with `search_symbols` (or `lemma symbols`) and keep its id (`#415`, or `#billing:415` in a workspace).
2. **Uses in its own repository.** `references` on the symbol; `implementations` too for an interface member. Each answer carries the function it sits in.
3. **Callers in other repositories.** Run from the workspace directory (the one whose `lemma.yaml` lists the repositories):
   - `graph` on the operation's handler with `direction: callers` follows the route to its operation and on to the code that calls it, whether through a URL or a generated client.
   - `lemma check --links` lists every link Lemma infers between repositories, caller to operation.
   - For a generated client, `references` on its method for the operation.
4. **Where the contract is declared.** The route's schema, each committed spec (OpenAPI documents) and each generated client's types for the operation.
5. **Separate look-alikes.** Other operations whose paths share a prefix, internal routes that keep the old behavior, and same-named fields of other types are not on the list. Say why each was left off.
6. **Say what Lemma could not see.** "No references found" means the server saw none, not that nothing uses it; a warning about unseen files names where to look by hand. Check those with text search and say so.

## What to hand back

- **Target**: the operation or symbol, with its handler and schema.
- **Impact list**: each caller and each declaration, `file:line` and function, grouped by repository.
- **Left off**: look-alikes and why.
- **Unseen**: what Lemma could not answer for, and how you checked it.
