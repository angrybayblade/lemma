# Source-backed verification contracts

Status: initial implementation, 2026-11-17.

## Objective

Check a declared contract against an existing implementation, without translating that implementation by hand or writing a checker for each operation. Exercise the same contract representation against TypeScript, Python and Go to prevent the representation from depending on one framework.

## Prior art and choices

| Foundation | What it contributes | Choice for this implementation |
| --- | --- | --- |
| [JML](https://www.openjml.org/tutorial/) and [ACSL](https://www.frama-c.com/html/acsl.html) | Preconditions, postconditions, explicit effects and frame conditions | Use named preconditions and postconditions, with explicit effect summaries for dependencies |
| [Boogie](https://boogie-docs.readthedocs.io/en/latest/) | Source frontends feeding an intermediate verification language | Keep source reading separate from symbolic evaluation and solver invocation |
| [Viper](https://www.pm.inf.ethz.ch/research/viper.html), [Nagini](https://www.pm.inf.ethz.ch/research/nagini.html), [Gobra](https://www.pm.inf.ethz.ch/research/gobra.html) | Reusable verification infrastructure and source-language frontends | Preserve a backend seam for these tools; do not claim to implement their heap, ownership or concurrency logics |
| [Why3](https://why3.org/doc/whyml.html) | Program contracts and multiple external provers | Keep solver execution external and its identity in evidence |
| [Gillian](https://gillianplatform.github.io/intro.html) | Multi-language symbolic execution with language-specific semantics | Treat unsupported source constructs as unavailable evidence |
| [SMT-LIB](https://smt-lib.org/language.shtml) and [Z3](https://github.com/Z3Prover/z3) | Standard logical queries and an existing solver | Emit SMT-LIB rather than implement satisfiability solving |
| [OpenJML feasibility checks](https://www.openjml.org/tutorial/Feasibility) | Detect inconsistent assumptions that would otherwise make a proof vacuous | Require satisfiable input assumptions and reachable return paths |

The existing Tree-sitter parsers, `.lm` expression parser, operation bindings, process runner, checker protocol and evidence store are reused. No new Rust dependency is needed. Z3 is an optional external executable.

## Integration

A checker may name `engine: "proof"` instead of `command`. Its `handler: Operation` argument supplies the repository, source file and symbol through the existing checker encoder. Its `contract: Json` argument holds the contract. Contracts remain `.lm` values; JSON is only the existing in-process checker protocol.

The native checker runs at the selected checkout, uses the same evidence and `--no-run` policies as command checkers, and returns an observation with status, assumptions, source identities, coverage and counterexamples. A source reader is selected by the implementation's extension. Additional backends can consume the same contract and return the same result.

## Contract semantics

Inputs declare symbolic Boolean and text values or records of such values. Integer literals such as HTTP status codes are supported. Arithmetic, mutable heap reasoning and concurrent schedules require a later, explicitly specified extension.

Parameters bind the implementation's arguments to declared inputs. Preconditions constrain the input domain. Every normal return must satisfy each named postcondition. Expressions use the existing `.lm` expression syntax. `result` names the returned value. `effects.<name>.called`, `.count`, and `.last` describe modeled calls.

call models declare their argument names, return-value templates, optional effect fields and a named assumption. Expression templates use `{ expr: "..." }`; ordinary values are literal templates. Every used call model is reported as an assumption. Callback invocation is outside the initial subset. Calls without a source interpretation or an explicit summary are unsupported.

Models summarize effects on the verification state. They do not prove database, cryptographic, network or runtime implementations. Source mutation outside the supported value subset is refused, rather than silently assuming a frame condition. Models must name the assumption under which their summarized effects are complete.

## Outcomes and trust

- `proved`: no violating input exists in the supported source semantics and no call model was assumed.
- `conditional`: the properties hold under the listed, used call model assumptions.
- `violated`: a satisfiable violating path exists in those semantics; the result includes a witness and source locations. With call model assumptions, that witness is conditional too.
- Unavailable evidence: unsupported syntax, unresolved or ambiguous binding, missing tools, solver errors or `unknown`, path limits, inconsistent assumptions, or no reachable return. `lemma check --level strict` fails on this existing unknown outcome.

The source readers and logical encoding are trusted implementation components, not independently verified compilers. The supported semantics and exclusions are part of every result. Solver input and output are retained beside the checker's evidence. The checker build and resolved solver executable contribute to cache identity. Native proof checkers read the full state of every repository they name, avoiding the impact walk's narrower assumptions. Their operation arguments also carry a digest of the bound source bytes, so ignored source cannot escape invalidation. Source and toolchain identities are checked again while producing evidence. Solver selection uses an absolute executable or the absolute entries of PATH.

## Validation

Use one contract on actual, independently written source functions in TypeScript, Python and Go. Verify the correct versions, then change the implementation to ignore a rejected write, reverse a status or bypass a permission check and require counterexamples. Test unsupported syntax, ambiguous bindings, invalid solver output, contradictory preconditions, dependency failures, evidence invalidation and `--no-run` through the existing `lemma check` entry point.

## First slice and next boundaries

Implemented: the native checker, strict Tree-sitter readers for all three languages, shared symbolic execution, call models with explicit assumptions and effects, feasibility checks, replayable SMT-LIB, counterexamples, and evidence integration. The worked example is `examples/source-contracts/mod.lm`; `docs/source-contracts.md` defines the supported syntax and manual obligations.

The first slice intentionally handles top-level named function bodies and TypeScript arrow bindings, rather than route-registration callbacks. Python records are ordinary dictionaries read with literal string keys; Go records are keyed struct literals and field reads. Source typechecking, module initialization, dynamic replacement of functions and framework routing are outside the proof. Parameter shapes and dependency interpretations remain explicit obligations. No claim is made that arbitrary production endpoints can be verified unchanged.

Before adding mutable heap, loops or concurrency, evaluate existing Viper/Nagini/Gobra and Boogie backends for that obligation class. The shared contract and result representation are the reusable Lemma layer; language semantics and proof infrastructure already offered by those projects should not be reimplemented merely for uniformity. State machines can later supply transition contracts, but a checked abstract state machine does not by itself discharge source refinement.

## Relationship to behavior harnesses (reviewed 2026-11-17)

This builds on document 13's behaviors, checker arguments, evidence store and outcomes. It runs alongside [document 18](18-behavior-harnesses.md), whose scope explicitly excludes static/symbolic evidence. It does not implement `replay/1`, service setup/entry, runtime call interception or language-pack installation and selection.

| Layer | Reused today | Still separate or pending |
| --- | --- | --- |
| Behavior evaluation and checker evidence | Existing `need`, assertions, operation encoding, caching and unknown/stale outcomes | No second behavior evaluator |
| Dependency identity | Source call names in proof models | Replay names links/operations; a shared mapping is still needed |
| Source/runtime entry | Named function body reader | Real framework entry belongs to replay setup; route callbacks are unsupported here |
| Coverage and toolchain identity | Kept with proof observations and evidence keys | Replay's top-level coverage/toolchain envelope is not yet unified with the proof result |
| Language distribution | Built-in TypeScript/Python/Go source readers | Different from document 18's installable runtime language packs; provider packaging remains an explicit integration decision |

The first source backend must not be described as completing the behavior-harness rollout. Future work should preserve one checker/evidence foundation and share provider capabilities and identities where useful, without presenting a symbolic assumption as a captured runtime observation. The private-corpus audit is an explicit ignored test, separate from the public-fixture Z3 CI lane.

## Corpus validation follow-up

The private-corpus audit uses named definitions as its coverage denominator and reports source readability separately from proved contracts. It does not treat parsing a function as verifying it. Focused runtime tests remain separate observations. Constant leading Python function docstrings are accepted as metadata; formatted strings and later expression statements remain unsupported, with regressions covering that boundary.
