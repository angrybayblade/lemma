# Contributing to Lemma

These conventions apply to all code in this repository. The goal is code that a new reader can follow without a guide, and history that explains itself.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

```text
<type>(<scope>): <summary>

<body: why the change was made, not a restatement of the diff>

<footer: BREAKING CHANGE:, Refs:, Co-Authored-By:>
```

**Types**

| Type | Use for |
| --- | --- |
| `feat` | New user-visible capability |
| `fix` | Bug fix |
| `refactor` | Behavior-preserving restructuring |
| `perf` | Performance improvement without behavior change |
| `test` | Adding or correcting tests only |
| `docs` | Documentation and specs |
| `build` | Build system, dependencies, toolchain |
| `ci` | CI configuration |
| `chore` | Maintenance that fits nowhere else |

**Rules**

- Scope is the crate or area touched, e.g. `feat(syntax): extract python call expressions`. Omit it only when the change is genuinely repository-wide.
- Summary is imperative, lowercase, no trailing period, at most 72 characters.
- One logical change per commit. A refactor and the feature it enables are two commits.
- Mark breaking changes with `!` after the scope and a `BREAKING CHANGE:` footer.
- Every commit should build and pass tests.

## Structure: cohesion and decoupling

- Each module or crate has one responsibility you can state in a sentence. If the sentence needs "and", consider splitting it.
- Dependencies point inward toward the domain model. The model knows nothing about Tree-sitter, LSP, SQLite, or the CLI.
- External formats stop at the boundary that consumes them. LSP response shapes, Tree-sitter nodes, and SQL rows are converted to Lemma types inside their adapter and never leak further.
- Adapters do not depend on each other. Shared behavior goes in the common layer they both use.
- Define a trait at a boundary only when there is, or will soon be, more than one implementation, or when it is needed to test the consumer in isolation.
- Prefer passing data over passing services. A function that needs three fields takes those fields, not the whole context object.

## Functions

- **Pure where possible.** Keep logic in functions that take inputs and return outputs. Push I/O (files, processes, network, database, clock) to the edges, and have the edges call the pure core.
- **One job each.** If a function's name needs "and", split it. If it takes a boolean that switches between two behaviors, it is usually two functions.
- **Deterministic.** Same inputs, same outputs. Sort anything that reaches output, storage, or a hash. Do not let map iteration order, wall-clock time, or thread scheduling affect results. Lemma's guarantees depend on this.
- **No hidden state.** No globals or mutable statics. Dependencies arrive as parameters.
- Keep functions short enough to read without scrolling. Length is a symptom; mixed responsibilities are the problem.

## Control flow and readability

- **Exit early.** Handle invalid input, missing data, and errors first with guard clauses, `?`, or `let ... else`, then write the main path unindented.
- No `else` after a branch that returns, breaks, or continues.
- Keep nesting to two or three levels. Deeper nesting means a helper function is missing.
- Prefer clear over clever. A plain loop beats an iterator chain nobody can read at a glance.
- Name intermediate values instead of nesting long expressions.

```rust
// Prefer
fn resolve(reference: &Reference, index: &SymbolIndex) -> Resolution {
    let Some(candidates) = index.lookup(&reference.name) else {
        return Resolution::Unresolved;
    };
    if candidates.len() > 1 {
        return Resolution::Ambiguous(candidates.to_vec());
    }
    Resolution::Resolved(candidates[0].clone())
}
```

## Errors

- **Typed errors.** Each library crate defines its own error enum with `thiserror`. Variants describe what went wrong in domain terms, such as `ProviderUnavailable` or `AmbiguousReference`, and carry the context needed to act on them.
- **Delegate, don't swallow.** Propagate with `?`. When a lower-level error crosses a boundary, wrap it in the caller's error type with `#[from]` or `#[source]` so the full chain survives. Never discard an error or replace it with a string.
- **Each layer adds its own context.** A storage error inside resolution becomes `ResolveError::Store { snapshot, source }`, not a bare `StoreError`.
- **Handle or return, not both.** Do not log an error and then also return it.
- **No panics in library code.** No `unwrap`, `expect`, or `panic!` outside tests. A truly impossible state gets an `expect` that states the invariant, and a review.
- **Expected outcomes are not errors.** An unresolved reference, an empty result, or an unsupported capability is a normal value (see `07-semantic-providers-and-indexing.md`), not an `Err`.
- Only the CLI binary turns the error chain into user-facing output.

## Tests

- Unit tests live beside the code in a `#[cfg(test)] mod tests` block.
- Test names describe behavior: `ambiguous_reference_returns_all_candidates`, not `test_resolve_2`.
- Arrange, act, assert, each step clearly separated. One behavior per test.
- Test pure functions directly. If something is hard to test, that usually means I/O is mixed into logic, so fix the design rather than the test.
- Tests are deterministic and offline: no network, no wall clock, no dependence on the machine they run on.

## Inline documentation

- Every public item gets a doc comment saying what it is for, in one or two plain sentences. Add detail only when the caller would otherwise get it wrong.
- Comments explain **why**: intent, constraints, non-obvious decisions, links to the relevant spec. They do not narrate what the code visibly does.
- Write for a capable engineer who is new to this codebase, not for a specialist. Avoid jargon where a plain word works.
- Keep comments accurate. Update or delete a comment in the same change that makes it wrong.
- Do not leave commented-out code. Git remembers it.
- `TODO` comments name the missing piece and, where one exists, an issue: `// TODO(#42): handle renamed symbols`.

