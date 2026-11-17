# Verify an implementation against a contract

**Experimental.** Lemma can check a small, explicit subset of existing TypeScript, Python and Go code against the same contract representation. This checks the selected function body. A call model is an assumption, and the result names it.

Start with the [working example](../examples/source-contracts/mod.lm). It binds three independently written handlers to one contract and checks six properties: permission before a write, project scope, lease epoch, requested status, write before success, and success only after an accepted write.

## Run the example

Build the current source and install a native [Z3 executable](https://github.com/Z3Prover/z3) separately. Z3 4.15.4 was used for the example. There is no new Rust dependency.

```sh
cargo build -p lemma-cli --bin lemma
LEMMA_SMT_SOLVER=/absolute/path/to/z3 ./target/debug/lemma check examples/source-contracts/mod.lm --no-daemon --level strict
```

`LEMMA_SMT_SOLVER` selects an absolute executable, or a name found on the absolute entries of `PATH`; it defaults to `z3`. A checker's `env` can also set it. Each checker has a total solver time budget from `timeout_ms`.

The example expects `conditional`: it verifies the handler's decisions assuming the stated atomic lease behavior. It does not prove that the database implements that assumption. The handler examples show function bodies whose persistence dependency is supplied by the enclosing application; they are verification examples, not standalone HTTP servers.

Try these changes in any one handler and run the check again:

| Change to the implementation | Property that fails |
| --- | --- |
| Replace the permission guard's condition with `false` / `False` | Permission before write |
| Pass the opposite of `request.enabled` to the persistence call | Requested status |
| Replace the rejected-write guard's condition with `false` / `False` | Success requires an accepted lease |

The real-solver tests exercise all nine mutations across the three languages. They also check that contradictory preconditions are refused.

## Connect a contract to source

Declare a checker with `engine: "proof"`, rather than `command`. Its arguments are named `handler: Operation` and `contract: Json`; it returns `Evidence<Json>`. The operation's existing `implementation: file(...).symbol(...)` binding selects the source. A behavior uses `need` and checks `proof.holds`, just like other checker evidence.

```lm
module proof_example

export checker prove(handler: Operation, contract: Json): Evidence<Json> {
    engine: "proof"
    timeout_ms: 30000
}
```

The complete example supplies these contract fields as an ordinary `.lm` record:

| Field | Meaning |
| --- | --- |
| `inputs` | Symbolic `"Bool"`, `"Text"`, or nested records of these sorts |
| `parameters` | Input names, in the source function's parameter order; defaults to the source names |
| `requires` | Optional list of Boolean expressions constraining inputs |
| `ensures` | Nonempty list of `{ name, condition }` postconditions |
| `models` | Source call names mapped to call models |
| `bindings` | Optional explicit assumed constants for source globals |
| `max_paths` | Optional source path limit, default 256, maximum 4096 |

Expressions use `.lm` syntax: `!`, `&&`, `||`, `==`, `!=`, record fields and exact integer constants. `result` is the returned value. Integer ordering can compare constants such as effect counts; symbolic numeric inputs and arithmetic are unsupported.

Inputs not listed in `parameters` are specification inputs. For example, `lease.rejected` describes an arbitrary answer from the lease check. It is available to call models and postconditions, not injected into the source function's local variables. A specification input names one value, so a call model whose templates read one may be called at most once on a path; a second call is refused rather than assumed to answer alike.

Each call model declares:

- `assumption`: a nonempty description of the assumed semantics, including which effects it can have.
- `parameters`: positional argument names used inside its templates.
- `returns`: a return-value template.
- Optional `outcomes`: sorts of values chosen afresh for every call, read in templates as `outcome.<name>`. Use them for whatever may differ between two calls, such as whether each write was rejected.
- Optional `effect: { name, fields }`: an event recording selected call arguments and outcomes.
- Optional `async: true`: the function returns a promise or coroutine. A call to it must be awaited, and awaiting a model without `async` is refused, so a forgotten `await` cannot pass.

Templates are literal Booleans, strings, exact integer constants and records. A record containing only `{ expr: "expression" }` evaluates that expression against specification inputs, the call's arguments and its outcomes. Models can use different source names for the same call model; the example maps both `commitStatus` and `commit_status` to the shared `commit` record.

`effects.commit.called`, `.count`, and `.last` describe **attempts to call the modeled function**. They do not mean a successful database commit. Guard `.last` with `.called`; it is absent before any such call. Calls without a call model are refused. A used call model is always reported as an assumption, even when the called function's source happens to be in the same file.

## Read the evidence

| Result | What was established |
| --- | --- |
| `proved`, `holds: true` | Every normal return satisfies the postconditions in the supported semantics, without modeled dependencies or assumed constants |
| `conditional`, `holds: true` | The same statement under the listed call model and constant assumptions |
| `violated`, `holds: false` | A violating input exists in those semantics; call model assumptions still apply |
| Unavailable evidence | Unsupported code, missing binding or solver, solver error/`unknown`, exceeded limits, inconsistent preconditions, or no reachable normal return |

`--level strict` fails on unavailable evidence. Never accept `conditional` without reviewing its assumptions. A behavior may explicitly assert the expected assumption list, as the example does.

The existing evidence directory retains `input.json`, `output.json`, SMT-LIB queries and solver outputs. `output.json` includes the source file, symbol, source digest, contract digest, toolchain digest, coverage, and named properties. Counterexamples include branch line numbers (one-based) and Z3's input model; `symbols` maps its `input_0` names back to contract input paths.

Run `lemma check ... --json` to find evidence keys in the behavior report. The default artifact directory is `$XDG_CACHE_HOME/lemma/evidence/runs/<first-16-key-characters>` (or `~/.cache/lemma/...`). Each `.smt2` file can be replayed with `z3 -smt2 <file>`.

Evidence reuse includes the verifier executable, solver executable, contract, declared environment, full states of the repositories read, and the bound source bytes, including ignored source. `--no-run` uses existing evidence only; changed inputs make a case stale. Repositories outside Git and unresolved toolchain identities disable reuse. Keep the solver's runtime dependencies/environment pinned in CI as part of the trusted toolchain.

## Supported source and limits

| Language | Initial source subset |
| --- | --- |
| TypeScript | Top-level named functions and function/arrow variable bindings; `const`/`let` bindings; Boolean guards; strict scalar equality; property/literal-string reads; plain object literals; scalar ternaries; direct modeled calls; `await` of a modeled call |
| Python | Top-level undecorated functions with constant docstrings; bindings; `if`/`elif`/`else`; Boolean guards; scalar equality; dictionaries with literal string keys; direct or dotted modeled calls |
| Go | Top-level functions; single-name `:=` bindings; Boolean guards; scalar equality; struct field reads; keyed struct literals; modeled calls |

All three support explicit returns and records of immutable values. Input shapes are preconditions on the values supplied to the selected function: ordinary getter-free objects in TypeScript, ordinary dictionaries in Python, and matching structs in Go. No source binding can shadow or rebind an existing binding in this first subset. Calls in record fields, symbolic ternary branches and short-circuit right operands are refused to avoid silently changing effect order.

The checker refuses loops, mutable heap updates, destructuring, callbacks, decorators, exceptions/`defer`, implicit Boolean coercion, JavaScript coercive equality, numeric arithmetic, string escapes/interpolation, and other unhandled syntax. It does not follow local helpers or imports automatically; an external helper needs an explicit model.

For an async function, `result` means the completed body return; it assumes the caller awaits completion. Promise/coroutine scheduling is outside these semantics. The result concerns normal returns. It does **not** establish absence of exceptions, termination, concurrency safety, correctness of called functions, source typechecking, module initialization, dynamic function replacement, or that a framework routes requests to the bound function. Keep normal compiler checks and runtime tests. Route-registration callbacks are outside this first subset.

The frontends and SMT encoding are trusted implementation components, not verified compilers. This is a usable first slice with explicit coverage, not a claim that arbitrary TypeScript, Python or Go programs are now formally verified.

## What remains manual

You define the intended behavior, runtime input shapes, which operation implements it, argument mapping, and call model assumptions. Lemma reads that source and generates the path obligations and counterexamples. You do not write a new symbolic executor for every endpoint.

The [design note](../.specs/22-source-contracts.md) compares the chosen architecture with Boogie, Viper, Why3, Gillian, JML and ACSL. Heap, ownership and concurrency extensions should reuse established backends where possible. A future state-machine contract would still need this connection to source; checking the machine alone cannot establish implementation conformance.

To run the optional real-Z3 validation lane explicitly:

```sh
LEMMA_SMT_SOLVER=/absolute/path/to/z3 cargo test -p lemma-graph --lib proof::tests -- --include-ignored
LEMMA_SMT_SOLVER=/absolute/path/to/z3 cargo test -p lemma-graph --test source_contracts -- --include-ignored
```
