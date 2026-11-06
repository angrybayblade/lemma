<!-- after: ../quickstart.md -->
# 1. Declare a link

In the quickstart, Lemma found web's call to billing by reading web's code. That needs no setup, but it only knows what the code names. A *link* says exactly which operation calls which, and lets Lemma compare the caller's own types with the provider's schemas, field by field and in both directions.

This tutorial starts where the [quickstart](../quickstart.md) ends, in `shop`.

## Give the client types

Web's request and response get TypeScript types. Replace `web/src/charge.ts` with:

<!-- file: web/src/charge.ts -->
```ts
export interface ChargeRequest {
  customer: string;
}

export interface ChargeResponse {
  id: string;
  receipt_url: string;
}

export async function charge(customer: string): Promise<string> {
  const request: ChargeRequest = { customer };
  const response = await fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify(request),
  });
  const body: ChargeResponse = await response.json();
  return body.receipt_url;
}
```

## Declare the call and the link

Replace `web/mod.lm` with:

<!-- file: web/mod.lm -->
```lm
module web

import * as typescript from lemma.typescript
import { Link } from lemma.links
import { api } from billing

repo billing = local("../billing")

let charges = typescript.file("src/charge.ts")

export service web {
    index: { include: ["src"] }

    client operation create_charge {
        implementation: charges.symbol("charge")
        sends: charges.type("ChargeRequest")
        accepts: charges.type("ChargeResponse")
    }
}

export graph links : Link {
    web.create_charge -> api.createCharge
}
```

What each part says:

- `import { api } from billing` brings in billing's service. Imports name modules, and a repository's root module is named after it.
- `repo billing = local("../billing")` says where billing is when `web` is checked on its own. In the workspace, the workspace's own `repo` declaration wins.
- `client operation create_charge` is web's outbound call. Its `implementation` is the function that makes it, and `sends` and `accepts` are the types it sends and reads.
- `graph links : Link { ... }` states one fact of the standard library's `Link` relation: web's `create_charge` calls billing's `createCharge`, the operation billing's document names with that `operationId`.

`lm check` confirms the names resolve. A typo in `createCharge` would be an error here, with its line:

```console
$ lm check web/mod.lm
[..]; 0 errors
```

Commit web's change:

```console
$ git -C web commit -q -a -m "Declare the charge call"
```

## Check the change again

```console
$ lemma check --change billing --base HEAD~1 --head HEAD
1 links, 1 inferred, level check
  #billing       [..]..[..]
  #web           working directory
  self           working directory
error   request: the receiver requires `currency` at the root; the sender may leave it out
        [request] #web:services.web.create_charge -> #billing:services.api.createCharge  (introduced)
error   response: the receiver expects `receipt_url` at the root; the sender does not send it
        [response] #web:services.web.create_charge -> #billing:services.api.createCharge  (introduced)
...
3 errors, 1 warnings, 3 informational (--all lists them)
```

The two new errors come from the link. Lemma resolved `ChargeRequest` and `ChargeResponse` from web's code and compared them with billing's schemas at the change's head:

- **request:** billing, the receiver, now requires `currency`, and web's `ChargeRequest` may leave it out.
- **response:** web, the receiver of the response, expects `receipt_url`, which billing no longer sends.

Both are marked *introduced*: at the base commit the two sides agreed. The findings from web's code are still there too, below; `--all` also lists the differences that break nothing.

A link is checked even when no code calls the operation yet, and it fails when either end disappears: delete `createCharge` from billing's document and the link reports that the provider operation no longer exists.

## Naming an operation by its route

An operation without an `operationId` can be named by its method and path, path parameters written in braces:

```lm
export graph links : Link {
    web.create_charge -> api["POST /v1/charges"]
}
```

Next: [write your own requirement](2-requirements.md).
