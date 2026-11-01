"""Ground truth: is each candidate occurrence a reference to its target?

One language server serves every job of a (corpus, language) pair. It is
started, given time to load the project, then asked textDocument/definition
at every candidate. A candidate is a reference when any returned location is
its job's target name position. A server that dies is restarted and the
candidate retried once; restarts are reported.

Input (stdin, JSON):
  {"root", "command": [...], "language_id", "ready": [glob patterns],
   "settle_seconds": n,
   "jobs": [{"target": {"path", "line", "column"},
             "candidates": [{"path", "line", "column"}]}]}
  Positions are zero-based; columns count UTF-8 bytes.
Output (stdout, JSON): {"verdicts": [[true|false|null|"unresolved", ...] per job],
                        "seconds", "restarts"}
null means the server did not answer for that candidate. "unresolved" means
its answer stopped at an import statement: the server could not follow the
import (an unresolved module or path alias), which says nothing either way.
"""

import collections
import fnmatch
import json
import pathlib
import re
import subprocess
import sys
import threading
import time

MAX_OPEN = 32
REQUEST_TIMEOUT = 120


class ServerGone(Exception):
    pass


class Server:
    def __init__(self, spec, root):
        self.spec, self.root = spec, root
        self.patterns = spec.get("ready", [])
        self.texts = {}
        self.start()

    def start(self):
        self.proc = subprocess.Popen(
            self.spec["command"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            cwd=self.root,
        )
        self.responses, self.lock = {}, threading.Condition()
        self.state = {"ready": False, "progress": {}, "last": time.time(), "alive": True}
        self.next_id = 0
        self.open = collections.OrderedDict()
        threading.Thread(target=self.reader, daemon=True).start()
        uri = self.root.as_uri()
        self.request(
            "initialize",
            {
                "processId": None,
                "rootUri": uri,
                "workspaceFolders": [{"uri": uri, "name": "w"}],
                "capabilities": {
                    "workspace": {"configuration": True},
                    "window": {"workDoneProgress": True},
                    "textDocument": {"definition": {"linkSupport": True}},
                },
            },
        )
        self.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    def send(self, message):
        body = json.dumps(message).encode()
        try:
            self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise ServerGone() from error

    def is_ready_text(self, text):
        return any(fnmatch.fnmatchcase(text, pattern) for pattern in self.patterns)

    def reader(self):
        stdout = self.proc.stdout
        while True:
            headers = {}
            while True:
                line = stdout.readline()
                if not line:
                    with self.lock:
                        self.state["alive"] = False
                        self.lock.notify_all()
                    return
                line = line.decode().strip()
                if not line:
                    break
                key, value = line.split(":", 1)
                headers[key] = value.strip()
            message = json.loads(stdout.read(int(headers["Content-Length"])))
            with self.lock:
                self.state["last"] = time.time()
                method = message.get("method")
                if "id" in message and method:
                    params = message.get("params") or {}
                    items = params.get("items", []) if isinstance(params, dict) else []
                    result = [None] * len(items) if method == "workspace/configuration" else None
                    try:
                        self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})
                    except ServerGone:
                        pass
                elif "id" in message:
                    self.responses[message["id"]] = message
                elif method == "window/logMessage" and self.is_ready_text(message["params"]["message"]):
                    self.state["ready"] = True
                elif method == "$/progress":
                    token, value = str(message["params"]["token"]), message["params"]["value"]
                    if value.get("kind") == "begin":
                        self.state["progress"][token] = value.get("title", "")
                    elif value.get("kind") == "end" and self.is_ready_text(self.state["progress"].pop(token, "")):
                        self.state["ready"] = True
                self.lock.notify_all()

    def request(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.time() + REQUEST_TIMEOUT
        with self.lock:
            while request_id not in self.responses:
                if not self.state["alive"]:
                    raise ServerGone()
                if not self.lock.wait(timeout=max(0.0, deadline - time.time())):
                    return None
            return self.responses.pop(request_id)

    def text_of(self, path):
        if path not in self.texts:
            self.texts[path] = (self.root / path).read_text(errors="replace")
        return self.texts[path]

    def show(self, path):
        if path in self.open:
            self.open.move_to_end(path)
            return
        self.open[path] = True
        self.send(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": (self.root / path).as_uri(),
                        "languageId": self.spec["language_id"],
                        "version": 1,
                        "text": self.text_of(path),
                    }
                },
            }
        )
        while len(self.open) > MAX_OPEN:
            old, _ = self.open.popitem(last=False)
            self.send(
                {
                    "jsonrpc": "2.0",
                    "method": "textDocument/didClose",
                    "params": {"textDocument": {"uri": (self.root / old).as_uri()}},
                }
            )

    def wait_ready(self, first_path):
        self.show(first_path)
        start = time.time()
        settle = self.spec.get("settle_seconds", 5)
        while True:
            with self.lock:
                quiet = time.time() - self.state["last"]
                elapsed = time.time() - start
                if (
                    self.state["ready"]
                    or (elapsed > settle and quiet > 3 and not self.state["progress"])
                    or elapsed > 300
                    or not self.state["alive"]
                ):
                    return
            time.sleep(0.05)

    def utf16_column(self, path, line, byte_column):
        lines = self.text_of(path).split("\n")
        if line >= len(lines):
            return byte_column
        prefix = lines[line].encode()[:byte_column].decode(errors="replace")
        return len(prefix.encode("utf-16-le")) // 2

    def definition(self, candidate):
        self.show(candidate["path"])
        return self.request(
            "textDocument/definition",
            {
                "textDocument": {"uri": (self.root / candidate["path"]).as_uri()},
                "position": {
                    "line": candidate["line"],
                    "character": self.utf16_column(candidate["path"], candidate["line"], candidate["column"]),
                },
            },
        )

    def stop(self):
        try:
            self.proc.kill()
        except OSError:
            pass


