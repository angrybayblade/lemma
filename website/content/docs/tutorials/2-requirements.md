<!-- after: ../quickstart.md -->
# 2. Write your own requirement

The checks in the quickstart came from `lemma.http`, the standard library's requirements for HTTP contracts. You can add your own: a rule over the facts Lemma knows about the system, reported as a finding wherever it matches. This tutorial adds one that warns about calls to a deprecated operation, then chooses what runs with `check main`.

It starts where the [quickstart](../quickstart.md) ends, in `shop`.

## Facts, rules and requirements

`lemma.http` declares relations, sets of facts with typed columns, that Lemma fills from documents and code. One of them is `calls(c, op, f, l)`: consumer `c` calls operation `op` from file `f`, line `l`. You can state facts of your own in a `graph`, and a requirement matches them together:

Replace `mod.lm` with:

<!-- file: mod.lm -->
```lm
module shop

import { calls } from lemma.http

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}

relation Deprecated(operation: Text)

graph deprecated : Deprecated {
    "POST /v1/charges"
}

require no_calls_to_deprecated_operations {
    severity: warning
    when: calls(c, op, f, l), Deprecated(op)
    message: "{c} calls {op}, which is deprecated ({f}:{l})"
}
```

- `relation Deprecated(operation: Text)` declares a relation with one column.
- `graph deprecated : Deprecated { ... }` states its facts, one per line.
- `require` turns a rule body into findings. `when:` holds when every part holds for the same values: a call, of an operation that is deprecated. Lowercase names (`c`, `op`, `f`, `l`) are variables; strings, numbers and uppercase names are constants.
- `message` fills in the variables, and `severity` is `error`, `warning` or `info`.

`lm check` checks the rule before anything runs. It knows `calls` has four columns, so `calls(c, op)` would be an error at its line:

```console
$ lm check
[..]; 0 errors
```

## Run it

Requirements over code run against a change, which says what Lemma reads. Check the quickstart's change again:

```console
$ lemma check --change billing --base HEAD~1 --head HEAD
0 links, 1 inferred, level check
...
warning #web:web:src/charge.ts::charge calls POST /v1/charges, which is deprecated (#web:web:src/charge.ts:2)
        [no_calls_to_deprecated_operations] #web:web:src/charge.ts::charge -> POST /v1/charges  (introduced)
...
1 errors, 2 warnings, 0 informational
```

Your requirement found web's call, beside the findings from `lemma.http`.

## Choose what runs

Without a `check main`, every requirement of every loaded module runs. A `check main` chooses, and can change a requirement's severity. Add to the end of `mod.lm`:

<!-- append: mod.lm -->
```lm
check main {
    run lemma.http.*
    run no_calls_to_deprecated_operations { severity: error }
    skip lemma.http.consumer_uses_removed_response_field
}
```

- `run lemma.http.*` runs every requirement in `lemma.http`.
- `run no_calls_to_deprecated_operations { severity: error }` runs yours as an error.
- `skip` drops one that `*` selected.

```console
$ lemma check --change billing --base HEAD~1 --head HEAD
0 links, 1 inferred, level check
...
error   #web:web:src/charge.ts::charge calls POST /v1/charges, which is deprecated (#web:web:src/charge.ts:2)
...
2 errors, 0 warnings, 0 informational
```

The deprecation is an error now, and the removed-field warning is gone. Keep `run lemma.http.*` in any `check main`: a check that names only your own requirements runs no HTTP contract check. Another check, such as `check nightly { ... }`, runs with `lemma check --select nightly`.

Next: [allow a known finding](3-allowances.md).
