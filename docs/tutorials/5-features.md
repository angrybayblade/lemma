<!-- after: ../quickstart.md -->
# 5. Add a feature

A change that adds to an API breaks nothing, so the checks so far stay quiet even when a feature is half done: an operation in the spec that no code serves, or that no consumer calls yet. Lemma checks what a change adds, too.

This tutorial starts where the [quickstart](../quickstart.md) ends, in `shop`.

## Billing's routes

Lemma compares a spec with the routes that serve it when it can read them. Give billing an Express server for its one operation:

<!-- file: billing/src/server.ts -->
```ts
import express from "express";

const app = express();

app.post("/v1/charges", (req, res) => {
  res.json({ id: "ch_1" });
});

export default app;
```

and tell Lemma the service's code is under `src`. Replace `billing/mod.lm` with:

<!-- file: billing/mod.lm -->
```lm
module billing

import * as http from lemma.http

export service api {
    index: { include: ["src"] }
    operations: http.openapi("openapi.json")
}
```

```console
$ git -C billing add .
$ git -C billing commit -q -m "Serve charges"
```

## Specify refunds

The feature is refunds. Add the operation to billing's document; replace `billing/openapi.json` with:

<!-- file: billing/openapi.json -->
```json
{
  "openapi": "3.0.0",
  "info": { "title": "Billing", "version": "3" },
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
    },
    "/v1/charges/{id}/refunds": {
      "post": {
        "operationId": "refundCharge",
        "responses": { "200": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "id": { "type": "string" } }
        } } } } }
      }
    }
  }
}
```

```console
$ git -C billing commit -q -a -m "Specify refunds"
$ lemma check --change billing --base HEAD~1 --head HEAD
...
warning #billing:api adds POST /v1/charges/{}/refunds, which no code in the workspace calls; if code outside it does, add "POST /v1/charges/{}/refunds" to a graph of lemma.http.public_operation
        [operation_without_consumer] operation_without_consumer  (introduced)
warning #billing:api adds POST /v1/charges/{}/refunds to its spec, but no route in its code serves it
        [operation_without_route] operation_without_route  (introduced)
0 errors, 2 warnings, 0 informational
```

The spec promises refunds, but nothing serves them and nothing asks for them. Operations are named by method and path, with each path parameter written `{}`.

## Wire it up

Serve the route. Replace `billing/src/server.ts` with:

<!-- file: billing/src/server.ts -->
```ts
import express from "express";

const app = express();

app.post("/v1/charges", (req, res) => {
  res.json({ id: "ch_1" });
});

app.post("/v1/charges/:id/refunds", (req, res) => {
  res.json({ id: "re_1" });
});

export default app;
```

and call it from web, in a new file `web/src/refund.ts`:

<!-- file: web/src/refund.ts -->
```ts
export async function refund(charge: string): Promise<string> {
  const response = await fetch(`/v1/charges/${charge}/refunds`, { method: "POST" });
  const body = await response.json();
  return body.id;
}
```

Commit billing's route, and check both of billing's commits together, as a pull request would be checked. Web's call is in its working directory, which is what Lemma reads for a repository the change does not name:

```console
$ git -C billing commit -q -a -m "Serve refunds"
$ lemma check --change billing --base HEAD~2 --head HEAD
...
0 errors, 0 warnings, 0 informational
```

The feature is wired up: specified, served and called.

## A route the spec leaves out

The other way round counts as well. Add a route without specifying it; append to `billing/src/server.ts`:

<!-- append: billing/src/server.ts -->
```ts
app.delete("/v1/charges/:id", (req, res) => {
  res.sendStatus(204);
});
```

```console
$ git -C billing commit -q -a -m "Delete charges"
$ lemma check --change billing --base HEAD~1 --head HEAD
...
warning #billing:api adds a route for DELETE /v1/charges/{} (src/server.ts:15) that its spec does not declare
        [route_without_operation] route_without_operation  (introduced)
0 errors, 1 warnings, 0 informational
```

Only what a change adds is reported, so routes and operations that were already out of step stay out of the way.

## Operations called from outside

An operation that only callers outside the workspace use, such as one in a public API, has no consumer to find. State it in a graph and Lemma expects none:

```lm
import { public_operation, public_service } from lemma.http

graph public : public_operation {
    "POST /v1/charges/{}/refunds"
}
```

`public_service` does the same for every operation of a service, such as `"#billing:api"`.

Next: [set up your editor](6-editors.md).