## Naming

- **Descriptive.** A name says what the value is or what the function does. Avoid `data`, `info`, `tmp`, `res`, `handle_stuff`. Single letters only for short closures and conventional indices.
- **No unexplained abbreviations.** `reference`, not `ref_`; `snapshot`, not `snap`. Established acronyms are fine: `id`, `uri`, `lsp`, `http`, `sql`.
- **Booleans read as questions:** `is_stale`, `has_coverage`, `should_retry`.
- **Functions are verbs, values are nouns:** `resolve_reference` returns a `Resolution`.
- **Units in names** when the type does not carry them: `timeout_ms`, `size_bytes`.
- **One word per concept, everywhere.** Use the vocabulary from `.specs/` consistently in code, comments, logs, and errors. Do not introduce synonyms.

| Term | Means | Do not call it |
| --- | --- | --- |
| snapshot | A fingerprinted set of source and configuration inputs | version, state, build |
| symbol | A logical code entity reconciled across snapshots | node, entity, item |
| occurrence | A symbol at a specific location in one snapshot | instance, usage, hit |
| relationship | A typed connection between entities | edge, dependency |
| requirement | A property that must hold | rule, constraint, check |
| checker | Something that produces evidence for a requirement | validator, test runner |
| evidence | A result bound to specific inputs | proof, result, verdict |
| provider | A source of semantic facts, such as a language server | backend, engine |
| adapter | Code that translates one provider or format into Lemma types | driver, plugin, wrapper |
| operation | Something a service provides or calls, with a request and response | endpoint, function, method |
| link | A relationship between two operations, caller to provider, checked for persistence and schema agreement | edge, binding, connection |
| external service | Another repository's description, named `#name` and pinned | remote, dependency, import |
| change-driven evaluation | Reading only the consumer code a change's findings can rest on, when the grammar's requirements allow it | demand-driven, incremental, lazy |
| anchor | A relation, computed without consumer facts, holding the operations a requirement's calls must be of | seed, root, trigger |
| index term | A string a file's code facts are indexed by, standing for calls the file may make | key, tag, token |
| covered operations | The operations whose calls a change-driven run read in full | scope, focus, subset |
| unit record | One file's parse in Lemma's binary format, kept under a key naming its contents | blob, entry, row |
| segment | An immutable object built from many unit records at once, such as a repository's functions, named by a digest of its inputs | pack, bundle, snapshot |
| module | One `.lm` file, named by its place in its repository | package, file, library |
| graph | A named set of facts of one relation, written as rows, supplied by a provider or derived by a rule | edge list, table |
| fact | One tuple of a relation, with the provenance of each support | edge, row, triple |
| behavior | A requirement checked by running statements that may demand evidence | test, scenario, spec |
| case | One run of a behavior's statements for one set of loop values | iteration, example |
| check | A named entry point selecting the requirements to evaluate | suite, target |
| outcome | What evaluating a requirement or behavior established: satisfied, violated, unknown or stale | result, status, verdict |
| allowance | An `allow`: findings of one requirement that its condition selects are reported as allowed, with a reason, until a date | exception, suppression, waiver |
| sentry | The daemon's attachment to one workspace: it listens to file changes and keeps the inputs of that workspace's checks prepared between asks | watch mode, monitor, sentinel |
| ask | One `lemma check`, or `check` tool call, that sentry answers | query, request |
| batch | File changes that arrived together and are prepared together | tick, chunk, flush |
| bulk state | A repository changing too much at once to prepare file by file, as during a checkout or a rebase | storm, burst |
| prepared inputs | What sentry keeps between asks so that an ask need not list, hash, parse or decode again: an ask's warm path | warm cache, hot state |
| token | An opaque position in sentry's journal; a delta or an event list covers what came after it | cursor, clock, timestamp |
| description | A system's `.lm` modules, read as what its repositories are made of, what they offer and how they link | inventory, topology |
| candidate | A suspected gap in a description, found by a pattern rather than shown from the code; reported, and never fails a command | guess, hint |

Extend this table when a new domain concept appears, in the same change that introduces it.

## Language-specific rules

The conventions above apply to every language. Each language adds its own idioms and tooling, configured when the first code in that language lands:

| Language | Formatting | Linting |
| --- | --- | --- |
| Rust | `rustfmt` (`rustfmt.toml`) | `clippy` (`clippy.toml`, workspace lints below) |
| TypeScript | Prettier | ESLint with `typescript-eslint` strict rules |
| Python | Ruff format | Ruff, plus Pyright in strict mode |
| Go | `gofmt` | `go vet`, `golangci-lint` |

Where a language idiom conflicts with these conventions, follow the idiom and keep the intent. For example, Go returns `error` values instead of typed enums but still wraps them with `%w` so the chain survives.

## Enforcement

**Commit messages** are checked by `.githooks/commit-msg`. Enable it once per clone:

```sh
git config core.hooksPath .githooks
```

**Rust** formatting and lint thresholds live in `rustfmt.toml` and `clippy.toml`. Workspace lints are declared in the root `Cargo.toml` and every crate opts in with `[lints] workspace = true`. A few pedantic lints are allowed there because they would force verbose doc sections (`missing_errors_doc`, `missing_panics_doc`) or noise (`must_use_candidate`).

Before committing, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI should run the same three commands.
