#!/usr/bin/env python3
"""Repeatable BlockCell coding evaluation runner using only Python stdlib."""

from __future__ import annotations

import argparse
import collections
import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any

from common_verify import parse_tests_run, restore_fixture_tests

ROOT = pathlib.Path(__file__).resolve().parent
METRICS_FILE = ".blockcell-eval-metrics.json"
REQUIRED_FIELDS = {"id", "category", "prompt", "fixture", "acceptance"}


def load_manifest(path: pathlib.Path) -> list[dict[str, Any]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    tasks = data.get("tasks")
    if not isinstance(tasks, list):
        raise ValueError("manifest.tasks must be an array")
    seen: set[str] = set()
    for task in tasks:
        missing = REQUIRED_FIELDS.difference(task)
        if missing:
            raise ValueError(f"task missing fields: {sorted(missing)}")
        if task["id"] in seen:
            raise ValueError(f"duplicate task id: {task['id']}")
        seen.add(task["id"])
    return tasks


def classify_failure(metrics: dict[str, Any], max_tool_calls: int) -> str:
    if int(metrics.get("subagent_errors", 0)) > 0:
        return "subagent_coordination"
    if int(metrics.get("tests_run", 0)) == 0:
        return "verification_missing"
    if bool(metrics.get("timed_out")) or int(metrics.get("tool_calls", 0)) >= max_tool_calls:
        return "navigation"
    return "editing_failure"


def summarize(results: list[dict[str, Any]]) -> dict[str, Any]:
    total = len(results)
    passed = sum(1 for result in results if result["passed"])
    failures = collections.Counter(
        result["failure_category"]
        for result in results
        if result.get("failure_category")
    )
    return {
        "tasks_total": total,
        "tasks_passed": passed,
        "completion_rate": round(passed / total, 4) if total else 0.0,
        "average_tool_calls": round(
            sum(float(result.get("tool_calls", 0)) for result in results) / total, 2
        )
        if total
        else 0.0,
        "average_tokens": round(
            sum(
                float(result.get("input_tokens", 0))
                + float(result.get("output_tokens", 0))
                for result in results
            )
            / total,
            2,
        )
        if total
        else 0.0,
        "failure_categories": dict(sorted(failures.items())),
        "results": results,
    }


def _run(command: list[str], cwd: pathlib.Path, timeout: int) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )


def _prepare_workspace(task: dict[str, Any], root: pathlib.Path) -> pathlib.Path:
    source = ROOT / task["fixture"]
    workspace = root / task["id"]
    shutil.copytree(source, workspace)
    _run(["git", "init", "-q"], workspace, 10)
    _run(["git", "config", "user.email", "coding-eval@blockcell.local"], workspace, 10)
    _run(["git", "config", "user.name", "BlockCell Coding Eval"], workspace, 10)
    _run(["git", "add", "."], workspace, 10)
    _run(["git", "commit", "-qm", "fixture baseline"], workspace, 10)
    return workspace


def _agent_argv(template: str, task: dict[str, Any], workspace: pathlib.Path) -> list[str]:
    values = {
        "prompt": task["prompt"],
        "task_id": task["id"],
        "workspace": str(workspace),
    }
    return [token.format(**values) for token in shlex.split(template)]


def _load_metrics(workspace: pathlib.Path) -> dict[str, Any]:
    path = workspace / METRICS_FILE
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else {}
    except (OSError, json.JSONDecodeError):
        return {}


