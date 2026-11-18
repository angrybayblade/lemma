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
| `inputs` | Symbolic `"Bool"`, `"Text"`, `"Int"`, `"Null"`, `"Undefined"`, records of these, unions written as lists, such as `["Text", "Null"]` for a text that may be `None`, or sequences of a stated longest length, such as `{ sequence: "Int", max_length: 3 }` |
| `parameters` | Input names, in the source function's parameter order, a method's receiver first; defaults to the source names. `null` leaves a parameter out, so its default applies |
| `captures` | Names a nested function reads from the functions around it, each mapped to an input: every value it could hold when the function runs |
| `requires` | Optional list of Boolean expressions constraining inputs |
| `ensures` | Nonempty list of `{ name, condition }` postconditions |
| `models` | Called functions mapped to call models; `new Name` models a constructor and `super.method` a parent method |
| `decorators` | Decorators, by their text or callee, mapped to the assumption that the decorated function runs its body with the same arguments |
| `contexts` | Context managers of a Python `with`, by their text or callee, mapped to the assumption that entering returns the manager and leaving neither suppresses an exception nor has another effect |
| `bindings` | Optional explicit assumed constants for source globals |
| `max_paths` | Optional source path limit, default 256, maximum 4096 |

Expressions use `.lm` syntax: `!`, `&&`, `||`, `==`, `!=`, ordering, `+`, `-`, `*`, record fields, constant positions such as `result[0]`, the `length` of a sequence, `null`, `undefined` and exact integer constants. `result` is the returned value. Contract arithmetic is exact. A contract may read a field only where it is defined: reading a field of null, or one a returned object lacks, makes a postcondition fail wherever that read is reached, and a precondition must be defined for every input. Guard such reads, as in `r.user == null || r.user.admin`.

Inputs not listed in `parameters` are specification inputs. For example, `lease.rejected` describes an arbitrary answer from the lease check. It is available to call models and postconditions, not injected into the source function's local variables. A specification input names one value, so a call model whose templates read one may be called at most once on a path; a second call is refused rather than assumed to answer alike.

Each call model declares:

- `assumption`: a nonempty description of the assumed semantics, including which effects it can have.
- `parameters`: positional argument names used inside its templates.
- `returns`: a return-value template.
- Optional `outcomes`: sorts of values chosen afresh for every call, read in templates as `outcome.<name>`. Use them for whatever may differ between two calls, such as whether each write was rejected.
- Optional `effect: { name, fields }`: an event recording selected call arguments and outcomes.
- Optional `async: true`: the function returns a promise or coroutine. A call to it must be awaited, and awaiting a model without `async` is refused, so a forgotten `await` cannot pass.
- Optional `ignores_callbacks: true`: the function never calls a function passed to it. Without it, passing a function to the model is refused, since what that function would do there is outside the model.

Arguments bind the model's parameters in order, then by keyword. A parameter left out is absent rather than read from a specification input of the same name.

Templates are literal Booleans, strings, `null`, exact integer constants, records and lists. A record containing only `{ expr: "expression" }` evaluates that expression against specification inputs, the call's arguments and its outcomes. Models can use different source names for the same call model; the example maps both `commitStatus` and `commit_status` to the shared `commit` record.

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

The three languages share one reading. Each row below is read in every language that has it, with that language's meaning where they differ.

| Area | Read |
| --- | --- |
| Definitions | Top-level functions; methods by `Class.method`, with the receiver as the first parameter (`self`, `this`, or the Go receiver); nested functions and object-literal methods by their path, or by the name Lemma's symbol index gives a definition inside anonymous functions; `.ts`, `.tsx`, `.mts`, `.cts`, `.py` and `.go` files |
| Parameters | Defaults, keyword-only and positional-only markers, TypeScript optional parameters |
| Values | Booleans, text, integers, `None`/`null`/`undefined`/`nil`, records, objects built from literals, tuples, lists and arrays of known length, Python sets for membership |
| Text | Escapes, Python f-strings and raw, joined and triple-quoted strings, TypeScript template literals, Go raw strings; values are spelled inside formatted strings as their language spells them |
| Operators | Boolean operators without coercion, strict equality, Python `is` against `None`, `True` and `False`, `in` and `not in`, chained comparisons, integer ordering and `+`, `-`, `*`, text `+`, `??`, `?.`, ternaries and Python conditional expressions, TypeScript type syntax that does nothing at runtime |
| Bindings | Declarations, reassignment, `+=`, unpacking with rest, TypeScript object destructuring with rest, Go `:=` and `var` with a value; TypeScript and Go declarations are scoped to their block |
| Objects | Spreads in object and sequence literals; assigning a field of an object the function built and has not let anything else see |
| Control flow | `if`, `return`, `raise`, `throw`, Go `panic`, `assert`, `try` with `except`/`catch`, `else` and `finally`, Python `with` under context models, `pass` |
| Loops | Python `for`, JavaScript `for … of` and Go `for … range` over a sequence of known length, with `break`, `continue` and Python's `else`; `len` and `.length` of such a sequence |
| Calls | Call models, constructors through `new` models, and functions the code defines: lambdas, arrow functions, function expressions, Go function literals and nested definitions, run where they are called |