IMPORT_LINE = re.compile(r"^\s*(import\b|export\b.*\bfrom\b|from\s+\S+\s+import\b)|\brequire\(")


def judge(result, target_path, target_line, root):
    """True if the answer points at the target, "unresolved" if it only
    reaches import statements, False otherwise."""
    if points_at(result, target_path, target_line):
        return True
    locations = result if isinstance(result, list) else [result] if result else []
    lines = [location_line(location) for location in locations]
    if lines and all(line is not None and IMPORT_LINE.search(line) for line in lines):
        return "unresolved"
    return False


def location_line(location):
    uri_text = location.get("targetUri") or location.get("uri")
    line_range = location.get("targetSelectionRange") or location.get("range")
    if not uri_text or not line_range:
        return None
    try:
        text = pathlib.Path(uri_text.replace("file://", "")).read_text(errors="replace")
    except OSError:
        return None
    lines = text.split("\n")
    number = line_range["start"]["line"]
    if number >= len(lines):
        return None
    # An import can span lines (`import {\n  A,\n} from "x"`): look back to its start.
    for back in range(number, max(-1, number - 30), -1):
        if IMPORT_LINE.search(lines[back]):
            return lines[back]
        if back != number and lines[back].rstrip().endswith((";", "}")) and "{" not in lines[back]:
            break
    return lines[number]


def points_at(result, target_path, target_line):
    if not result:
        return False
    locations = result if isinstance(result, list) else [result]
    for location in locations:
        uri_text = location.get("targetUri") or location.get("uri")
        line_range = location.get("targetSelectionRange") or location.get("range")
        if not uri_text or not line_range:
            continue
        path = pathlib.Path(uri_text.replace("file://", "")).resolve()
        if path == target_path and line_range["start"]["line"] == target_line:
            return True
    return False


def main():
    spec = json.load(sys.stdin)
    root = pathlib.Path(spec["root"]).resolve()
    jobs = spec["jobs"]
    start = time.time()
    server = Server(spec, root)
    first_path = jobs[0]["target"]["path"] if jobs else None
    if first_path:
        server.wait_ready(first_path)
    restarts = 0
    all_verdicts = []
    for job in jobs:
        target = job["target"]
        target_path = (root / target["path"]).resolve()
        verdicts = []
        for candidate in job["candidates"]:
            answer = None
            for _attempt in range(2):
                try:
                    answer = server.definition(candidate)
                    break
                except ServerGone:
                    restarts += 1
                    server.stop()
                    server = Server(spec, root)
                    server.wait_ready(candidate["path"])
            if answer is None or "error" in answer:
                verdicts.append(None)
            else:
                verdicts.append(judge(answer.get("result"), target_path, target["line"], root))
        all_verdicts.append(verdicts)
    server.stop()
    print(json.dumps({"verdicts": all_verdicts, "seconds": time.time() - start, "restarts": restarts}))


if __name__ == "__main__":
    main()
