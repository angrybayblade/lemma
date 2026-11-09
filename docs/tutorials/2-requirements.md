<!-- after: ../quickstart.md -->
# 2. Write your own requirement

The quickstart's checks came from `lemma.http`, the standard library's requirements for HTTP contracts. Write your own requirement when a property of your system is not covered there: a rule over the facts Lemma knows about the system, reported as a finding wherever it matches.

Prerequisite: the end state of the [quickstart](../quickstart.md), in `shop`. Result: a requirement that warns about calls to a deprecated operation, and a `check main` that chooses what runs.

## Write the requirement

`lemma.http` declares relations, sets of facts with typed columns, that Lemma fills from documents and code. One of them is `calls(c, op, f, l)`: consumer `c` calls operation `op` from file `f`, line `l`. State facts of your own in a `graph`, and match them together in a requirement.

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

Run `lm check` to check the rule before anything runs. It knows `calls` has four columns, so it reports `calls(c, op)` as an error at its line:

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

Without a `check main`, every requirement of every loaded module runs. Use a `check main` to choose what runs and to change a requirement's severity. Add to the end of `mod.lm`:

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

The deprecation is an error now, and the removed-field warning is gone. Keep `run lemma.http.*` in any `check main`: a check that names only your own requirements runs no HTTP contract check. Declare other checks, such as `check nightly { ... }`, and run them with `lemma check --select nightly`.

Next: [allow a known finding](3-allowances.md).
