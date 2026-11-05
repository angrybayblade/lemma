---
name: lemma-usage
description: Use Lemma to navigate and change code safely - find references, definitions, implementations and signatures through language servers instead of text search, and read `lemma verify` findings about what a change breaks in other repositories. Use whenever a task involves where a symbol is used, the impact of renaming or changing something, or checking a service change against its callers, in TypeScript, JavaScript, Python, Go or Rust.
---

# Using Lemma

Lemma answers code questions through each language's own server (references, definitions, implementations, signatures) and checks changes to HTTP contracts across repositories (`lemma verify`). It is installed as the `lemma` command and, when configured, as MCP tools.

## Choose the tool

| Question | Use | Why |
| --- | --- | --- |
| Where is this symbol used? What breaks if I change it? | `lemma refs <symbol>` | Semantic references: no matches in comments, strings or unrelated same-named symbols |
| Where is the thing used at this line defined? | `lemma definition <file>:<line>:<column>` | Follows imports and re-exports |
| What implements this interface or overrides this method? | `lemma implementations <symbol>` | Text search cannot tell implementations from mentions |
| What calls this, and what calls those? What does it depend on? | `lemma graph <symbol> --direction callers --depth 3` (or `callees`, `both`) | Follows references several levels; `--source names` answers instantly without a server, approximately; `--format mermaid` or `dot` to draw |
| Which endpoints, services and other repositories reach this function? | `lemma graph <symbol> --direction callers --workspace <workspace lemma.yaml>` | Continues from route handlers to their operations and to callers in other repositories; takes as long as `lemma check` |
| What is this symbol's declaration and documentation? | `lemma signature <symbol>` | Header and the server's hover text, without opening files |
| What does this file define? | `lemma outline <file>` | Symbols in source order, nested |
| Which symbols are named like X? | `lemma symbols <name>` | Searches the index by name or qualified name |
| Does my service change break callers in other repositories? | `lemma check lemma.yaml --change '#repo' --base <commit> --head <commit>`, or `lemma verify <manifest>` where the system uses a manifest | Checks declared and inferred links, compares schemas, and traces what a break affects |
| Strings, log messages, config keys, one-off lookups in small repositories | `rg` / grep | Text is the question |

Prefer Lemma's MCP tools when they are available (`references`, `definition`, `implementations`, `signature`, `outline`, `search_symbols`, `index`); they answer the same questions. Before deleting something "unused", cross-check with text search: "no references found" means the server saw none, not that nothing uses it dynamically.

## Name symbols precisely

- A name: `Router`. A qualified name: `Router.delete` (class or type, then member; Go methods by receiver type, Rust methods by impl type).
- An id from `lemma symbols`, as shown: `'#415'`, or `'#billing:415'` in a workspace, where each repository numbers its own symbols. An id survives edits to its file while the definition keeps its name and kind.
- An export's address from `lemma.yaml`: `services.api.createCharge` (a declared operation or an `operationId`), which resolves to the symbol implementing it.
- Narrow an ambiguous name with `--language python` or by qualifying it.
- `lemma refs` exits with 2 when the name matched several symbols: the output lists them; pick one.
- `lemma refs` exits with 3 when no language server could answer (server missing or failed; queries re-index first, so edited files are not stale): run `lemma providers` to see which servers are installed.

## Read `lemma verify` output

```text
change billing HEAD~1..HEAD  (61 facts, souffle engine)
error   web:src/invoices.ts::createInvoice calls POST /v1/invoices and names `memo`; request field `memo` was removed and POST /v1/invoices now rejects requests that send it (web:src/invoices.ts:4)
        [consumer_sends_rejected_field]
1 errors, 0 warnings, 0 informational
```

- `error`: a request will fail. Exit code 4. Fix the consumer or keep the contract.
- `warning`: behaviour changes without failing (a response field the consumer reads is gone, or a field it sends is now ignored). Check by hand; warnings about very common field names (`id`, `slug`) can be wrong.
- `info`: worth knowing; hidden unless `--all`.
- Each finding names the requirement in brackets and the consumer as `repository:file::Function`, with `file:line`.

To check a finding, rerun with `--explain`: it prints the facts behind it, indented. Read from the bottom: input facts from specs and code. A wrong finding is usually one wrong fact there, such as a call matched to the wrong operation or a common name counted as a use.

To see what Lemma concluded, print a relation: `--relation calls` (which consumer calls which operation), `--relation removed_request_field`, `--relation uses`.

## Read `lemma check` output

```text
error   request: the receiver requires `currency` at the root; the sender may leave it out
        [request] #web:services.web.create_charge -> #billing:services.api.create_charge  (introduced)
        affects 1 operation and 2 callers (traced by name):
          reached directly:
            #web:web: 1 operation
              POST /v1/checkout
                through src/checkout.go:Checkout -> src/charges.go:Charges.Create
          one call away:
            #storefront: 2 callers of 1 operation
              src/cart.ts::Cart.pay -> POST /v1/checkout
              src/quick-buy.ts::buy -> POST /v1/checkout
```

- `[link]`: an operation was removed or renamed, or its code anchor no longer resolves.
- `[request]`: the provider now requires or rejects something the caller sends or omits.
- `[response]`: the caller expects something the provider no longer sends, or reads it as another type.
- `[schema]`: a side's schema could not be produced; read the message (a resolver failed, a type moved).
- A bracketed requirement name, such as `[consumer_sends_rejected_field]`, is an inferred link, read as in `lemma verify`.
- `(introduced)` means the change caused it; `(already there)` findings are informational when a change is given.
- `affects` lists, by distance, the operations whose handlers reach the problem (with the calls from each handler down to it), the code elsewhere that calls them, the operations reached through that code, and so on; each group shows three, and `--all` lists them all. It is traced by name, so treat it as where to look, not as proof.

`--head` must be a commit: commit the change on a branch before checking it.

## When changing a service's API

1. Before editing: `lemma refs` on the handler or schema to see local uses.
2. After editing, with the change committed on a branch: `lemma check <parent lemma.yaml> --change '#<repo>' --base main --head HEAD` (or `lemma verify <manifest> --base main --head HEAD`).
3. For each error: update the consumer in the same change, or keep the old field or parameter accepted.
4. For each warning: decide whether the consumer still works; say so in the pull request.
5. For each `affects` entry in another repository: check whether it passes the changed field on to its own callers.

More detail: [reference.md](reference.md).
