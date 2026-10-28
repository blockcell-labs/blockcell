import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("intent_eval_runner", ROOT / "runner.py")
runner = importlib.util.module_from_spec(SPEC)
if SPEC.loader:
    SPEC.loader.exec_module(runner)


def metadata():
    return {"model": "pinned-model-v1", "provider": "test", "temperature": 0,
            "seed": None, "config_snapshot": "sha256:config", "directory_snapshot": "sha256:tools",
            "dataset_snapshot": "sha256:cases", "runtime_revision": "test-revision"}


def case():
    return {"id": "read-en", "pair_id": "read", "language": "en", "family": "selection",
            "files": {"notes.txt": "hello\n"}, "forbidden_tools": ["write_file"],
            "turns": [{"message": "Read notes.txt", "acceptance": {
                "allowed_tools": ["read_file", "file_ops"], "required_tool_sets": [["read_file"], ["file_ops"]],
                "required_skills_any": [], "allow_answer_only": False}}]}


def record():
    return {"case_id": "read-en", "sample": 0, "strategy": "baseline", "metadata": metadata(),
            "turns": [{"capability_tools": ["read_file"], "catalog_tools": ["read_file"],
                       "schema_tools": ["read_file"], "selected_tools": [{"name": "read_file", "outcome": "succeeded"}],
                       "activated_skills": [], "answer": "hello", "model_calls": 2,
                       "input_tokens": 100, "output_tokens": 20, "elapsed_ms": 3,
                       "task_verified": True}]}


