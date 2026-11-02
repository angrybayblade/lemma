import subprocess
import tempfile
import unittest
from pathlib import Path

from lemmabench.toolserver import ToolError, call, handle


class ToolServerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name).resolve()
        (self.root / "src").mkdir()
        (self.root / "src" / "app.py").write_text("def delete():\n    return 1\n\n# delete later\n")
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "-A"], cwd=self.root, check=True)

    def tearDown(self):
        self.directory.cleanup()

    def test_files_are_read_with_line_numbers(self):
        text = call(self.root, "none", "read_file", {"path": "src/app.py", "start_line": 2, "end_line": 2})

        self.assertTrue(text.startswith("2:     return 1"))
        self.assertIn("(lines 2-2 of 4)", text)

    def test_paths_outside_the_repository_are_refused(self):
        with self.assertRaises(ToolError):
            call(self.root, "none", "read_file", {"path": "../secret"})

    def test_grep_reports_relative_paths(self):
        text = call(self.root, "grep", "grep", {"pattern": "delete", "word": True})

        self.assertIn("src/app.py:1:def delete():", text)

    def test_tools_outside_the_arm_are_unknown(self):
        with self.assertRaises(ToolError):
            call(self.root, "none", "grep", {"pattern": "x"})

    def test_only_the_arms_search_tool_is_listed(self):
        reply = handle({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, self.root, "ripgrep")

        names = [tool["name"] for tool in reply["result"]["tools"]]
        self.assertEqual(names, ["read_file", "list_files", "ripgrep"])

    def test_tool_errors_are_returned_as_results(self):
        message = {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "read_file", "arguments": {"path": "missing.py"}},
        }

        reply = handle(message, self.root, "none")

        self.assertTrue(reply["result"]["isError"])


if __name__ == "__main__":
    unittest.main()
