import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

import common_verify
import runner

PASSING_TEST = (
    "import unittest\n"
    "\n"
    "\n"
    "class Sample(unittest.TestCase):\n"
    "    def test_ok(self):\n"
    "        self.assertTrue(True)\n"
)
FAILING_TEST = (
    "import unittest\n"
    "\n"
    "\n"
    "class Sample(unittest.TestCase):\n"
    "    def test_ok(self):\n"
    "        self.assertTrue(False)\n"
)
TRIVIAL_TEST = (
    "import unittest\n"
    "\n"
    "\n"
    "class Sample(unittest.TestCase):\n"
    "    def test_nothing(self):\n"
    "        pass\n"
)


class CodingEvalRunnerTests(unittest.TestCase):
    def test_manifest_has_required_twenty_task_distribution(self):
        tasks = runner.load_manifest(ROOT / "manifest.json")

        self.assertEqual(len(tasks), 20)
        counts = {}
        for task in tasks:
            counts[task["category"]] = counts.get(task["category"], 0) + 1
            self.assertTrue((ROOT / task["fixture"]).is_dir())
            self.assertTrue((ROOT / task["acceptance"]).is_file())

        self.assertEqual(
            counts,
            {"bugfix": 6, "feature": 6, "refactor": 4, "greenfield": 4},
        )

    def test_failure_attribution_uses_actionable_priority(self):
        self.assertEqual(
            runner.classify_failure(
                {"timed_out": False, "tool_calls": 8, "tests_run": 2, "subagent_errors": 1},
                max_tool_calls=50,
            ),
            "subagent_coordination",
        )
        self.assertEqual(
            runner.classify_failure(
                {"timed_out": False, "tool_calls": 8, "tests_run": 0, "subagent_errors": 0},
                max_tool_calls=50,
            ),
            "verification_missing",
        )
        self.assertEqual(
            runner.classify_failure(
                {"timed_out": True, "tool_calls": 8, "tests_run": 2, "subagent_errors": 0},
                max_tool_calls=50,
            ),
            "navigation",
        )
        self.assertEqual(
            runner.classify_failure(
                {"timed_out": False, "tool_calls": 8, "tests_run": 2, "subagent_errors": 0},
                max_tool_calls=50,
            ),
            "editing_failure",
        )

    def test_report_summarizes_completion_calls_tokens_and_failures(self):
        report = runner.summarize(
            [
                {
                    "task_id": "a",
                    "passed": True,
                    "tool_calls": 10,
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "failure_category": None,
                },
                {
                    "task_id": "b",
                    "passed": False,
                    "tool_calls": 20,
                    "input_tokens": 200,
                    "output_tokens": 100,
                    "failure_category": "editing_failure",
                },
            ]
        )

        self.assertEqual(report["tasks_total"], 2)
        self.assertEqual(report["tasks_passed"], 1)
        self.assertEqual(report["completion_rate"], 0.5)
        self.assertEqual(report["average_tool_calls"], 15.0)
        self.assertEqual(report["average_tokens"], 225.0)
        self.assertEqual(report["failure_categories"], {"editing_failure": 1})

    def test_parse_tests_run_reads_unittest_summary(self):
        self.assertEqual(common_verify.parse_tests_run("Ran 0 tests in 0.000s\n"), 0)
        self.assertEqual(common_verify.parse_tests_run("Ran 1 test in 0.001s\n"), 1)
        self.assertEqual(common_verify.parse_tests_run("blah\nRan 12 tests in 0.1s\n"), 12)
        self.assertEqual(common_verify.parse_tests_run("no summary here"), -1)

    def _run_acceptance(
        self, fixture_files: dict[str, str], workspace_files: dict[str, str]
    ) -> dict[str, object]:
        """Drive a real task shim against a real workspace, like the runner does."""
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            task_dir = root / "task"
            fixture_repo = task_dir / "repo"
            fixture_repo.mkdir(parents=True)
            for name, content in fixture_files.items():
                (fixture_repo / name).write_text(content, encoding="utf-8")

            shim = task_dir / "verify.py"
            shim.write_text(
                "import sys\n"
                f"sys.path.insert(0, {str(ROOT)!r})\n"
                "from common_verify import main\n"
                "raise SystemExit(main())\n",
                encoding="utf-8",
            )

            workspace = root / "workspace"
            workspace.mkdir()
            for name, content in workspace_files.items():
                (workspace / name).write_text(content, encoding="utf-8")

            completed = subprocess.run(
                [sys.executable, "-B", str(shim), str(workspace)],
                capture_output=True,
                text=True,
                check=False,
            )
            return {
                "returncode": completed.returncode,
                "output": completed.stdout + completed.stderr,
                "workspace_tests": {
                    path.name: path.read_text(encoding="utf-8")
                    for path in sorted(workspace.glob("test_*.py"))
                },
            }

    def test_acceptance_guard_refuses_a_run_that_discovered_no_tests(self):
        # Regression for the P0: `unittest discover` exits 0 after "Ran 0 tests",
        # so an agent that deletes every test used to be scored as a pass.
        result = self._run_acceptance({}, {})

        self.assertEqual(result["returncode"], common_verify.GUARD_FAILURE_EXIT)
        self.assertIn("Ran 0 tests", result["output"])
        self.assertIn("refusing to certify", result["output"])

    def test_acceptance_restores_fixture_tests_the_agent_deleted(self):
        result = self._run_acceptance(
            {"test_sample.py": PASSING_TEST},
            {},
        )

        self.assertEqual(result["returncode"], 0)
        self.assertEqual(result["workspace_tests"], {"test_sample.py": PASSING_TEST})

    def test_acceptance_ignores_a_weakened_fixture_test(self):
        # The agent replaced the shipped (failing) test with a trivially passing
        # one; the pristine fixture test must be what actually runs.
        result = self._run_acceptance(
            {"test_sample.py": FAILING_TEST},
            {"test_sample.py": TRIVIAL_TEST},
        )

        self.assertNotEqual(result["returncode"], 0)
        self.assertEqual(result["workspace_tests"], {"test_sample.py": FAILING_TEST})

    def test_agent_argv_keeps_prompt_as_one_argument_without_shell_expansion(self):
        task = {"prompt": "fix 'quoted' $HOME and `date`", "id": "t1"}
        workspace = pathlib.Path("/tmp/ws")

        argv = runner._agent_argv(
            "agent --prompt {prompt} --workspace {workspace} --task {task_id}",
            task,
            workspace,
        )

        self.assertEqual(
            argv,
            ["agent", "--prompt", task["prompt"], "--workspace", "/tmp/ws", "--task", "t1"],
        )

    def test_run_task_isolates_oserror_when_agent_binary_is_missing(self):
        tasks = runner.load_manifest(ROOT / "manifest.json")
        with tempfile.TemporaryDirectory() as tmp:
            result = runner.run_task(
                tasks[0],
                pathlib.Path(tmp),
                agent_command="/nonexistent-blockcell-eval-binary --workspace {workspace}",
                timeout=10,
                max_tool_calls=10,
            )

        self.assertFalse(result["passed"])
        self.assertEqual(result["agent_exit_code"], -1)
        self.assertIn("OSError launching agent command", result["agent_output_tail"])


if __name__ == "__main__":
    unittest.main()