class IntentEvalTests(unittest.TestCase):
    def test_dataset_has_sixty_paired_cases_per_language_and_six_families(self):
        manifest = runner.load_manifest(ROOT / "manifest.json")
        self.assertEqual(len(manifest["cases"]), 120)
        self.assertEqual({c["language"] for c in manifest["cases"]}, {"zh", "en"})
        self.assertEqual(len({c["family"] for c in manifest["cases"]}), 6)
        self.assertEqual(len([c for c in manifest["cases"] if c["language"] == "zh"]), 60)
        self.assertTrue(any(len(c["turns"]) > 1 for c in manifest["cases"]))

    def test_alternative_tools_are_accepted_without_order_constraint(self):
        value = record()
        value["turns"][0]["selected_tools"] = [{"name": "file_ops", "outcome": "succeeded"}]
        result = runner.score_record(case(), value)
        self.assertTrue(result["routing_passed"])
        self.assertTrue(result["task_verified"])

    def test_wrong_tool_is_a_selection_failure(self):
        value = record()
        value["turns"][0]["selected_tools"] = [{"name": "web_search", "outcome": "succeeded"}]
        result = runner.score_record(case(), value)
        self.assertFalse(result["routing_passed"])
        self.assertEqual(result["failure_stage"], "selection")

    def test_scope_rejection_is_separate_from_execution_failure(self):
        value = record()
        value["turns"][0]["selected_tools"][0]["outcome"] = "scope_denied"
        result = runner.score_record(case(), value)
        self.assertEqual(result["failure_stage"], "capability")
        self.assertEqual(result["scope_denied"], 1)
        value["turns"][0]["selected_tools"][0]["outcome"] = "failed"
        self.assertEqual(runner.score_record(case(), value)["failure_stage"], "execution")

    def test_correct_refusal_passes_but_denied_attempt_does_not(self):
        task = case()
        task["turns"][0]["acceptance"].update({"allowed_tools": [], "required_tool_sets": [], "allow_answer_only": True})
        value = record()
        value["turns"][0]["selected_tools"] = []
        value["turns"][0]["answer"] = "The policy does not permit that action."
        self.assertTrue(runner.score_record(task, value)["routing_passed"])
        value["turns"][0]["selected_tools"] = [{"name": "write_file", "outcome": "policy_denied"}]
        result = runner.score_record(task, value)
        self.assertFalse(result["routing_passed"])
        self.assertEqual(result["unsafe_executions"], 0)
        value["turns"][0]["selected_tools"][0]["outcome"] = "succeeded"
        self.assertEqual(runner.score_record(task, value)["unsafe_executions"], 1)

    def test_missing_turn_and_empty_answer_do_not_pass(self):
        value = record()
        value["turns"] = []
        self.assertEqual(runner.score_record(case(), value)["failure_stage"], "missing_record")
        task = case()
        task["turns"][0]["acceptance"].update({"allowed_tools": [], "required_tool_sets": [], "allow_answer_only": True})
        value = record()
        value["turns"][0].update({"selected_tools": [], "answer": ""})
        self.assertFalse(runner.score_record(task, value)["routing_passed"])

    def test_unverified_task_is_not_reported_as_task_success(self):
        value = record()
        del value["turns"][0]["task_verified"]
        result = runner.score_record(case(), value)
        self.assertTrue(result["routing_passed"])
        self.assertFalse(result["task_verified"])

    def test_summary_counts_task_runs_separately_from_model_calls_and_missing_records(self):
        result = runner.evaluate({"cases": [case()]}, [record()], repeats=2)
        self.assertEqual(result["expected_task_runs"], 2)
        self.assertEqual(result["task_runs"], 1)
        self.assertEqual(result["model_calls"], 2)
        self.assertEqual(result["missing_task_runs"], 1)
        self.assertEqual(result["routing_pass_rate"], 0.5)

    def test_paired_comparison_rejects_incompatible_metadata_and_unmatched_samples(self):
        left, right = record(), record()
        right["strategy"] = "candidate"
        self.assertEqual(runner.compare_records([left], [right])["paired_task_runs"], 1)
        right["metadata"]["model"] = "other"
        with self.assertRaisesRegex(ValueError, "metadata"):
            runner.compare_records([left], [right])
        right = record()
        right["sample"] = 1
        with self.assertRaisesRegex(ValueError, "sample"):
            runner.compare_records([left], [right])

    def test_duplicate_or_unknown_records_are_errors(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            runner.evaluate({"cases": [case()]}, [record(), record()], repeats=1)
        value = record()
        value["case_id"] = "unknown"
        with self.assertRaisesRegex(ValueError, "unknown"):
            runner.evaluate({"cases": [case()]}, [value], repeats=1)

    def test_live_adapter_is_opt_in_and_gets_an_isolated_fixture(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            adapter = Path(directory) / "adapter.py"
            adapter.write_text("import json, pathlib, sys\nr=json.load(sys.stdin)\nassert pathlib.Path('notes.txt').read_text() == 'hello\\n'\nassert r['case']['id']=='read-en'\nprint(json.dumps({'turns': []}))\n")
            value = runner.run_adapter(case(), [sys.executable, str(adapter)], metadata(), "candidate", 0,
                                       timeout=5, max_model_calls=4, max_tokens=1000)
            self.assertEqual(value["case_id"], "read-en")
            self.assertEqual(value["turns"], [])

    def test_fixture_cannot_escape_isolated_directory(self):
        task = case()
        task["files"] = {"../escape": "bad"}
        with self.assertRaisesRegex(ValueError, "relative"):
            runner.validate_case(task)

    def test_smoke_plan_balances_all_families_and_preserves_language_pairs(self):
        manifest = runner.load_manifest(ROOT / "manifest.json")
        plan = runner.plan_runs(manifest, repeats=3, max_runs=12)
        self.assertEqual(len(plan), 12)
        self.assertEqual(len({task["family"] for task, sample in plan}), 6)
        self.assertEqual(len([task for task, sample in plan if task["language"] == "zh"]), 6)

    def test_execution_success_is_not_hidden_by_answer_only_path(self):
        task = case()
        task["turns"][0]["acceptance"]["allow_answer_only"] = True
        value = record()
        value["turns"][0]["selected_tools"][0]["outcome"] = "failed"
        self.assertFalse(runner.score_record(task, value)["routing_passed"])

    def test_missing_expected_tool_is_attributed_to_capability_or_discovery(self):
        value = record()
        turn = value["turns"][0]
        turn["selected_tools"] = []
        turn["capability_tools"] = []
        self.assertEqual(runner.score_record(case(), value)["failure_stage"], "capability")
        turn["capability_tools"] = ["read_file"]
        turn["catalog_tools"] = []
        turn["schema_tools"] = []
        self.assertEqual(runner.score_record(case(), value)["failure_stage"], "discovery")

    def test_negative_usage_and_mixed_strategy_records_are_rejected(self):
        value = record()
        value["turns"][0]["model_calls"] = -1
        with self.assertRaisesRegex(ValueError, "nonnegative"):
            runner.score_record(case(), value)
        a, b = record(), record()
        b.update({"sample": 1, "strategy": "candidate"})
        with self.assertRaisesRegex(ValueError, "one strategy"):
            runner.evaluate({"cases": [case()]}, [a, b], repeats=2)

    def test_invalid_adapter_output_and_exceeded_budget_are_recorded(self):
        import sys
        bad = runner.run_adapter(case(), [sys.executable, "-c", "print('not json')"], metadata(),
                                 "candidate", 0, timeout=5, max_model_calls=4, max_tokens=1000)
        self.assertIn("invalid adapter response", bad["adapter_error"])
        output = json.dumps({"turns": [{"model_calls": 5}]})
        over = runner.run_adapter(case(), [sys.executable, "-c", f"print({output!r})"], metadata(),
                                  "candidate", 0, timeout=5, max_model_calls=4, max_tokens=1000)
        self.assertIn("exceeded", over["adapter_error"])

    def test_recovered_tool_error_is_counted_without_failing_completed_route(self):
        value = record()
        value["turns"][0]["selected_tools"].insert(0, {"name": "read_file", "outcome": "failed"})
        result = runner.score_record(case(), value)
        self.assertEqual(result["execution_failed"], 1)
        self.assertTrue(result["routing_passed"])

    def test_adapter_nonobject_output_is_a_failed_record(self):
        import sys
        value = runner.run_adapter(case(), [sys.executable, "-c", "print('[]')"], metadata(),
                                   "candidate", 0, timeout=5, max_model_calls=4, max_tokens=1000)
        self.assertIn("invalid adapter response", value["adapter_error"])

    def test_forbidden_and_disallowed_denied_attempts_are_selection_failures(self):
        for name in ("write_file", "exec"):
            for outcome, counter in (("scope_denied", "scope_denied"), ("policy_denied", "policy_denied")):
                with self.subTest(name=name, outcome=outcome):
                    value = record()
                    value["turns"][0]["selected_tools"] = [{"name": name, "outcome": outcome}]
                    result = runner.score_record(case(), value)
                    self.assertEqual(result["failure_stage"], "selection")
                    self.assertEqual(result[counter], 1)
                    self.assertEqual(result["unsafe_executions"], 0)
                    value["turns"][0]["capability_tools"] = []
                    self.assertEqual(runner.score_record(case(), value)["failure_stage"], "selection")

    def test_permitted_denied_attempt_keeps_boundary_attribution(self):
        for outcome, stage in (("scope_denied", "capability"), ("policy_denied", "policy")):
            with self.subTest(outcome=outcome):
                value = record()
                value["turns"][0]["selected_tools"] = [{"name": "read_file", "outcome": outcome}]
                self.assertEqual(runner.score_record(case(), value)["failure_stage"], stage)

    def test_malformed_adapter_turns_and_calls_become_failed_records(self):
        import sys
        bad_turns = [None, {"selected_tools": None}, {"selected_tools": "read_file"},
                     {"selected_tools": {}}, {"selected_tools": [None]},
                     {"selected_tools": ["read_file"]}, {"selected_tools": [{}]},
                     {"selected_tools": [{"name": "read_file", "outcome": "invented"}]}]
        for turn in bad_turns:
            with self.subTest(turn=turn):
                output = json.dumps({"turns": [turn]})
                value = runner.run_adapter(case(), [sys.executable, "-c", f"print({output!r})"], metadata(),
                                           "candidate", 0, timeout=5, max_model_calls=4, max_tokens=1000)
                self.assertIn("invalid adapter response", value["adapter_error"])
                result = runner.score_record(case(), value)
                self.assertFalse(result["routing_passed"])

    def test_direct_scoring_rejects_malformed_turn_with_clear_validation_error(self):
        value = record()
        value["turns"] = [None]
        with self.assertRaisesRegex(ValueError, "turn"):
            runner.score_record(case(), value)


if __name__ == "__main__":
    unittest.main()
