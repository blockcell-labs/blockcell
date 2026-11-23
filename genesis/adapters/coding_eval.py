import json
import hashlib
import copy
import shlex
import sys
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
from contextvars import copy_context

from ..source import command, remaining_cycle_time
from ..isolation import BudgetExceeded, ProviderUnavailable, clean_env, offline_sandbox


def restore_tests(fixture, workspace):
    # Never follow candidate symlinks from trusted orchestration.
    if any(path.is_symlink() for path in Path(workspace).rglob('*')):
        raise RuntimeError('candidate workspace contains a symlink')
    for source in Path(fixture).rglob('test_*.py'):
        target = Path(workspace) / source.relative_to(fixture)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)


def verify_workspace(acceptance, workspace, suite, home, timeout, env):
    argv = [sys.executable, '-I', '-B', str(acceptance), str(workspace)]
    argv = offline_sandbox(argv, workspace, home, [suite])
    remaining = remaining_cycle_time()
    if remaining <= 0:
        raise BudgetExceeded('cycle time limit')
    try:
        return subprocess.run(argv, cwd=workspace, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True, timeout=min(timeout, remaining), check=False)
    except subprocess.TimeoutExpired as exc:
        if remaining <= timeout:
            raise BudgetExceeded('cycle time limit') from exc
        raise


def isolated_evaluate(frozen, binary, output, config, wrapper, check):
    suite = frozen / 'tests/coding_eval'
    manifest = json.loads((suite / 'manifest.json').read_text())['tasks']
    ids = config['benchmarks']['L3']['coding_eval'].get('task_ids') or [t['id'] for t in manifest]
    tasks = {t['id']: t for t in manifest}
    if not ids or len(set(ids)) != len(ids) or not set(ids) <= tasks.keys():
        raise ValueError('coding_eval requires unique known task IDs')
    output.parent.mkdir(parents=True, exist_ok=True)
    from ..config import evaluation_hash
    binary_hash = hashlib.sha256(Path(binary).read_bytes()).hexdigest()
    identity = evaluation_hash(config) + ':' + binary_hash
    checkpoints = output.parent / 'task-checkpoints'
    checkpoints.mkdir(exist_ok=True)

    def run(task_id):
        checkpoint = checkpoints / (hashlib.sha256(task_id.encode()).hexdigest() + '.json')
        if checkpoint.exists():
            saved = json.loads(checkpoint.read_text())
            if saved.get('identity') == identity:
                row = saved['result']
                if row.get('task_id') != task_id or type(row.get('passed')) is not bool:
                    raise ValueError('invalid coding task checkpoint')
                return row
        task = tasks[task_id]
        folder = Path(tempfile.mkdtemp(prefix='eval-', dir=output.parent))
        work = folder / 'workspace'
        fixture = (suite / task['fixture']).resolve()
        acceptance = (suite / task['acceptance']).resolve()
        if not fixture.is_relative_to(suite.resolve()) or not acceptance.is_relative_to(suite.resolve()):
            raise ValueError('coding fixture or acceptance escaped frozen suite')
        shutil.copytree(fixture, work)
        began = time.monotonic()
        failure = ''
        agent_output = ''
        try:
            agent_output = command([sys.executable, str(wrapper), task['prompt'], str(work), task_id], work,
                                   config['agent']['timeout'], clean_env(folder/'controller-home', config['agent']['key_env']), check=check)
        except (BudgetExceeded, ProviderUnavailable):
            raise
        except (RuntimeError, TimeoutError) as exc:
            failure = str(exc)
        if check:
            check()  # A wrapper exit may race the proxy's persisted outage.
        passed = False
        verification_output = ''
        try:
            restore_tests(fixture, work)
            result = verify_workspace(acceptance, work, suite, folder/'verify-home', config['agent']['timeout'],
                                      clean_env(folder/'verify-home', config['agent']['key_env']))
            verification_output = result.stdout
            counts = re.findall(r'^Ran (\d+) tests? in ', result.stdout, re.MULTILINE)
            passed = not failure and result.returncode == 0 and bool(counts) and int(counts[-1]) > 0
        except BudgetExceeded:
            raise
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as exc:
            failure = str(exc)
        metrics_file = work / '.blockcell-eval-metrics.json'
        metrics = json.loads(metrics_file.read_text()) if metrics_file.is_file() and not metrics_file.is_symlink() else {}
        row = dict(task_id=task_id, category=task.get('category', ''), passed=passed,
                    verified_by_tests=passed, duration_ms=(time.monotonic()-began)*1000,
                    trace_path=metrics.get('trace_path', ''), failure_reason=failure or ('' if passed else 'acceptance failed'),
                    agent_output_tail=agent_output[-4000:], acceptance_output_tail=verification_output[-4000:])
        row.update({key: metrics[key] for key in ('input_tokens', 'output_tokens', 'tool_calls', 'tool_errors') if key in metrics})
        pending = checkpoint.with_suffix('.new')
        pending.write_text(json.dumps({'identity': identity, 'result': row}, ensure_ascii=False))
        pending.replace(checkpoint)
        return row
    with ThreadPoolExecutor(max_workers=config['agent'].get('concurrency', 1)) as pool:
        futures = [pool.submit(copy_context().run, run, task_id) for task_id in ids]
        rows = [future.result() for future in futures]
    data = {'status': 'ok', 'results': rows}
    output.write_text(json.dumps(data, ensure_ascii=False, indent=2))
    return data


def evaluate(frozen, binary, output, config, wrapper, parent_report=None, env=None, check=None):
    if config['agent']['require_bwrap']:
        return isolated_evaluate(frozen, binary, output, config, wrapper, check)
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
            futures = [pool.submit(copy_context().run, task, task_id) for task_id in ids]
            reports = [future.result() for future in futures]
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
