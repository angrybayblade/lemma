# Warm runtimes

**Status: built, 2026-11-13, for Python.** Measurements are from one five-repository workspace of TypeScript, Python and Go services with their dependencies installed, on Linux. macOS runs the same code and the same tests; a real dependency set has not been measured there. The rows under [Decisions](#decisions) were settled in the design discussion of 2026-11-13.

## Problem

A check runs a repository's own Python to learn what its code describes: a schema resolver written as `json.command("{python} -m …")`, and a type read with Pydantic ([check.md](../docs/check.md)). Each run starts an interpreter from nothing.

On the measured workspace one resolver took 1.4 to 1.7 s, and it was the longest thing a check after a Python edit waited for. About 1.2 s of that was importing third-party packages (a tracing library, a protocol client, a cloud SDK) that the edit did not touch. The repository's own modules took 0.07 s.

Kept outputs do not help here. A resolver's output is kept by the state of the repository's code ([spec 14](14-sentry.md)), and the edit being checked changes that state. The run has to happen; what it should not have to do is import the same packages again.

## Goal

A run of a repository's Python pays for the repository's own modules, not for its dependencies, and prints what a new interpreter would print.

**Not goals:**

- Running anything before a check asks for it.
- Other languages. Go's warm state is its build cache on disk. TypeScript types do not exist at run time, so a warm process has nothing of them to hold.
- Replacing language servers. They answer lookups; warm runtimes answer checks. They are separate processes, and the daemon supervises both.

## Decisions

| Decision | Choice |
| --- | --- |
| Who keeps a warm runtime | The daemon. If their memory draws complaints, the fallback is to keep them only while sentry is attached |
| How a check reaches one | It asks a daemon that is already running. No daemon is started for it |
| When one warms | After the first run it serves has been answered, never before one |
| What one holds | Modules from the interpreter's own directories that a run was seen to import. Never the repository's code |
| What one serves | `{python} -m NAME ARGS` and `{python} PATH.py ARGS` resolvers, and Pydantic type reads. Anything a shell would have to read runs as before |
| Platforms | macOS and Linux |
| How much one may use | Set by the user, between none and no limit, in memory held and in time kept ([settings](#settings)) |
| Where it is set | `[runtime]` in the user's config file. A project's `.lemma/config.toml` overrides it setting by setting, and may turn warm runtimes on where the user turned them off |
| The name | Warm runtime, after `runtime:` in a description |

The last but one differs from sentry on purpose: a project cannot attach sentry for a user, but it can ask for warm runtimes.

## How a run goes

```
lemma check (its own process)
   │  a resolver runs `{python} -m NAME`, or a type is read with Pydantic
   ▼
is a daemon running, and does [runtime] allow memory?  ── no ──▶ the check runs it itself
   │ yes: one request on the daemon's runtime socket
   ▼
the host for this interpreter and environment
   ├── none yet   start one (it holds the modules kept from before, if any)
   ├── busy       not waited for: the check runs it itself
   └── ready      fork ─▶ the child imports the repository's code and runs
   ▼
exit status 0 ─▶ the child's standard output is the answer
anything else ─▶ the check runs it itself, and reports what that run says
```

**The host** is one Python process per key, running `runtime_host.py`, which Lemma writes into its cache. It forks for each run. The child changes to the run's directory, takes the caller's whole environment, sets `sys.argv` and the head of `sys.path` as `python -m`, `python FILE` or `python -c` would, and runs the module, script or code. When it ends it reports which modules of the interpreter's own directories it had imported. The host adds those to its list, keeps the list on disk, and imports them, so the next fork starts with them.

**The key** is the interpreter, what it reports of its version and installed packages, the variables a description sets for it, and the variables an interpreter reads as it starts: `PATH`, `VIRTUAL_ENV`, the locale, and everything named `PYTHON…`. Two runs with one key start interpreters that differ in nothing a warm one keeps. A new package changes the key, so the next run starts a new host and the old one idles out.

**The runtime socket** is beside the daemon's own, and each request is answered on its own thread. A run therefore never waits behind a lookup that is loading a language server. Like the daemon's, it takes requests only from its user: a run executes what the asker names.

## Staying equal to a new interpreter

- **The repository's code is never in the host.** The host imports only modules whose files are in the standard library or `site-packages`, the directories the interpreter's package stamp watches. An editable install lives outside them, so each child imports it fresh, as it does the repository's own modules. After importing, the host checks every module it holds; if one comes from anywhere else, it says so and is not used again.
- **The host forks only with one thread.** A library that starts a thread as it is imported makes a fork unsafe. The host counts its threads after importing (the system's count, not Python's) and, with more than one, is not used.
- **A relative search path means the run's directory.** The host starts in an empty directory of its own. Search path entries under it were relative when it started, and a child rewrites them to its own directory, as a new interpreter started there would have them.
- **Startup hooks must match.** If a new interpreter in the run's directory would import a different `sitecustomize` or `usercustomize` than the host did, the child ends without running and the check runs it itself.
- **Only a clean exit is an answer.** Any other exit status, a signal, a refusal, a daemon of another build, or a run that takes more than three times the slowest run so far plus five seconds: the check runs the command itself. A failure's message therefore always comes from an ordinary run. A host that fails three times, or cannot be forked from, is given up on for the daemon's life.
- **Same keys as before.** What is kept of a resolver's output is keyed as it was. A warm run only changes how long a miss takes.

What still differs from a new interpreter:

- A module is run with `runpy.run_module`, so `__main__` is a module made for the run, as it is under `python -m` in every way a resolver can see except identity.
- The modules the host itself needs (`json`, `select`, `signal` and others of the standard library) are already imported in the child.
- All children of one host share its hash seed.

## Settings

The trade is memory for time, and it is the user's to set at any point between the two ends.

```toml
[runtime]
memory = "512MB"     # what warm runtimes may hold: "0" keeps none, "none" sets no limit
stop_after = "30m"   # how long an idle one is kept; "never" keeps it as long as the daemon runs
```

- `memory` bounds each host, which stops importing when it holds that much, and all hosts together: beyond it, the least recently used stop first. A host that holds more than a limit made smaller since is started again.
- A project's `.lemma/config.toml` takes the same section and overrides the user's file setting by setting.
- `LEMMA_RUNTIME=off` keeps none warm, whatever either file says.
- Warm runtimes also count toward `[servers] memory_budget` when one is set. Over it, they stop before any language server does: a runtime is rebuilt in a second, a server in many.

Measured on one resolver whose dependencies are 1,696 modules, cold 1.6 to 1.7 s:

| `memory` | Held | Modules | The resolver's run |
| --- | --- | --- | --- |
| `"0"` | nothing | — | 1.6–1.7 s |
| `"24MB"` | 39 MB | 446 | 1.1–1.3 s |
| `"48MB"` | 63 MB | 719 | 0.9–1.3 s |
| `"64MB"` | 65 MB | 747 | 0.8–1.1 s |
| `"96MB"` | 96 MB | 1,280 | 0.6–0.7 s |
| `"128MB"` | 139 MB | 1,675 | 0.20–0.27 s |
| `"512MB"` | 141 MB | 1,696 | 0.17–0.25 s |

Time saved grows evenly with memory held, about 10 ms a megabyte. Importing first the modules that save the most time for their memory was tried and measured no better than importing in the order a run first imports them, so the host uses that order. A host can pass its limit by what one import brings with it; the limit is looked at between imports.

The whole check, after an edit to a Python model, took about 2.7 s with no warm runtime and 1.3 to 1.8 s with one holding everything, over two sessions on a shared machine. The resolver's output was the same bytes warm and cold.

## Limits

- **A check never starts the daemon.** A user who only runs `lemma check` sees no change until a daemon runs: after a lookup, or `lemma daemon install`.
- **The first run in a new host pays what a cold run pays.** With a list kept from before, the host imports it as it starts and then runs: 1.3 to 1.6 s measured, against 1.6 to 1.7 s cold.
- **One run at a time in a host.** A second check asking while the first's run is in progress runs its own command.
- **Threads a library starts are not seen on every system the same way.** On Linux the count is exact. On macOS it comes from `ps`; if that undercounts, a forked child can crash, which is handled as any failed run.
- **Python 3.8 and later.** The host was run on 3.8, 3.10, 3.12, 3.13 and 3.14. On an interpreter it cannot run on, a run ends without an answer and the check runs it itself.
- **The interpreter's own directories are trusted.** A file edited in place in `site-packages`, with no package installed or removed, does not change the stamp, as it does not for kept resolver outputs.

## Where the code is

- `lemma-graph::runtime` holds what a check knows: a run of Python, the key, which commands qualify, and the `WarmRuntime` boundary. It starts no process.
- `lemma-cli::runtime` holds the daemon's side (hosts, the socket, limits, status) and the client a check asks through.
- `lemma-cli/src/runtime_host.py` is the host.

## Prior art

- Python's `multiprocessing` forkserver with preloaded modules: a clean single-threaded server that forks workers.
- Rails' Spring and Android's zygote: an application's dependencies loaded once, each request forked from them.
- Bazel's persistent workers and Gradle's daemon: a warm tool process kept by a supervisor, with a cold fallback.

## Open questions

- Whether the idle timeout should share the language servers' `stop_after`.
- Whether Python lookups (`signature`, `definition`) should be answered from a warm runtime when one is up.
- Asking for one type's schema at a time. A warm run takes about 0.2 s whatever it prints, so there is little left to save.
- The FastAPI adapter and Python checkers, which also start the repository's interpreter.
- A real dependency set on macOS.
