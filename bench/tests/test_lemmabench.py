import json
import tempfile
import unittest
from pathlib import Path

from lemmabench.events import command_name, read_activity
from lemmabench.gold import Definition, added_definitions, changed_definitions, from_polybench, is_test_path
from lemmabench.patches import parse_diff
from lemmabench.runner import parse_resources
from lemmabench.scoring import score
from lemmabench.tasks import IMPACT, LOCALIZATION, Source, Task
from lemmabench.truth import combine

DIFF = """diff --git a/src/router.py b/src/router.py
index 1111111..2222222 100644
--- a/src/router.py
+++ b/src/router.py
@@ -10,3 +10,3 @@ class Router:
     def delete(self, id):
-        return self.routes.pop(id)
+        return self.routes.pop(id, None)

@@ -20,0 +21,3 @@ class Router:
+
+def helper():
+    pass
diff --git a/tests/test_router.py b/tests/test_router.py
--- a/tests/test_router.py
+++ b/tests/test_router.py
@@ -1,1 +1,2 @@
 import router
+--- not a header
"""


def task(track: str, gold: dict) -> Task:
    return Task(id="t", track=track, language="python", source=Source("o/r", "c", "u"), brief="", gold=gold)


class DiffTests(unittest.TestCase):
    def test_removed_lines_and_insertion_points_are_old_side_lines(self):
        router, test = parse_diff(DIFF)

        self.assertEqual(router.path, "src/router.py")
        self.assertEqual(router.changed_lines, {11})
        self.assertEqual(router.insertions, {11, 20})
        self.assertEqual(test.insertions, {1})

    def test_content_that_looks_like_a_header_stays_content(self):
        test = parse_diff(DIFF)[1]

        self.assertEqual(test.path, "tests/test_router.py")


class GoldTests(unittest.TestCase):
    definitions = [Definition("Router", 1, 19), Definition("Router.delete", 10, 12), Definition("other", 21, 25)]

    def test_changed_lines_name_the_innermost_definition(self):
        change = parse_diff(DIFF)[0]
        change.insertions = set()

        self.assertEqual(changed_definitions(change, self.definitions), {"Router.delete"})

    def test_code_inserted_between_definitions_belongs_to_neither(self):
        change = parse_diff(DIFF)[0]
        change.changed_lines = set()
        change.insertions = {20}

        self.assertEqual(changed_definitions(change, self.definitions), set())

    def test_added_definitions_count(self):
        after = [*self.definitions, Definition("Router.__str__", 13, 14, "method")]

        self.assertEqual(added_definitions(self.definitions, after), {"Router.__str__"})

    def test_polybench_nodes_become_qualified_names(self):
        nodes = [
            "a/b.py->module->class_definition:Event->function_definition:__str__",
            "c.ts->program->function_declaration:dom",
        ]

        self.assertEqual(from_polybench(nodes), {"a/b.py::Event.__str__", "c.ts::dom"})

    def test_test_files_are_recognized(self):
        self.assertTrue(is_test_path("tests/test_router.py"))
        self.assertTrue(is_test_path("src/router.spec.ts"))
        self.assertTrue(is_test_path("pkg/server_test.go"))
        self.assertFalse(is_test_path("src/contest.py"))


class ScoringTests(unittest.TestCase):
    def test_localization_scores_files_and_symbols(self):
        gold = {"files": ["src/router.py"], "symbols": ["src/router.py::Router.delete", "src/router.py::Router.add"]}
        answer = {
            "files": ["./src/router.py", "src/app.py"],
            "symbols": [{"path": "src/router.py", "name": "Router.delete"}],
        }

        matches = score(task(LOCALIZATION, gold), answer)

        self.assertEqual(matches["file"].recall, 1.0)
        self.assertEqual(matches["file"].precision, 0.5)
        self.assertEqual(matches["symbol"].precision, 1.0)
        self.assertEqual(matches["symbol"].recall, 0.5)

    def test_impact_scores_lines(self):
        gold = {"locations": ["a.py:3", "b.py:9"]}
        answer = {"locations": [{"path": "a.py", "line": 3}, {"path": "a.py", "line": 4}]}

        matches = score(task(IMPACT, gold), answer)

        self.assertEqual(matches["location"].f1, 0.5)

    def test_unknown_lines_count_neither_way(self):
        gold = {"locations": ["a.py:3"], "unknown": ["b.py:5"]}
        answer = {"locations": [{"path": "a.py", "line": 3}, {"path": "b.py", "line": 5}]}

        matches = score(task(IMPACT, gold), answer)

        self.assertEqual(matches["location"].precision, 1.0)

    def test_a_missing_answer_scores_zero(self):
        matches = score(task(IMPACT, {"locations": ["a.py:3"]}), None)

        self.assertEqual(matches["location"].f1, 0.0)


class TruthTests(unittest.TestCase):
    symbol = {"path": "a.ts", "line": 0}

    def test_confirmed_lines_are_references_and_import_only_lines_unknown(self):
        found = [
            {"path": "a.ts", "line": 0},
            {"path": "b.ts", "line": 4},
            {"path": "c.ts", "line": 1},
            {"path": "d.ts", "line": 2},
        ]
        per_server = [[True, True, "unresolved", False], [True, True, True, "unresolved"]]

        checked = combine(self.symbol, found, per_server)

        self.assertEqual(checked.references, [("b.ts", 5)])
        self.assertEqual(checked.unknown, [("c.ts", 2)])

    def test_a_missing_answer_drops_the_symbol(self):
        found = [{"path": "b.ts", "line": 4}]

        self.assertIsNone(combine(self.symbol, found, [[None]]))


class EventTests(unittest.TestCase):
    def test_tokens_and_tools_are_counted(self):
        events = [
            {"type": "item.completed", "item": {"type": "command_execution", "command": "bash -lc 'rg -n foo'"}},
            {"type": "item.completed", "item": {"type": "mcp_tool_call", "server": "lemma", "tool": "references"}},
            {"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 7}},
            {"type": "something.new"},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("\n".join(json.dumps(e) for e in events) + "\nnot json\n")

            activity = read_activity(path)

        self.assertEqual(activity.tool_calls, 2)
        self.assertEqual(activity.tools["rg"], 1)
        self.assertEqual(activity.tools["mcp:lemma.references"], 1)
        self.assertEqual(activity.uncached_input_tokens, 60)

    def test_commands_are_named_by_their_program(self):
        self.assertEqual(command_name("/bin/bash -lc 'cd src && sed -n 1,5p x'"), "sed")
        self.assertEqual(command_name(["bash", "-lc", "/usr/bin/rg foo"]), "rg")


class ResourceTests(unittest.TestCase):
    def test_systemd_summary_is_parsed(self):
        summary = (
            "Running as unit: x.service\nFinished with result: success\n"
            "Service runtime: 1min 2.5s\nCPU time consumed: 1min 3.250s\nMemory peak: 1.5G\n"
        )

        resources = parse_resources(summary)

        self.assertAlmostEqual(resources["cpu_seconds"], 63.25)
        self.assertEqual(resources["memory_peak_bytes"], int(1.5 * 1024**3))
        self.assertEqual(resources["result"], "success")


if __name__ == "__main__":
    unittest.main()
