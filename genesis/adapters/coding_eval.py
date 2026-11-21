import json
import shlex
import sys

from ..source import command


def evaluate(frozen, binary, output, config, wrapper, parent_report=None, env=None):
    command_template = shlex.join([sys.executable, str(wrapper), "{prompt}", "{workspace}", "{task_id}"])
    argv = [sys.executable, str(frozen / "tests/coding_eval/runner.py"),
            "--agent-command", command_template, "--output", str(output)]
    for task_id in config["benchmarks"]["L3"]["coding_eval"].get("task_ids", []):
        argv += ["--task", task_id]
    if parent_report:
        argv += ["--baseline", str(parent_report), "--max-token-ratio", str(config["benchmarks"]["thresholds"]["max_token_ratio"])]
    try:
        command(argv, frozen, config["genesis"]["max_cycle_minutes"] * 60, env)
    except RuntimeError:
        # The real runner exits nonzero for a legitimate failed acceptance test.
        if not output.exists():
            raise
    data = json.loads(output.read_text())
    if not isinstance(data.get("results"), list) or not data["results"]:
        raise ValueError("coding_eval returned no task results")
    for row in data["results"]:
        row["task_id"] = row.get("task_id", row.get("id"))
    traces = {}
    for path in sorted(output.parent.glob('eval-*/metrics.json'), key=lambda p: p.stat().st_mtime):
        metrics = json.loads(path.read_text())
        traces[metrics['task_id']] = metrics['trace_path']
    for row in data['results']:
        row['trace_path'] = traces.get(row['task_id'], '')
    data["status"] = "ok"
    return data
