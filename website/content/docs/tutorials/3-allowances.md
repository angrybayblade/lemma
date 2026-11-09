<!-- after: ../quickstart.md -->
# 3. Allow a known finding

Sometimes a finding is known and accepted for now: the client that reads a removed field is being retired, or a fix ships next week. An *allowance* keeps such a finding visible, says why it is accepted, and stops it failing the check, until a date you choose.

This tutorial starts where the [quickstart](../quickstart.md) ends, in `shop`.

## Allow it

The quickstart's change produced a warning: web reads `receipt_url`, which billing no longer returns. Say that's accepted while web moves off it. Replace `mod.lm` with:

<!-- file: mod.lm -->
```lm
module shop

import lemma.http

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}

allow lemma.http.consumer_uses_removed_response_field {
    when: op == "POST /v1/charges"
    reason: "web stops reading receipt_url in its next release"
    until: "2099-01-31"
}
```

- `allow` names the requirement whose findings it covers, here one of `lemma.http`'s, which the module imports to name it.
- `when` selects the findings, by the requirement's variables: `op` is the operation called. Leave it out to cover all of them.
- `reason` says why, and is shown with each covered finding.
- `until` is compared with the date of the commit being checked, not today's date, so the same commit always gives the same answer.

```console
$ lm check
[..]; 0 errors
$ lemma check --change billing --base HEAD~1 --head HEAD --all
...
info    #web:web:src/charge.ts::charge calls POST /v1/charges and names `receipt_url`; response field `receipt_url` was removed or renamed (#web:web:src/charge.ts:2) (allowed: web stops reading receipt_url in its next release)
...
1 errors, 0 warnings, 1 informational
```

The warning is now informational and says why; `--all` lists informational findings. The error, the missing `currency`, still fails the check: the allowance covers only the requirement it names.

## When an allowance expires

After `until`, the findings count again and Lemma says the allowance expired. Set a date in the past to see it; replace `mod.lm` with:

<!-- file: mod.lm -->
```lm
module shop

import lemma.http

workspace {
    repo billing = local("billing")
    repo web = local("web")
    config: { level: check }
}

allow lemma.http.consumer_uses_removed_response_field {
    when: op == "POST /v1/charges"
    reason: "web stops reading receipt_url in its next release"
    until: "2020-01-31"
}
```

```console
$ lemma check --change billing --base HEAD~1 --head HEAD
...
warning #web:web:src/charge.ts::charge calls POST /v1/charges and names `receipt_url`; response field `receipt_url` was removed or renamed (#web:web:src/charge.ts:2)
...
warning the allowance for lemma.http::consumer_uses_removed_response_field expired on 2020-01-31; the findings it covered count again
...
1 errors, 2 warnings, 0 informational
```

An allowance for a behavior that ran and covered none of its cases is reported too, so stale ones get removed. Behaviors are next: [test a behavior](4-behaviors.md).
