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
| A served API's documents are not listed | `git ls-files \| grep -iE 'openapi\|swagger' \| grep -E '\.(json\|ya?ml)$'` in each repository; compare with `openapi:` | Add them to the serving service's `openapi` |
| Route code is not listed | Route directories (`pages/api`, `routes`, `handlers`, `router`) not in `routes:` | Add to `routes` |
| Calling code is not searched | A service with several services in its repository and no `code:`; directories that build URLs or call clients outside `code:` | Add to `code` |
| A generated client is not recognised | Operation tables, `openapi-ts` or `openapi-generator` in build scripts, a `generated/` directory; calls through a client object | Add a `client` (`generated_from`, `call_names`, `specs`), and `client_receivers` or `client_modules` on the code calling it |
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