Some meanings are the language's own, and are kept:

- A path that raises has no normal return. Reading a field of null raises in every language; a position past the end raises in Python and Go and reads undefined in TypeScript; reading a field an object built in the code lacks raises in Python and reads undefined in TypeScript. Inside a `try`, such a path is followed to its handler. Handlers match Python's built-in exception classes by their parents; a class defined elsewhere is caught by its name, by `BaseException` or a bare `except`, and by `Exception` under a reported assumption.
- Python integers are exact. TypeScript integers are exact up to 2^53 - 1 and Go's to 64 bits, so their `Int` inputs stay in that range and arithmetic on them reports an assumption.
- A Python function's parameter defaults are evaluated once, where the function is defined; a TypeScript default at each call that leaves the parameter out.
- A loop over a sequence runs its body once for each item, so it is followed where the length is known: a literal, what a call model returns, or an input with a `max_length`. A result about such an input holds for every length up to the stated one and says nothing of a longer sequence. Each pass of a TypeScript or Go loop has its own variables; a Python loop binds variables of the function.
- Calling a function the code defines reads what it uses from around it as it is then, as late binding does. It is called only where every name it reads still means the variable it was defined with; a call from a block or function that declares one of those names again is refused. A name a Python function assigns is its own from its first line, and a TypeScript `let` or `const` its block's, so reading one before it is assigned is refused, where the languages raise. A callee with several paths is joined into one value only where its paths differ in nothing else.
- Input shapes are preconditions on the values supplied to the selected function: getter-free objects in TypeScript, attribute and item reads without descriptors or `__getattr__` in Python, and matching structs in Go. A record a contract declares may hold fields it never named; reading one of those is refused.

Every used call, decorator and context model, assumed constant and language assumption is reported, and makes the result `conditional`.

The checker refuses loops that run until a condition fails (`while`, and the three-part and bare `for` of JavaScript and Go), `for … in`, `for await`, a `break` or `continue` with a label, loops over a value whose length is not known, comprehensions, generators, variable subscript keys, mutation of objects the function did not build (including its parameters), `nonlocal` and `global`, rest parameters, implicit Boolean coercion, JavaScript coercive equality, division and modulo, Go zero values, `defer` and `recover`, JSX, and other unhandled syntax. It reads a function's own code; a function defined elsewhere is called only under a call model.

For an async function, `result` means the completed body return; it assumes the caller awaits completion. Promise and coroutine scheduling is outside these semantics. The result concerns normal returns. It does **not** establish absence of exceptions, termination, concurrency safety, correctness of called functions, source typechecking, module initialization, dynamic function replacement, or that a framework routes requests to the bound function. Keep normal compiler checks and runtime tests.

The frontends and SMT encoding are trusted implementation components, not verified compilers. This is a usable slice with explicit coverage, not a claim that arbitrary TypeScript, Python or Go programs are now formally verified.

## What remains manual

You define the intended behavior, runtime input shapes, which operation implements it, argument mapping, and call model assumptions. Lemma reads that source and generates the path obligations and counterexamples. You do not write a new symbolic executor for every endpoint.

The [design note](../.specs/22-source-contracts.md) compares the chosen architecture with Boogie, Viper, Why3, Gillian, JML and ACSL. Heap, ownership and concurrency extensions should reuse established backends where possible. A future state-machine contract would still need this connection to source; checking the machine alone cannot establish implementation conformance.

To run the optional real-Z3 validation lane explicitly:

```sh
LEMMA_SMT_SOLVER=/absolute/path/to/z3 cargo test -p lemma-graph --lib proof:: -- --include-ignored --skip corpus
LEMMA_SMT_SOLVER=/absolute/path/to/z3 cargo test -p lemma-graph --test source_contracts -- --include-ignored
```
