---
name: lemma-audit
description: Audit a system's `.lm` description against its code - run `lemma verify`, then find services, documents, clients, calling code and cross-repository calls the description misses, and propose the edits that close each gap. Use when asked to audit, review, complete or fix `mod.lm` files, when `lemma check` finds fewer links than expected, or after adding a repository or service to a system Lemma checks.
---

# Auditing `.lm` descriptions

`lemma check` only sees what the description says. Inference finds HTTP calls to documented operations by itself; everything else (an undocumented API, a client it cannot recognise, a call over a queue or a function runtime) is a gap until someone declares it. Find those gaps, explain each one, and propose the smallest edit that closes it. Field meanings are in [the `.lm` reference](reference.md).

Work from evidence in the code and in `lemma verify` and `lemma check` output, not from what a system usually looks like. Present gaps one at a time, most consequential first, and change files only after the user agrees.

## 1. Run `lemma verify` first

Find the workspace's `mod.lm` (the one with a `workspace { repo ... }` block), and from its directory:

```sh
lm check
lemma verify --all --links
```

`lm check` reports what is invalid in a module with its line. `lemma verify` reports what no longer matches the code: fix these first, since they make everything below unreliable. Its findings name a location as `#repo:service`; read the description there.

| Finding | Edit |
| --- | --- |
| `include_matches_nothing` | Correct the pattern, or remove it |
| `document_missing`, `document_unreadable` | Correct the path, or remove the document from `operations` |
| `schema_unresolved` | Point `request`/`response` at a type or JSON path that exists |
| `schema_open` | Informational: point the schema at the type's definition if it matters |
| `runtime_missing` | Correct the `runtime`, or install what it names |
| `external_unpinned` | Pin a commit: `git(url: "...", rev: "...")` |
| `service_empty` | Give it an `index`, or delete it |
| `operation_not_documented` | Correct the link's end, or add the operation |
| `anchor_unresolved` | Correct the `implementation` |
| `anchor_uncalled` | Move the caller's `implementation` to the method the code calls |
| `previously_still_present` | Remove the `previously` claim, or finish the rename |
| `link_redundant` | Informational: the link can go; inference finds this call |

Then note the links, declared and inferred, and any `not looked at` repositories.

## 2. Look for what `lemma verify` cannot find yet

Check each item against the code. Record the file and line that shows the gap.

| Gap | How to find it | Edit |
| --- | --- | --- |
| A served API's documents are not exported | `git ls-files \| grep -iE 'openapi\|swagger' \| grep -E '\.(json\|ya?ml)$'` in each repository; compare with `operations` | Add `http.openapi("...")` to the serving service's `operations` |
| Code is not part of any service | Route directories (`pages/api`, `routes`, `handlers`, `router`), directories that build URLs or call clients, and shared libraries outside every service's `index.include` | Add them to the service they belong to |
| A client is not recognised | A client library whose calls resolve to nothing (`lemma verify --relation calls` lacks them): its service has no `client`, or its code is outside `index.include`; calling code that neither imports its package nor depends on it | Declare the service's `client: http.generated_client(...)` and include its code in `index.include`; calls through it are then found from its code. A client generated at build time needs nothing: it is recognised by its missing module |
| A generated client's own methods are reported as callers | Findings whose caller is a method of the client itself, duplicating `client_declares_removed_parameter` | Declare that service's `client`: its code is the client, so it is not searched for calls |
| Operations' handlers are guessed, or missing | `sqlite3 .lemma/index.db "SELECT found_by, count(*) FROM exports WHERE kind = 'endpoint' GROUP BY found_by"`; rows with no `path`, or found by `code`; read how their routes are written | Give a framework with an adapter an `operations` adapter, which places handlers where the routes are registered (`adapter`); otherwise name the route calls in `routes`: `register` for calls taking a route and its handler, `define` for calls only describing one |
| A cross-repository call inference cannot see | Calls not over documented HTTP: function runtimes (Lambda `Invoke`), queues and topics, RPC stubs, webhooks, shared databases | Declare an operation on each side with schemas, and a link in a graph of `Link` |
| A declared operation has no schema | Operations without `request`/`response` (or `sends`/`accepts`) | Point them at the types (`file.type("Name")`), a document, or a `json.command` |

For a call inference cannot see, find both ends: the code that sends (a client method, a publisher) and the code that receives (a handler, a consumer). Each end becomes an operation in its repository's `mod.lm`, and the link goes from the sender's operation to the receiver's.

## 3. Propose and apply

For each gap, show the user:

- what is missing, with the file and line that shows it;
- what `lemma check` misses because of it (calls unchecked, a break that would go unreported);
- the edit, as an `.lm` snippet.

After the user agrees, edit the file, run `lm check`, and run `lemma verify` again. A gap is closed when the numbers move as expected (links grow, an informational finding about an open type disappears) and no new findings appear.

## 4. Confirm on a real change

Replay a past change that crosses a gap you closed, or make one on a scratch branch (add a required field, remove a response field), and confirm `lemma check --change <repo> --base <commit>~1 --head <commit>` reports it. Delete the scratch branch afterwards.

## Report

End with a table: each gap, its evidence, the edit made or declined, and what `lemma verify` and `lemma check` reported before and after.
