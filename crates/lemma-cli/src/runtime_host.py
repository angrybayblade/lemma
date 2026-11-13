"""Lemma's warm runtime host: one Python process, kept by the daemon, that
runs a repository's Python for checks without paying for its dependencies'
imports each time.

The host holds only modules from the interpreter's own directories (the
standard library and site-packages). For each run it forks; the child
imports the repository's code fresh, runs, and exits. The host never imports
the repository's code, so an edit is always seen, and it forks only while it
has a single thread.

It speaks JSON lines with the daemon: requests on its standard input, answers
on its standard output. See `.specs/20-warm-runtimes.md`.
"""

import atexit
import importlib
import importlib.util
import json
import os
import resource
import runpy
import select
import signal
import site
import subprocess
import sys
import sysconfig
import threading
import traceback
import types

STARTUP_HOOKS = ("sitecustomize", "usercustomize")
"""Modules an interpreter imports as it starts, from wherever its search path
finds them: a system's own may live outside the interpreter's directories."""


def own_directories():
    """The interpreter's own directories: where the standard library and
    installed packages are, which is what the daemon's package stamp watches."""
    found = set()
    for name in ("stdlib", "platstdlib", "purelib", "platlib"):
        found.add(sysconfig.get_paths().get(name))
    try:
        found.update(site.getsitepackages())
    except AttributeError:
        pass
    if site.ENABLE_USER_SITE:
        found.add(site.getusersitepackages())
    return tuple(sorted(os.path.realpath(path) + os.sep for path in found if path))


def origin_of(module):
    """Where a module came from: a file, `built-in`, `frozen`, or nothing."""
    spec = getattr(module, "__spec__", None)
    origin = getattr(spec, "origin", None)
    return origin or getattr(module, "__file__", None)


def is_own(module, directories):
    """Whether a module is the interpreter's own: built in, or a file in one
    of its directories. A package with no file of its own counts by its
    search path. `None` for a module with neither, which some other module
    made as it loaded (Cython's runtime, a lazy alias): it holds no code of
    its own to go stale, and cannot be imported by name."""
    origin = origin_of(module)
    if origin in ("built-in", "frozen"):
        return True
    if origin is None or not os.path.isabs(str(origin)):
        paths = list(getattr(module, "__path__", []) or [])
        if not paths:
            return None
        return all((os.path.realpath(path) + os.sep).startswith(directories) for path in paths)
    return os.path.realpath(origin).startswith(directories)


def memory_bytes():
    """The most memory this process has held."""
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return peak if sys.platform == "darwin" else peak * 1024


def thread_count():
    """How many threads this process has, counting ones a library started
    without Python knowing."""
    if sys.platform.startswith("linux"):
        return len(os.listdir("/proc/self/task"))
    try:
        listed = subprocess.run(
            ["ps", "-M", "-p", str(os.getpid())], capture_output=True, text=True, check=False
        ).stdout.strip().splitlines()
        return max(len(listed) - 1, threading.active_count())
    except OSError:
        return threading.active_count()