def run_task(
    task: dict[str, Any],
    work_root: pathlib.Path,
    agent_command: str | None,
    timeout: int,
    max_tool_calls: int,
) -> dict[str, Any]:
    workspace = _prepare_workspace(task, work_root)
    started = time.monotonic()
    agent_output = ""
    timed_out = False
    agent_exit_code: int | None = None
    if agent_command:
        env = os.environ.copy()
        env["BLOCKCELL_EVAL_WORKSPACE"] = str(workspace)
        env["BLOCKCELL_EVAL_TASK_ID"] = task["id"]
        try:
            completed = subprocess.run(
                _agent_argv(agent_command, task, workspace),
                cwd=workspace,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=timeout,
                check=False,
            )
            agent_exit_code = completed.returncode
            agent_output = completed.stdout[-20_000:]
        except subprocess.TimeoutExpired as error:
            timed_out = True
            agent_output = (error.stdout or "")[-20_000:]
        except OSError as error:
            agent_exit_code = -1
            agent_output = f"OSError launching agent command: {error}"

    acceptance = ROOT / task["acceptance"]
    # The workspace is agent-controlled, so the acceptance must not be able to
    # be gamed: put the pristine fixture tests back before verifying, and refuse
    # a run that discovered no test at all (`unittest discover` exits 0 after
    # printing "Ran 0 tests"). Both checks are independent of the agent-reported
    # metrics file, which is untrusted.
    fixture_tests_restored = restore_fixture_tests(ROOT / task["fixture"], workspace)
    verification = _run(
        [sys.executable, "-B", str(acceptance), str(workspace)], workspace, timeout
    )
    acceptance_tests_run = parse_tests_run(verification.stdout)
    passed = verification.returncode == 0 and acceptance_tests_run >= 1
    metrics = _load_metrics(workspace)
    metrics["timed_out"] = timed_out
    metrics.setdefault("tool_calls", 0)
    metrics.setdefault("tests_run", 0)
    metrics.setdefault("subagent_errors", 0)
    metrics.setdefault("input_tokens", 0)
    metrics.setdefault("output_tokens", 0)
    if passed:
        failure_category = None
    elif acceptance_tests_run == 0:
        failure_category = "verification_missing"
    else:
        failure_category = classify_failure(metrics, max_tool_calls)
    changed = _run(["git", "status", "--porcelain"], workspace, 10).stdout.splitlines()
    return {
        "task_id": task["id"],
        "category": task["category"],
        "passed": passed,
        "failure_category": failure_category,
        "duration_seconds": round(time.monotonic() - started, 3),
        "tool_calls": int(metrics["tool_calls"]),
        "tests_run": int(metrics["tests_run"]),
        "acceptance_tests_run": acceptance_tests_run,
        "fixture_tests_restored": fixture_tests_restored,
        "input_tokens": int(metrics["input_tokens"]),
        "output_tokens": int(metrics["output_tokens"]),
        "subagent_errors": int(metrics["subagent_errors"]),
        "agent_exit_code": agent_exit_code,
        "changed_files": changed,
        "agent_output_tail": agent_output,
        "acceptance_output_tail": verification.stdout[-20_000:],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=ROOT / "manifest.json")
    parser.add_argument("--task", action="append", dest="task_ids")
    parser.add_argument(
        "--agent-command",
        help="argv template with {prompt}, {workspace}, {task_id}; omit for baseline verification",
    )
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--max-tool-calls", type=int, default=100)
    parser.add_argument("--output", type=pathlib.Path, default=ROOT / "latest-report.json")
    parser.add_argument("--keep-workspaces", action="store_true")
    args = parser.parse_args()

    tasks = load_manifest(args.manifest)
    if args.task_ids:
        selected = set(args.task_ids)
        tasks = [task for task in tasks if task["id"] in selected]
        missing = selected.difference(task["id"] for task in tasks)
        if missing:
            parser.error(f"unknown task ids: {', '.join(sorted(missing))}")

    temp = pathlib.Path(tempfile.mkdtemp(prefix="blockcell-coding-eval-"))
    try:
        results = [
            run_task(task, temp, args.agent_command, args.timeout, args.max_tool_calls)
            for task in tasks
        ]
        report = summarize(results)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({key: value for key, value in report.items() if key != "results"}, ensure_ascii=False, indent=2))
        return 0 if report["tasks_passed"] == report["tasks_total"] else 1
    finally:
        if args.keep_workspaces:
            print(f"workspaces: {temp}")
        else:
            shutil.rmtree(temp, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
