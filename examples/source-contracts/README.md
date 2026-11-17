# One contract, three implementations

[mod.lm](mod.lm) applies one contract to [TypeScript](handler.ts), [Python](handler.py) and [Go](handler.go) handler bodies. The enclosing application supplies the persistence dependency. Its assumed lease semantics are explicit in `commit.assumption`.

```sh
cargo build -p lemma-cli --bin lemma
LEMMA_SMT_SOLVER=/absolute/path/to/z3 ./target/debug/lemma check examples/source-contracts/mod.lm --no-daemon --level strict
```

Run from the repository root. Expected result: all three cases satisfy the behavior under the listed dependency assumption. Change a guard or write argument and rerun to produce a counterexample.

See [the contract guide](../../docs/source-contracts.md) for the supported subset, evidence artifacts and remaining manual obligations.