class Host:
    def __init__(self, answers, list_path, limit_bytes):
        self.answers = answers
        self.list_path = list_path
        self.limit_bytes = limit_bytes
        self.directories = own_directories()
        self.own_file = os.path.realpath(__file__)
        self.home = os.getcwd()
        self.known = []
        self.running = {}
        self.unsafe = None
        self.threads = 1

    def say(self, message):
        os.write(self.answers, (json.dumps(message) + "\n").encode())

    def state(self):
        return {
            "state": {
                "modules": sum(1 for name in self.known if name in sys.modules),
                "known": len(self.known),
                "memory_bytes": memory_bytes(),
                "unsafe": self.unsafe,
            }
        }

    def read_list(self):
        try:
            with open(self.list_path, encoding="utf-8") as kept:
                self.known = [name for name in json.load(kept) if isinstance(name, str)]
        except (OSError, ValueError):
            self.known = []

    def keep_list(self):
        partial = f"{self.list_path}.{os.getpid()}.partial"
        try:
            with open(partial, "w", encoding="utf-8") as kept:
                json.dump(self.known, kept)
            os.replace(partial, self.list_path)
        except OSError:
            pass

    def preload(self):
        """Imports the known modules not held yet, in the order a run first
        imported them, until the memory limit; then looks at what the host
        now holds and how many threads it has. What importing saves a run
        grows evenly with the memory it takes, so the order is no worse
        than any other under a limit."""
        if self.unsafe:
            return
        for name in self.known:
            if memory_bytes() >= self.limit_bytes:
                break
            if name in sys.modules:
                continue
            try:
                importlib.import_module(name)
            except KeyboardInterrupt:
                raise
            except BaseException:  # noqa: BLE001 - a module that cannot load is only not held
                pass
        self.look()

    def look(self):
        """Gives up being forked from if the host holds code from outside the
        interpreter's own directories, or has more than one thread."""
        for name, module in list(sys.modules.items()):
            if module is None or name == "__main__" or name in STARTUP_HOOKS:
                continue
            if origin_of(module) and os.path.realpath(str(origin_of(module))) == self.own_file:
                continue
            if is_own(module, self.directories) is False:
                self.unsafe = f"module {name} from {origin_of(module)} is not the interpreter's own"
                return
        self.threads = thread_count()
        if self.threads > 1:
            self.unsafe = f"{self.threads} threads after importing"

    def learn(self, modules_path):
        """Adds what a finished run imported from the interpreter's own
        directories to the known modules, and holds them too."""
        try:
            with open(modules_path, encoding="utf-8") as reported:
                names = json.load(reported)
        except (OSError, ValueError):
            return
        known = set(self.known)
        added = [name for name in names if isinstance(name, str) and name not in known]
        if not added:
            return
        self.known.extend(added)
        self.keep_list()
        self.preload()

    def start(self, request):
        request["home"] = self.home
        if self.unsafe:
            self.say({"id": request["id"], "refused": self.unsafe})
            return
        sys.stdout.flush()
        sys.stderr.flush()
        child = os.fork()
        if child == 0:
            run_as_child(request, self.directories)
        self.running[child] = request

    def reap(self):
        while self.running:
            try:
                child, status = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if child == 0:
                return
            request = self.running.pop(child, None)
            if request is None:
                continue
            answer = {"id": request["id"]}
            if os.WIFSIGNALED(status):
                answer["signal"] = os.WTERMSIG(status)
            else:
                answer["exit"] = os.WEXITSTATUS(status)
            self.say(answer)
            if answer.get("exit") == 0:
                self.learn(request["modules"])
                self.say(self.state())

    def stop_run(self, identifier):
        for child, request in self.running.items():
            if request["id"] == identifier:
                try:
                    os.killpg(child, signal.SIGKILL)
                except OSError:
                    pass


def exit_code_of(raised):
    """The exit status `SystemExit` asks for, as the interpreter gives it."""
    code = raised.code
    if code is None:
        return 0
    if isinstance(code, int):
        return code & 0xFF
    print(code, file=sys.stderr)
    return 1


def execute(request):
    """Runs what a request names as `python -m`, `python FILE` or
    `python -c` would in a new interpreter; its exit status."""
    kind, target, arguments = request["kind"], request["target"], request["args"]
    try:
        if kind == "module":
            sys.argv = [target, *arguments]
            if not sys.flags.safe_path:
                sys.path.insert(0, os.getcwd())
            runpy.run_module(target, run_name="__main__", alter_sys=True)
        elif kind == "script":
            sys.argv = [target, *arguments]
            if not sys.flags.safe_path:
                sys.path.insert(0, os.path.dirname(os.path.realpath(target)))
            runpy.run_path(target, run_name="__main__")
        else:
            sys.argv = ["-c", *arguments]
            if not sys.flags.safe_path:
                sys.path.insert(0, "")
            main = types.ModuleType("__main__")
            sys.modules["__main__"] = main
            exec(compile(target, "<string>", "exec"), main.__dict__)  # noqa: S102 - what `python -c` does
    except SystemExit as raised:
        return exit_code_of(raised)
    except BaseException:  # noqa: BLE001 - reported as the interpreter reports it
        traceback.print_exc()
        return 1
    return 0


