<!-- after: ../quickstart.md -->
# 4. Test a behavior

Some properties are not in any schema. *When billing declines a charge and says the failure may be retried, orders passes that on to its own caller* is about what code does, so Lemma has to run it. A *behavior* says what should hold; a *checker* is a command that runs your code and reports what it observed, its *evidence*; Lemma runs the checker for each case, keeps the evidence, and reuses it while the code is unchanged.

This tutorial adds a Python service, `orders`, to the [quickstart's](../quickstart.md) `shop`. You need `python3`.

## A service that calls billing

Orders charges the customer through billing. When the charge fails, it answers its caller with a 502, but it forgets billing's `retriable` flag:

```console
$ mkdir -p orders/orders orders/tools
$ touch orders/orders/__init__.py
```

<!-- file: orders/orders/billing.py -->
```python
def charge(customer):
    raise RuntimeError("calls the billing service")
```

<!-- file: orders/orders/handlers.py -->
```python
from orders import billing


def place_order(order):
    result = billing.charge(order["customer"])
    if "error" in result:
        return {"status": 502, "body": {"error": result["error"]}}
    return {"status": 201, "body": {"id": result["id"]}}
```

## The checker

A checker is any command. Lemma writes its arguments to the JSON file `LEMMA_INPUT` names and runs it in its repository; it writes what it observed to the file `LEMMA_OUTPUT` names. This one imports orders' handler, replaces the call to billing with a declined charge, calls the handler, and reports billing's answer and the handler's response. It finds the code to run from its arguments: each operation comes with its file and symbol.

<!-- file: orders/tools/replay.py -->
```python
"""Replays an order whose charge billing declines."""
import importlib
import json
import os
import sys

given = json.load(open(os.environ["LEMMA_INPUT"]))
arguments = given["arguments"]
handler = arguments["handler"]["implementation"]
call = arguments["through"]["values"]["caller"]["implementation"]
sys.path.insert(0, given["root"])


def module(path):
    return importlib.import_module(path.removesuffix(".py").replace("/", "."))


declined = {"error": "card declined", "retriable": arguments["retriable"]}
setattr(module(call["file"]), call["symbol"], lambda customer: declined)
response = getattr(module(handler["file"]), handler["symbol"])({"customer": "c_1"})

with open(os.environ["LEMMA_OUTPUT"], "w") as output:
    json.dump({"observed": {"billing": declined, "response": response}}, output)
```

## The behavior

Save `orders/mod.lm`:

<!-- file: orders/mod.lm -->
```lm
module orders

import * as python from lemma.python
import { Link } from lemma.links
import { api } from billing

repo billing = local("../billing")

let billing_client = python.file("orders/billing.py")
let handlers = python.file("orders/handlers.py")

export service orders {
    index: { include: ["orders"] }

    client operation charge {
        implementation: billing_client.symbol("charge")
    }

    export operation place_order {
        implementation: handlers.symbol("place_order")
    }
}

export graph links : Link {
    orders.charge -> api.createCharge
}

export checker replay_declined_charge(
    handler: Operation,
    through: Fact<Link>,
    retriable: Bool
): Evidence<Json> {
    command: ["python3", "tools/replay.py"]
    timeout_ms: 60000
}

export behavior passes_on_retriable {
    let link = links[orders.charge, api.createCharge]
    expect link.exists

    for retriable in [true, false] {
        let trace = need replay_declined_charge(
            handler: orders.place_order,
            through: link,
            retriable: retriable
        )
        expect trace.response.status == 502
        expect trace.response.body.retriable == retriable
            because "clients retry a declined order only when billing says they may"
    }
}
```

- The `checker` declares the command and what it takes: the handler to run, the link whose call it replaces, and the flag billing answers with. It returns `Evidence<Json>`.
- In the behavior, `links[orders.charge, api.createCharge]` looks up the link fact; `expect link.exists` makes sure it is there, so deleting the link fails the behavior rather than skipping it.
- Each iteration of `for` is a *case*, `retriable=true` and `retriable=false`. `need` asks the checker for evidence; without evidence, the case is unknown.
- A failed `expect` ends its case and is reported with the values it compared and the `because` text. Reading a field the evidence lacks gives a *missing* value, and the `expect` says which field is missing.

Add orders to the workspace; replace `mod.lm` with:

<!-- file: mod.lm -->
```lm
module shop

workspace {
    repo billing = local("billing")
    repo web = local("web")
    repo orders = local("orders")
    config: { level: check }
}
```

Then check the modules and commit orders. Git should ignore what Python writes while the checker runs, or every run would change the repository's state:

<!-- file: orders/.gitignore -->
```text
__pycache__/
```

```console
$ lm check
[..]1 behaviors, 1 checkers, 0 allowances; 0 errors
$ git -C orders init -q
$ git -C orders add .
$ git -C orders commit -q -m "Orders"
```

## Run it

`lemma check` runs the behaviors with the rest of the checks:

```console
$ lemma check
1 links, level check
...
behavior orders::passes_on_retriable: violated (1 of 3 cases held, 0 of 2 evidence reused)
error   `trace.response.body.retriable == retriable`: `retriable` is missing (orders/mod.lm:48); trace.response.body.retriable = missing `retriable`, retriable = false; clients retry a declined order only when billing says they may
        [orders::passes_on_retriable] orders::passes_on_retriable[retriable=false]  (introduced)
error   `trace.response.body.retriable == retriable`: `retriable` is missing (orders/mod.lm:48); trace.response.body.retriable = missing `retriable`, retriable = true; clients retry a declined order only when billing says they may
        [orders::passes_on_retriable] orders::passes_on_retriable[retriable=true]  (introduced)
2 errors, 0 warnings, 0 informational
```

The behavior found the bug. Of its three cases, the root (the link exists) held, and both replays failed: the handler's response has no `retriable`.

## Fix it

Pass the flag on. Replace `orders/orders/handlers.py` with:

<!-- file: orders/orders/handlers.py -->
```python
from orders import billing


def place_order(order):
    result = billing.charge(order["customer"])
    if "error" in result:
        body = {"error": result["error"], "retriable": result["retriable"]}
        return {"status": 502, "body": body}
    return {"status": 201, "body": {"id": result["id"]}}
```

```console
$ lemma check
1 links, level check
...
behavior orders::passes_on_retriable: satisfied (3 of 3 cases held, 0 of 2 evidence reused)
0 errors, 0 warnings, 0 informational
```

## Evidence is kept

Lemma keeps each piece of evidence under a key made of the checker's declaration, its arguments, the state of the code it reads and its environment. Run the check again, and nothing runs:

```console
$ lemma check
...
behavior orders::passes_on_retriable: satisfied (3 of 3 cases held, 2 of 2 evidence reused)
...
```

When the code changes, the key changes and the checker runs again. `--no-run` forbids running checkers, as in a CI job that should only use what is kept; then the evidence is reported as stale:

```console
$ echo "# reviewed" >> orders/orders/handlers.py
$ lemma check --no-run
...
behavior orders::passes_on_retriable: stale (1 of 3 cases held, 0 of 2 evidence reused)
...
0 errors, 2 warnings, 0 informational
```

At level `check`, only violated behaviors fail. At level `strict` (`config: { level: strict }` or `--level strict`), unknown and stale ones fail too.

Next: [add a feature](5-features.md).
