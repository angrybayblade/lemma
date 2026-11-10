# Quickstart

Describe two repositories, break the contract between them, and run `lemma check` to catch the break. Allow about ten minutes.

Prerequisites: Git, and Lemma [installed](getting-started.md#1-install). Verify that both commands print a version:

```console
$ lemma --version
lemma [..]
$ lm --version
lm [..]
```

## 1. Create two repositories

`billing` serves an HTTP API described by an OpenAPI document. `web` calls that API from TypeScript. Create a directory for both:

```console
$ mkdir -p shop/billing shop/web/src
$ cd shop
```

Save billing's document as `billing/openapi.json`. The operation requires `customer` and returns `id` and `receipt_url`:

<!-- file: billing/openapi.json -->
```json
{
  "openapi": "3.0.0",
  "info": { "title": "Billing", "version": "1" },
  "paths": {
    "/v1/charges": {
      "post": {
        "operationId": "createCharge",
        "requestBody": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "customer": { "type": "string" } },
          "required": ["customer"]
        } } } },
        "responses": { "200": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "id": { "type": "string" }, "receipt_url": { "type": "string" } }
        } } } } }
      }
    }
  }
}
```

Save web's client as `web/src/charge.ts`. It sends `customer` and reads `receipt_url`:

<!-- file: web/src/charge.ts -->
```ts
export async function charge(customer: string): Promise<string> {
  const response = await fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify({ customer }),
  });
  const body = await response.json();
  return body.receipt_url;
}
```

## 2. Describe each repository

Give each repository a root module, `mod.lm`, named after the repository. Save `billing/mod.lm`:

<!-- file: billing/mod.lm -->
```lm
module billing

import * as http from lemma.http

export service api {
    operations: http.openapi("openapi.json")
}
```

This declares one service, `api`, and takes its operations from the OpenAPI document. `lemma.http` is the standard library's module for HTTP contracts.

Save `web/mod.lm`:

<!-- file: web/mod.lm -->
```lm
module web

export service web {
    index: { include: ["src"] }
}
```

This declares one service with its code under `src`. Lemma reads that code for calls to operations other services document, so do not list those calls yourself.

## 3. Compose them

Name the repositories to check together in a workspace module. Save `mod.lm` in `shop`:

<!-- file: mod.lm -->
```lm
module shop

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}
```

Run `lm check` to load the module and everything it names. It reports each mistake with its file, line and column:

```console
$ lm check
[..]; 0 errors
```

`lemma check` compares a change's commits, so initialize each repository in Git:

```console
$ git -C billing init -q
$ git -C billing add .
$ git -C billing commit -q -m "Billing API"
$ git -C web init -q
$ git -C web add .
$ git -C web commit -q -m "Charge customers"
```

Run `lemma check` to check the workspace as it stands. It reports no findings yet:

```console
$ lemma check
0 links, level check
  #billing       working directory
  #web           working directory
  self           working directory
0 errors, 0 warnings, 0 informational
```

## 4. Break the contract

Make billing require `currency` and stop returning `receipt_url`. Replace `billing/openapi.json` with:

<!-- file: billing/openapi.json -->
```json
{
  "openapi": "3.0.0",
  "info": { "title": "Billing", "version": "2" },
  "paths": {
    "/v1/charges": {
      "post": {
        "operationId": "createCharge",
        "requestBody": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "customer": { "type": "string" }, "currency": { "type": "string" } },
          "required": ["customer", "currency"]
        } } } },
        "responses": { "200": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "id": { "type": "string" } }
        } } } } }
      }
    }
  }
}
```

Commit the change:

```console
$ git -C billing commit -q -a -m "Require a currency; drop receipt_url"
```

## 5. Check the change

Pass the changed repository and its commits before and after:

```console
$ lemma check --change billing --base HEAD~1 --head HEAD
0 links, 1 inferred, level check
  #billing       [..]..[..]
  #web           working directory
  self           working directory
error   #web:web:src/charge.ts::charge calls POST /v1/charges, which now requires `currency`; its code never names `currency` (#web:web:src/charge.ts:2)
        [consumer_misses_new_required_field] #web:web:src/charge.ts::charge -> POST /v1/charges  (introduced)
warning #web:web:src/charge.ts::charge calls POST /v1/charges and names `receipt_url`; response field `receipt_url` was removed or renamed (#web:web:src/charge.ts:2)
        [consumer_uses_removed_response_field] #web:web:src/charge.ts::charge -> POST /v1/charges  (introduced)
1 errors, 1 warnings, 0 informational
```

Lemma read billing's document at both commits and found what changed: a newly required field and a removed response field. It read web's code and found one call to that operation, the "1 inferred" in the first line, from `charge` in `src/charge.ts`. Each finding names the requirement that found it, in brackets, and says whether the change introduced it.

The error fails the check: the command exits with 4. Gate merges on that exit code. At level `check` a newly required field is an error and a removed field a warning, because code that reads a field may cope without it.

## Next steps

You wrote no requirements: the checks came from `lemma.http`. To set up your own repositories, follow [Onboarding](onboarding.md). To learn the rest of Lemma, continue in this `shop` with the [tutorials](tutorials/README.md):

1. [Declare a link](tutorials/1-links.md) and check that web's types agree with billing's schemas.
2. [Write your own requirement](tutorials/2-requirements.md), and choose what runs with `check main`.
3. [Allow a known finding](tutorials/3-allowances.md), with a reason and an expiry date.
4. [Test a behavior](tutorials/4-behaviors.md) by running code through a checker (beta).
5. [Add a feature](tutorials/5-features.md), and check that what a change adds is served and called.
6. [Set up your editor](tutorials/6-editors.md) for highlighting, errors as you type and formatting.
