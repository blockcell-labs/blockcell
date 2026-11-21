import json
import copy
import shlex
import sys
from concurrent.futures import ThreadPoolExecutor

from ..source import command
from ..isolation import BudgetExceeded, ProviderUnavailable


def evaluate(frozen, binary, output, config, wrapper, parent_report=None, env=None, check=None):
    workers = config['agent'].get('concurrency', 1)
    if workers > 1:
        options = config['benchmarks']['L3']['coding_eval']
        ids = options.get('task_ids') or [task['id'] for task in json.loads((frozen / 'tests/coding_eval/manifest.json').read_text())['tasks']]
        if not ids or len(set(ids)) != len(ids):
            raise ValueError('coding_eval requires unique nonempty task IDs')
        def task(task_id):
            child = copy.deepcopy(config)
            child['agent']['concurrency'] = 1
            child['benchmarks']['L3']['coding_eval']['task_ids'] = [task_id]
            folder = output.parent / ('task-' + str(ids.index(task_id)))
            folder.mkdir(parents=True, exist_ok=True)
            return evaluate(frozen, binary, folder / 'report.json', child, wrapper, env=env, check=check)
        with ThreadPoolExecutor(max_workers=workers) as pool:
            reports = list(pool.map(task, ids))
        data = {'status': 'ok', 'results': [row for report in reports for row in report['results']]}
        if {row['task_id'] for row in data['results']} != set(ids) or len(data['results']) != len(ids):
            raise ValueError('coding_eval task results do not match requested task IDs')
        traces = {}
        for path in output.parent.glob('eval-*/metrics.json'):
            metrics = json.loads(path.read_text())
            traces[metrics['task_id']] = metrics['trace_path']
        for row in data['results']:
            row['trace_path'] = traces.get(row['task_id'], '')
        output.write_text(json.dumps(data, ensure_ascii=False, indent=2))
        return data
    command_template = shlex.join([sys.executable, str(wrapper), "{prompt}", "{workspace}", "{task_id}"])
    argv = [sys.executable, str(frozen / "tests/coding_eval/runner.py"),
            "--agent-command", command_template, "--output", str(output)]
    for task_id in config["benchmarks"]["L3"]["coding_eval"].get("task_ids", []):
        argv += ["--task", task_id]
    if parent_report:
        argv += ["--baseline", str(parent_report), "--max-token-ratio", str(config["benchmarks"]["thresholds"]["max_token_ratio"])]
    try:
        command(argv, frozen, config["genesis"]["max_cycle_minutes"] * 60, env, check=check)
    except (BudgetExceeded, ProviderUnavailable):
        raise
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
