---
name: lemma-audit
description: Audit a system's composable lemma.yaml files against its code - find services, documents, clients, calling code and cross-repository calls the files miss or describe wrongly, and propose the edits that close each gap. Use when asked to audit, review, complete or fix lemma.yaml files, when `lemma check` finds fewer links than expected, or after adding a repository or service to a system Lemma checks.
---

# Auditing lemma.yaml files

`lemma check` only sees what the `lemma.yaml` files describe. Inference finds HTTP calls to documented operations by itself; everything else (an undocumented API, a client it cannot recognise, a call over a queue or a function runtime) is a gap until someone declares it. Find those gaps, explain each one, and propose the smallest edit that closes it. Field meanings are in [the lemma.yaml reference](reference.md).

Work from evidence in the code and in `lemma check` output, not from what a system usually looks like. Present gaps one at a time, most consequential first, and change files only after the user agrees.

## 1. Read the system

1. Find the parent `lemma.yaml` (the one composing others with `#name` services) and every repository it reaches.
2. Run the check as it stands:

   ```sh
   lemma check <parent> --change '<#repo>' --base <commit>~1 --head <commit> --all --json > check.json
   ```

   Pick a recent commit in a repository that serves an API, so inference runs. Note `links`, `inferred_links`, and the informational findings that say a schema uses types it could not follow.

## 2. Look for gaps

Check each item against the code. Record the file and line that shows the gap.

| Gap | How to find it | Edit |
| --- | --- | --- |
| A served API's documents are not exported | `git ls-files \| grep -iE 'openapi\|swagger' \| grep -E '\.(json\|ya?ml)$'` in each repository; compare with `exports.openapi` | Add them to the serving service's `exports.openapi` |
| Code is not part of any service | Route directories (`pages/api`, `routes`, `handlers`, `router`), directories that build URLs or call clients, and shared libraries outside every service's `index.include` | Add them to the service they belong to |
| A client is not recognised | A client library whose calls resolve to nothing (`lemma verify --relation calls` lacks them): its service has no `exports.client`, or its code is outside `index.include`; consumers that neither import its package nor depend on it | Declare the client's `exports.client` and include its code in `index.include`; calls through it are then found from its code. A client generated at build time needs nothing: it is recognised by its missing module |
| A generated client's own methods are reported as callers | Findings whose caller is a method of the client itself, duplicating `client_declares_removed_parameter` | Declare that service's `exports.client`: its code is the client, so it is not searched for calls |
| Endpoints' handlers are guessed, or missing | `sqlite3 .lemma/lemma.db "SELECT found_by, count(*) FROM exports WHERE kind = 'endpoint' GROUP BY found_by"`; endpoints with no `path`, or found by `code`; read how their routes are written | Give a framework with an adapter `exports.operations_from`, which places handlers where the routes are registered (`adapter`); otherwise name the route calls in `exports.routes`: `register` for calls taking a route and its handler, `define` for calls only describing one |
| A cross-repository call inference cannot see | Calls not over documented HTTP: function runtimes (Lambda `Invoke`), queues and topics, RPC stubs, webhooks, shared databases | Declare an operation on each side with schemas, and a link in `graph.links` |
| A declared operation's schema is missing or open | Operations without `schema`; informational findings about types that could not be followed | Point `request`/`response` at the types (`file#Type`), or a resolver command |
| A link points nowhere | `link` findings: an operation not declared, or an anchor that does not resolve | Fix the address or the `path` |
| An external service is unpinned | `#name` services with `git` and no `ref` | Pin a commit in the parent |

For a call inference cannot see, find both ends: the code that sends (a client method, a publisher) and the code that receives (a handler, a consumer). Each end becomes an operation in its repository's `lemma.yaml`, and the link goes from the sender's operation to the receiver's.

## 3. Propose and apply

For each gap, show the user:

- what is missing, with the file and line that shows it;
- what `lemma check` misses because of it (calls unchecked, a break that would go unreported);
- the edit, as a YAML snippet.

After the user agrees, edit the file and run `lemma check` again. A gap is closed when the numbers move as expected (`links` or `inferred_links` grow, an informational finding about an open type disappears) and no new `link` findings appear.

## 4. Confirm on a real change

Replay a past change that crosses a gap you closed, or make one on a scratch branch (add a required field, remove a response field), and confirm `lemma check` reports it. Delete the scratch branch afterwards.

## Report

End with a table: each gap, its evidence, the edit made or declined, and what `lemma check` reported before and after.
