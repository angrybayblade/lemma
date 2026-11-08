# Sentry: warm paths for checks

**Status: proposal, 2026-11-08.** The rows marked *decided* under [Decisions](#decisions) were settled in the design discussion that followed the November agent benchmark. Everything else is proposal. Measurements come from the `hill-climb` branch (commit 94eb04b), on a five-repository workspace of TypeScript, Python and Go services with their dependencies installed. Agent runs come from a private benchmark on the same workspace, whose figures are not recorded here.

## Problem

An agent checks one change many times. The verification skill (`skills/lemma-verify`) asks for a check at the end of each unit of work. The opt-in Stop hook then checks every edited repository again before the agent may stop. In the benchmark, agents following that skill ran several checks per task.

Every check is a new process that starts from disk. Here is `lemma check --change` on the repository whose links use schema resolvers:

| Run | Time | Where the time goes |
| --- | --- | --- |
| First, cold | 6.8 s | 4.9 s on declared links, most of it running resolvers once; 1.7 s inference |
| Again, nothing changed | 2.7 s | 0.16 s starting; 0.73 s declared links; 1.5 s listing files, decoding the kept facts of 3,404 files and reading 1,166 of 10,650 consumer files; 0.3 s Soufflé |
| After a one-line edit | 3.0 s | the same, plus one file parsed |
| Each other repository, again | 2.4–2.8 s | |

Almost none of a repeat run's work is new. It rediscovers that nothing changed:

- It lists and hashes files whose contents it already knows.
- It decodes facts it decoded seconds ago.
- It asks Git for the state of every repository.
- It evaluates the rules over the same inputs as last time.

At 100k files a warm check takes 2.2 s (the pre-release scale gate). Most of that is decoding and indexing data that did not change ([spec 12](12-segments-and-shared-storage.md)).

The daemon already keeps state warm for lookups: language servers, and a live symbol index per indexed directory ([spec 7](07-semantic-providers-and-indexing.md)). `lemma check` never uses that state, because it always runs in its own process.

The benchmark also showed what long-lived state costs when its lifecycle is wrong:

- **Leaked indexes.** The daemon kept every index it had opened, with its database files and watcher, until it ran out of file descriptors. After a day of short-lived workspaces it held 644 deleted files and 116 watchers. This was fixed in 94eb04b: the maintenance tick now lets go of indexes whose directory is gone.
- **Watch limits.** The watcher registers every directory under a root. A workspace with dependencies installed has 90,148 directories, 90% of them under `node_modules` and `.venv`. Three such workspaces exceed the machine's limit of 259,504 watches per user. When registration fails, the live index silently walks the whole tree on every run.

## Goal

An *ask*, a check that sentry answers, should cost what the change costs, not what the system costs. It should return the same report a fresh check would.

Between asks, sentry listens to file changes and prepares the check's inputs in small batches. It never evaluates rules, runs resolvers or runs checkers ahead of an ask. All of that happens when an ask arrives.

**Targets** (unmeasured):

- On the five-repository workspace, an ask with nothing changed in under 0.1 s, and an ask after a one-file edit in under 1 s.
- At 100k files (the scale gate), under 1 s after a one-file edit.
- A first, cold ask at most 10% slower than an in-process check.
- Watches registered in proportion to the directories Lemma reads, not the directories on disk.

**Not in v1:**

- Lookups that know the working copy, such as `refs` or `search` saying "you removed that symbol". These come next.
- More than one workspace per daemon.
- Incremental Datalog.
- Running anything ahead of an ask.
- CI, where nothing lives long enough to be warm.

## Decisions

| Decision | Status |
| --- | --- |
| Sentry runs inside the daemon, attached to one workspace at a time | decided |
| It attaches only to a daemon that is already running; it never starts one | decided |
| Automatic attachment is opt-in, by the user | decided |
| Sentry listens and prepares; asks evaluate. No rule evaluation, resolver or checker runs ahead of an ask | decided |
| v1 serves checks only | decided |
| The name is sentry | decided |
| Everything below | proposal |

## Shape

```text
file changes ──▶ watcher ──▶ batches ──▶ preparation ──▶ prepared inputs
            (one per daemon)  (fast, slow, bulk)                 │
                                                                 ▼
lemma check, check tool, Stop hook ──▶ daemon ──▶ ask ──▶ change-driven evaluation ──▶ report
                                                   └── same inputs and options ──▶ the answer already given
```

Inputs are set as they change, and results are computed only when asked for. This is the model of salsa (rust-analyzer) and of Gradle's retained file system state, adapted to a check that spans repositories.

## 1. Prepared inputs: the warm path

| Input | A fresh check | Sentry keeps | Refreshed when |
| --- | --- | --- | --- |
| Each repository's state: its Git tree and the digests of changed paths | runs `git rev-parse HEAD^{tree}` and `git status --untracked-files=all` in every repository with resolvers | the state, updated from batches | a batch touches the repository, or HEAD moves |
| Manifests (`lemma.yaml`, `mod.lm`) and their composition | parsed and composed | parsed and composed | one changes (slow batch) |
| Documents (OpenAPI, JSON Schema) | read and parsed | parsed | one changes (slow batch) |
| Code facts per file | lists every file, hashes it, decodes kept unit records, parses misses | decoded facts by content hash, in memory | a batch reports the file |
| Index terms for change-driven selection | reads kept term segments | in memory | with the file's facts |
| The base side | reads the repository at the merge-base | kept | the merge-base moves |
| Resolver outputs | read from the on-disk cache by repository state; run on a miss | nothing ahead; read the same way at the ask | — |
| Answers | — | the report of each recent ask, by the fingerprint of its inputs | — |

Preparation writes the same unit records and segments a fresh check would ([spec 12](12-segments-and-shared-storage.md)). None of that work is wasted: an in-process check after a batch also starts warmer.

**Fast and slow batches.**

- A batch of source edits is **fast**: parse those files, then update their facts, their terms and their repository's state.
- A batch is **slow** if it touches a manifest, a document, a grammar or an ignore file, or if it moves the merge-base. Then sentry composes again, reads the documents again and rebuilds the base side.
- A newer batch cancels a preparation it would replace, and the two are prepared together. Sorbet cancels its slow path the same way.
- An ask never cancels preparation. It waits for it, since it needs the same work.
- Status counts fast, slow, bulk and cancelled batches.

## 2. Watching

Rollout step 0 replaces the recursive watcher per indexed directory with one watcher per daemon, shared by sentry and the live symbol indexes. That means one inotify instance on Linux and one FSEvents stream on macOS.

- **Only what Lemma reads.** Sentry watches directories in the index's selection, with ignore files respected. It also watches Git's metadata: `HEAD`, the refs, `index.lock`, rebase and merge state, and, for a worktree, the directory its `.git` file names.
  - A directory created later is watched when it appears, if it is selected.
  - `node_modules`, `.venv` and build output are not watched. Their changes do not change what a check reads today either, because resolver outputs are cached by the repository's Git state, which ignores them.
- **Events are hints.** Before an ask, the watcher's barrier confirms that every change made before the ask has been reported. The barrier writes a marker file and waits for its event; reports arrive in order, so every earlier change has been seen. Reported paths are then read and hashed again. A full walk of the repository happens at the next ask when:
  - an event was lost;
  - the queue filled up (`IN_Q_OVERFLOW`);
  - FSEvents asked for a rescan;
  - or the barrier did not arrive in time.

  That ask is slower, but never wrong.
- **Limits are visible.** Registrations count against the per-user limits, `max_user_watches` and `max_user_instances`. When registration fails, sentry and the live indexes walk the tree instead, and `lemma daemon status` says so rather than hiding it.
- **Network file systems** such as NFS, SMB and FUSE report changes unreliably. Sentry does not attach there, and says why. Git's fsmonitor daemon refuses them by default too.

## 3. Batches and bulk state

- **Settling.** Changes coalesce until 200 ms pass without a new one. They are prepared at most 2 s after the first.
- **Bulk state.** A repository is in bulk state while Git is busy, or when one batch holds more than 1,000 paths. Git counts as busy when its `index.lock` is held, when HEAD or the refs are moving, or when a rebase or merge is in progress.
  - In bulk state, paths accumulate and nothing is prepared file by file.
  - When Git lets go and changes settle, one refresh runs. A very large refresh is left to the next ask, as a full walk that reuses kept unit records.
  - Watchman lets version control tools announce such states (`state-enter`, `state-leave`). Git sends no such signal, so sentry infers it.
- **Base.** When HEAD or the default branch moves, sentry asks Git for the merge-base again. If it changed, the base side is rebuilt as a slow batch.

## 4. Asks

**Routing.**

- `lemma check` asks the daemon only if it is already running. It never starts one for sentry.
- If sentry is attached to the workspace the command names (the same manifest, by canonical path), the daemon serves the ask.
- Otherwise the daemon's reply says so, and the command runs in its own process, as today.
- Commands that need language servers (`--anchors`) run in their own process in v1.
- `--no-daemon` and `LEMMA_SENTRY=off` always run in-process.
- The MCP `check` tool runs `lemma check`, so it follows the same route.

**The caller's environment.** Resolvers and checkers that an ask runs must see the caller's environment. The daemon's own environment may come from another shell or virtualenv. Today the resolver cache keys on `PATH`, `VIRTUAL_ENV` and `PYTHONPATH` as the process sees them (`inherited_environment`), and resolvers and checkers inherit the process's whole environment. So an ask carries its working directory and its environment, and the check code takes the environment as a parameter instead of reading the process's.

**Serving.** The daemon serves one request at a time today. It hands an ask, with its connection, to sentry's worker thread, so lookups keep being answered while a check runs.

**Steps.**

1. Wait for the barrier, then prepare whatever is pending.
2. Fingerprint the ask. The fingerprint covers:
   - the manifests;
   - each repository's state at the base and the head;
   - the options that change what is evaluated or reported, such as `--change`, `--base`, `--head`, `--level`, `--select`, `--no-run`, `--run-base` and `--links` (options that only change how an answer is printed, such as `--json`, apply when it is rendered);
   - the environment variables the resolver cache already keys on;
   - Lemma's build.
3. If an answer with that fingerprint is kept, use it. Otherwise evaluate on the prepared inputs, using change-driven evaluation, with resolvers and checkers run on demand through their existing caches.
4. Render the result with the same code an in-process run uses.

**Output.** The report is the same as an in-process run's: the same findings, in the same order, with the same exit code. When sentry served the ask:

- JSON output adds a `sentry` object: the token, whether the answer was reused, and the batch it reflects.
- Text output adds one last line with the token, so an agent can ask for a delta next time.

Checks are deterministic (see CONTRIBUTING), so a reused answer is the answer a fresh run would give. Reusing answers is a local, in-memory form of the receipts in [spec 10](10-native-datastore.md). It is exactly as strict as the resolver and evidence caches a fresh check already relies on.

## 5. Tokens, deltas and events

- **Token.** A token is an opaque string naming sentry's instance and a position in its journal. Consumers must not parse it.
  - A token from another instance (sentry restarted or attached again) gets a full answer marked `reset`. So does a token older than the journal keeps.
  - Opaque tokens rather than timestamps follow Git's fsmonitor protocol, whose second version replaced timestamps because they raced with the watcher.
- **Deltas.** `lemma check --since <token>`, and `since` on the MCP `check` tool, report what changed since the answer at that token, for the same options. That means findings introduced since then, and findings that were there and are gone. Agents re-read whole reports today; one reached 117,000 characters.
- **Events.** `lemma daemon sentry events [--since <token>] [--wait <duration>] [--json]` lists the journal after the token:
  - **batches**: the repository, how many paths, whether fast, slow or bulk, and whether prepared or cancelled;
  - **state changes**: attached, warm, bulk entered and left, degraded (and why), detached (and why);
  - **answers**: the options, the counts of errors, warnings and informational findings, whether reused or evaluated, and how long they took.

  With `--wait`, the command blocks until there is an entry or the time runs out. This is pull with a cursor, like Watchman's clocks. Neither Claude Code's hooks nor MCP clients can receive a message pushed to them in the middle of a turn, but a hook or a script can long-poll.
- **Retention.** Sentry keeps the last 1,000 journal entries, and the last 8 answers per set of options.

## 6. Lifecycle and configuration

- **Attaching by hand.** `lemma daemon sentry start [MANIFEST]` attaches sentry to a workspace's `lemma.yaml` or `mod.lm`, by default the `lemma.yaml` in the current directory. It fails in two cases:
  - No daemon is running. The daemon starts with the first lookup, or at login after `lemma daemon install`.
  - Another workspace is attached. The error names it.
- **Attaching automatically.** This happens only if the user opted in, with `auto_attach = true` under `[sentry]` in `~/.config/lemma/config.toml`.
  - Then the first `lemma check` in a workspace, while the daemon runs and nothing is attached, attaches sentry and becomes its first ask. Its warm-up is that check, done once instead of twice.
  - A project's own configuration cannot turn this on, since opting in is the user's choice.
  - Never automatic when `CI` is set. Nx turns its daemon off in CI by default for the same reason.
- **Detaching.** Sentry detaches:
  - on `lemma daemon sentry stop`;
  - when the workspace's directory disappears (the maintenance tick from 94eb04b);
  - when the daemon exits or is replaced by a new build.

  Attachment does not survive the daemon. With `auto_attach`, the next check attaches again. Sentry's own activity does not count as requests for the daemon's idle exit.
- **Status.** `lemma daemon sentry status [--json]`, and a section in `lemma daemon status`, show:
  - the workspace;
  - its state: warming, warm, bulk or degraded;
  - pending paths;
  - batches by kind;
  - watches used against the limits;
  - memory held;
  - the last answer's token, whether it was reused, and how long it took.
- **Off.** `LEMMA_SENTRY=off` turns off both routing and attaching. Rails' Spring preloader shows why a cheap, obvious switch matters: when its warm state went stale, users reached for `spring stop` and `DISABLE_SPRING`.
- **Keys** under `[sentry]`:
  - `auto_attach`, default `false`;
  - `settle`, default `"200ms"`;
  - `max_delay`, default `"2s"`;
  - `bulk_paths`, default `1000`.

  Unknown keys stay errors, as they are under `[daemon]` and `[servers]`.

## 7. Where the code goes

- **`lemma-graph`** holds prepared inputs as a value. It offers `load` (everything, cold), `refresh` (changed paths per repository, and Git state) and a check over the prepared inputs.
  - The in-process `lemma check` becomes "load, then check". The warm path and the oracle therefore share one evaluation. They can differ only through stale prepared inputs, which the refresh rules and the differential gate guard against.
  - The environment becomes a parameter here.
- **`lemma-index`** holds the shared watcher: subscribers by root, the barrier, registration that follows the selection, and limits with their fallback.
- **`lemma-cli`** holds the daemon's sentry worker (its thread, batching, journal, routing and configuration) and the commands. Rendering stays unchanged.

## Correctness argument

1. Warm and fresh checks run the same evaluation code, over inputs of the same type.
2. Every prepared input is derived from file contents and recorded with their hash. The barrier guarantees that every change made before an ask was reported. Reported paths are hashed again, and any doubt falls back to a full walk.
3. An answer is reused only for an identical fingerprint, and checks are deterministic.
4. Sentry runs nothing ahead of an ask, so it causes no side effects between asks. Resolvers and checkers run at the ask, with the caller's environment and their existing caches, exactly as in a fresh check.

**Gate.** This follows the pattern `--evaluation both` set for change-driven evaluation.

- A differential mode replays each case's change as file edits into an attached sentry, asks, and compares the report with a fresh in-process check. Any difference fails the gate.
- It runs over the replay cases, the mutation cases, the fixtures, and the 10k and 100k systems.
- Seeded randomized sequences add what replays miss: edits and reverts, a Git checkout mid-sequence, a deleted directory, and simulated lost events.
- Resource tests check that watches and descriptors return to their baseline after detaching, and after a workspace is deleted.

## Rollout

0. **Watcher.** This step is worth doing even without sentry.
   - One instance per daemon, with registration that follows the selection.
   - Limits and fallback reported; network file systems refused.
   - Per-event timestamps in the agent benchmark, to split an agent's time between the model, Lemma's answers, checks and hooks.

   Measure the watches per workspace, which should be near the 9,200 of the measured workspace's 90,148 directories that are not under dependencies, and confirm no change in behaviour.

   **Landed, 2026-11-08.** On Linux each live index watches only the directories its selection enters, one by one, and follows new ones when changes are collected. A failure is kept and shown by `lemma daemon status`. On the measured workspace the daemon holds 1,684 watches (90,148 directories on disk), and preparing a workspace for agents (index plus language servers) fell from about 95 s to 53 s, since registering every directory had been slow too. Still to do: an index still has its own watcher instance (5 for this workspace, against a limit of 128); one instance per daemon waits until that matters. Refusing network file systems waits for a way to ask the file system type. Sentry needs a watch scope covering what a check reads, which is wider than a symbol index's selection.
1. **Prepared inputs** in `lemma-graph`.
   - The in-process check rebuilt on them, with the environment as a parameter.
   - The differential gate.
   - A profile of the 0.73 s declared-links phase, to see how much of it is Git, documents and schema comparison.
2. **Hosting in the daemon.** Start, stop and status; batches, bulk state and cancellation; routing; reused answers; configuration. Measure against the targets.
3. **Tokens, deltas and events.** Add `since` to the MCP tool, and have the Stop hook and the skills read warm answers and deltas.
4. **An agent benchmark round** of verification-mode agents with and without sentry.
   - Each run gets its own daemon, through its own `TMPDIR`: the daemon's socket lives there, and a daemon attaches only one workspace.
   - Compare time and cost with the same agents before sentry.

**Later:**

- Lookups that know the working copy.
- Edit hints from agent hooks, as `dmypy recheck --update` takes them.
- A Watchman backend for very large repositories. Buck2 uses Watchman for those and `notify` otherwise.
- More than one workspace.
- Incremental Datalog, if evaluation comes to dominate ([spec 10](10-native-datastore.md)).

## Prior art

| System | What it does | What sentry takes |
| --- | --- | --- |
| Gradle file system watching | Keeps a virtual file system in the daemon between builds, kept current by file system events. Uses one inotify watch per directory and one instance per daemon. Gives no benefit when the daemon dies after each build. | One watcher instance per daemon; watch only what is read; warm state lives as long as the daemon |
| Bazel `--watchfs` | Off by default. The watcher supplies candidate changes, and Skyframe's dependency tracking decides. Skyfocus keeps incremental state only for declared working directories. | Events are hints; correctness comes from contents; one attached workspace |
| salsa, and Buck2's DICE | Inputs are set when they change, and derived queries are computed on demand. A pending write cancels queries on the old revision. Unchanged results stop propagation (early cutoff). | Prepare inputs, evaluate on ask; cancel stale preparation |
| dmypy | Keeps type-checking state in memory. Takes changed files from a watcher or the client (`recheck --update`, `--remove`), so unchanged files are not even stat'd. | Changed paths from the watcher; client hints later |
| Sorbet | A fast path for small edits, such as inside method bodies, and a slow path otherwise. A new edit cancels a running slow path. It counts both. | Fast and slow batches, cancellation, counters in status |
| Flow lazy mode | Focuses files its watcher reports. Saved state hashes each file, so only modified files are parsed. A lazy start from saved state does not recheck, so new errors stay hidden until a file is focused. | A hash per file; warm must never mean skipped |
| Watchman | Clocks and `since` queries, and subscriptions that settle. `state-enter` and `state-leave` hold or drop notifications while version control works. | Tokens; bulk state |
| Git fsmonitor | An opt-in daemon (`core.fsmonitor`) with start, run, stop and status, on a Unix socket. Refuses network file systems by default. Its hook protocol moved from timestamps to opaque tokens. | Opt-in; opaque tokens; network file systems refused |
| Nx daemon | Keeps the project graph in memory, with a file watcher. Off in CI by default; an environment variable overrides that. | Never automatic in CI; an environment switch |
| Rails' Spring | A preloader that polls for changes. It is known for stale code, CPU use and inotify limits on CI. Users reach for `spring stop` and `DISABLE_SPRING`. | Staleness impossible by construction; a cheap off switch |
| Incremental Datalog: Soufflé research, IncA, DBSP | Upstream Soufflé recomputes from scratch. Research engines evaluate incrementally, with proofs that the results match. | Not in v1: Soufflé takes 0.3 s here, and change-driven evaluation already shrinks its input |

None of these check agreement across repositories; that part is Lemma's. The mechanics beneath it are well understood.

## Open questions

- **Declared links.** What the warm 0.73 s is made of decides what sentry keeps for it. Rollout step 1 profiles it.
- **Memory.** How much decoded facts take at 100k files, and whether one workspace needs a budget or only a number in status.
- **The token in text output.** Whether to print it whenever sentry serves an ask, or only once `--since` has been used.
- **Environment in fingerprints.** The resolver cache keys on `PATH`, `VIRTUAL_ENV` and `PYTHONPATH`. Should answers key on more, given that checkers' evidence does not either?
- **macOS.** Whether one FSEvents stream keeps the barrier's ordering across roots, and how coalesced events map onto batches.
- **Worktrees and submodules.** Which Git metadata to watch when `.git` is a file.
- **The first automatic ask.** Whether it is served by sentry, so its warm-up is the check itself (proposed), or run in-process while sentry warms beside it.

## References

- Gradle: [file system watching](https://docs.gradle.org/current/userguide/file_system_watching.html), [introducing file system watching](https://blog.gradle.org/introducing-file-system-watching)
- Bazel: [filesystem watching](https://bazel.virtuslab.com/book/5~9~5/), [Skyframe and incrementality](https://bazel.virtuslab.com/book/2~4~1/) (a third-party book)
- salsa: [cancellation, PR #265](https://github.com/salsa-rs/salsa/pull/265), [`cancelled.rs`](https://docs.rs/crate/salsa/latest/source/src/cancelled.rs); Buck2: [Modern DICE](https://buck2.build/docs/insights_and_knowledge/modern_dice/), [Watchman or notify, issue #59](https://github.com/facebook/buck2/issues/59)
- mypy: [`dmypy recheck --update`, PR #5745](https://github.com/python/mypy/pull/5745)
- Sorbet: [server status](https://sorbet.org/docs/server-status), [making Sorbet more incremental](https://blog.jez.io/making-sorbet-more-incremental/)
- Flow: [lazy modes](https://flow.org/en/docs/lang/lazy-modes), [saved state and rechecks](https://github.com/facebook/flow/blob/2f02130fe8fdc195b41fb6ee4c2c97aae9f35268/tests/saved_state_init_recheck/test.sh)
- Watchman: [subscribe](https://facebook.github.io/watchman/docs/cmd/subscribe.html), [state-enter](https://sources.debian.org/src/watchman/4.9.0-9/website/_docs/cmd.state-enter.markdown)
- Git: [git-fsmonitor--daemon](https://man7.org/linux/man-pages/man1/git-fsmonitor--daemon.1.html), [hook protocol version 2 and opaque tokens](https://googlers.googlesource.com/jrn/git/+/8da2c57629a16e148b0f94282ac787c1503e4779)
- Nx: [the Nx daemon](https://nx.dev/docs/concepts/nx-daemon)
- Spring: [README](https://github.com/andrew/spring), [reliability discussion](https://discuss.rubyonrails.org/t/spring-fails-to-notice-changes-and-reload-frequently-enough-that-it-is-a-constant-hindrance/74496)
- Incremental Datalog: [incremental Soufflé](https://comp.anu.edu.au/sapling/2019/abstracts/zhao-abstract.pdf), [IncA](https://voelter.de/data/pub/inca-oopsla2018.pdf), [incrementalizing static analyses in Datalog](https://openscience.ub.uni-mainz.de/handle/20.500.12030/5617), [a DBSP-based Datalog compared with Soufflé and DDlog](https://ceur-ws.org/Vol-3801/paper1.pdf)