def search_from_here(home):
    """Makes search path entries that were relative when the host started
    relative to the run's directory, as a new interpreter started here would
    have them: the host started in `home`, a directory of its own."""
    here = os.getcwd()
    for index, entry in enumerate(sys.path):
        if entry == home or entry.startswith(home + os.sep):
            sys.path[index] = here + entry[len(home) :]
    for cached in list(sys.path_importer_cache):
        if cached == home or cached.startswith(home + os.sep):
            del sys.path_importer_cache[cached]


def starts_as_the_host_did():
    """Whether an interpreter started in this directory would import the
    startup hooks the host did: with a relative search path it may find
    others, and then the host is no stand-in for it."""
    for name in STARTUP_HOOKS:
        try:
            found = getattr(importlib.util.find_spec(name), "origin", None)
        except (ImportError, ValueError):
            found = None
        held = origin_of(sys.modules[name]) if sys.modules.get(name) else None
        if found != held:
            print(f"a new interpreter would import {name} from {found}", file=sys.stderr)
            return False
    return True


def run_as_child(request, directories):
    """The forked child: becomes the run, and never returns."""
    code = 1
    try:
        signal.set_wakeup_fd(-1)
        signal.signal(signal.SIGCHLD, signal.SIG_DFL)
        os.setpgid(0, 0)
        for path, descriptor in ((request["stdout"], 1), (request["stderr"], 2)):
            opened = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            os.dup2(opened, descriptor)
            os.close(opened)
        os.chdir(request["cwd"])
        search_from_here(request["home"])
        os.environ.clear()
        os.environ.update(request["env"])
        code = execute(request) if starts_as_the_host_did() else 1
        own = [
            name
            for name, module in list(sys.modules.items())
            if module is not None and name != "__main__" and is_own(module, directories) is True
        ]
        with open(request["modules"], "w", encoding="utf-8") as reported:
            json.dump(own, reported)
        # What the interpreter does as it ends: wait for the run's threads,
        # then run what it registered to run at exit.
        threading._shutdown()  # noqa: SLF001  # pyright: ignore[reportAttributeAccessIssue]
        atexit._run_exitfuncs()  # noqa: SLF001
    except BaseException:  # noqa: BLE001 - the child must end here whatever happened
        traceback.print_exc()
        code = code or 1
    finally:
        try:
            sys.stdout.flush()
            sys.stderr.flush()
        except (OSError, ValueError):
            pass
        os._exit(code)


def serve(requests, host, wake):
    """Answers requests until the daemon closes the host's input."""
    pending = b""
    while True:
        try:
            ready, _, _ = select.select([requests, wake], [], [])
        except InterruptedError:
            continue
        if wake in ready:
            os.read(wake, 4096)
            host.reap()
        if requests not in ready:
            continue
        received = os.read(requests, 65536)
        if not received:
            return
        pending += received
        while b"\n" in pending:
            line, pending = pending.split(b"\n", 1)
            request = json.loads(line)
            if "stop" in request:
                host.stop_run(request["stop"])
            else:
                host.limit_bytes = request.get("limit_bytes", host.limit_bytes)
                host.start(request)


def main():
    list_path, limit_bytes = sys.argv[1], int(sys.argv[2])
    own_directory = os.path.dirname(os.path.realpath(__file__))
    if sys.path and os.path.realpath(sys.path[0] or ".") == own_directory:
        # The host's own directory is no place a run would import from.
        del sys.path[0]
    # The daemon's pipes are the host's alone: what a module prints while it
    # is imported must not reach them, and a run gets files of its own.
    requests, answers = os.dup(0), os.dup(1)
    null = os.open(os.devnull, os.O_RDWR)
    os.dup2(null, 0)
    os.dup2(2, 1)
    os.close(null)
    wake, woken = os.pipe()
    os.set_blocking(woken, False)
    signal.set_wakeup_fd(woken)
    signal.signal(signal.SIGCHLD, lambda *_: None)
    host = Host(answers, list_path, limit_bytes)
    host.read_list()
    host.preload()
    host.say(host.state())
    serve(requests, host, wake)
    for child in list(host.running):
        try:
            os.killpg(child, signal.SIGKILL)
        except OSError:
            pass


if __name__ == "__main__":
    main()
