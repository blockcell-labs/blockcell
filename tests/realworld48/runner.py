#!/usr/bin/env python3
"""Reproducible 48-task evaluation. No provider request is made by this module."""
from __future__ import annotations

import argparse
from collections import Counter
from contextlib import nullcontext
from datetime import datetime, timezone
import hashlib
import importlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parent
METRIC_FIELDS = ('input_tokens', 'output_tokens', 'tool_calls', 'tool_errors', 'http429', 'llm_calls')
MODULE_NAMES = ('task_code', 'task_data', 'task_misc')
VERSION = 1


def write_json(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(path)


def modules():
    return [importlib.import_module(name) for name in MODULE_NAMES]


def catalog():
    tasks = []
    for module in modules():
        for original in module.TASKS:
            task = dict(original, module=module.__name__)
            # Eight consecutive six-task bands, each 2 easy/2 medium/2 hard.
            task['difficulty'] = ('easy', 'easy', 'medium', 'medium', 'hard', 'hard')[(int(task['id'][1:]) - 1) % 6]
            tasks.append(task)
    tasks.sort(key=lambda t: t['id'])
    if [t['id'] for t in tasks] != [f't{i:02d}' for i in range(1, 49)]:
        raise ValueError('catalog must contain exactly t01..t48')
    for task in tasks:
        for key in ('category', 'title', 'prompt', 'protected'):
            if key not in task:
                raise ValueError(f'{task["id"]} missing {key}')
    return tasks


def file_hash(path):
    path = Path(path)
    if path.is_symlink():
        value = b'symlink\0' + os.readlink(path).encode()
    elif path.is_file():
        value = b'file\0' + path.read_bytes()
    else:
        raise FileNotFoundError(str(path))
    return hashlib.sha256(value).hexdigest()


def protected_hashes(task, workspace):
    hashes = {}
    for rel in task['protected']:
        p = Path(rel)
        if p.is_absolute() or '..' in p.parts:
            raise ValueError('protected paths must be relative and contained')
        path = workspace / p
        if path.is_dir() and not path.is_symlink():
            for child in sorted(path.rglob('*')):
                if child.is_file() or child.is_symlink():
                    hashes[str(child.relative_to(workspace))] = file_hash(child)
        else:
            hashes[rel] = file_hash(path)
    return hashes


def changed_protected(workspace, hashes):
    changed = []
    for rel, expected in hashes.items():
        p = workspace / rel
        # Parent symlinks could redirect protected reads outside the workspace.
        parent_redirected = any(parent.is_symlink() for parent in p.parents if parent != workspace and workspace in parent.parents)
        try:
            if parent_redirected or file_hash(p) != expected:
                changed.append(rel)
        except (OSError, ValueError):
            changed.append(rel)
    return changed


def load_metrics(workspace):
    result = dict.fromkeys(METRIC_FIELDS)
    try:
        value = json.loads((workspace / '.eval-metrics.json').read_text(encoding='utf-8'))
        for key in METRIC_FIELDS:
            n = value.get(key)
            if isinstance(n, (int, float)) and not isinstance(n, bool) and math.isfinite(n) and n >= 0:
                result[key] = n
    except (OSError, ValueError, TypeError, AttributeError):
        pass
    return result


def command_argv(template, prompt, workspace, task_id):
    values = dict(prompt=prompt, workspace=str(workspace), task_id=task_id)
    return [part.format_map(values) for part in shlex.split(template)]


def redact(text, secrets=()):
    text = str(text)
    for secret in sorted(set(secrets), key=len, reverse=True):
        if secret and len(secret) >= 4:
            text = text.replace(secret, '[REDACTED]')
    text = re.sub(r'(?i)(Bearer\s+)[A-Za-z0-9_.~+/=-]+', r'\1[REDACTED]', text)
    return text


def env_secrets(env):
    return [v for k, v in env.items() if any(s in k.upper() for s in ('KEY', 'TOKEN', 'SECRET', 'PASSWORD')) and len(v) >= 4]


def execute(argv, cwd, env, timeout):
    """File-backed output avoids unbounded pipe memory; kill entire POSIX group."""
    started = time.monotonic()
    with tempfile.TemporaryFile() as output:
        try:
            process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=output,
                                       stderr=subprocess.STDOUT, start_new_session=True)
        except OSError as error:
            return dict(returncode=None, timed_out=False, launch_error=str(error), output='', wall_s=0)
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        except BaseException:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            raise
        # Also terminate abandoned background children after the wrapper exits.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        length = output.tell()
        output.seek(max(0, length - 100_000))
        text = output.read().decode('utf-8', errors='replace')
    return dict(returncode=process.returncode, timed_out=timed_out, launch_error=None,
                output=text, output_truncated=length > 100_000,
                wall_s=round(time.monotonic() - started, 3))


def read_records(path):
    if not Path(path).exists():
        return []
    records = []
    for lineno, line in enumerate(Path(path).read_text(encoding='utf-8').splitlines(), 1):
        if not line.strip():
            continue
        try:
            item = json.loads(line)
            if not isinstance(item, dict) or 'id' not in item:
                raise ValueError('not a record')
            records.append(item)
        except ValueError as e:
            raise ValueError(f'{path}:{lineno}: damaged checkpoint; preserve and repair before resume') from e
    return records


def append_record(path, record):
    with Path(path).open('a', encoding='utf-8') as f:
        f.write(json.dumps(record, ensure_ascii=False) + '\n')
        f.flush()
        os.fsync(f.fileno())


def recover_interrupted(run_dir):
    records_path = run_dir / 'attempts.jsonl'
    records = read_records(records_path)
    active_path = run_dir / 'active.json'
    if active_path.exists():
        active = json.loads(active_path.read_text(encoding='utf-8'))
        if not any(r.get('attempt_id') == active['attempt_id'] for r in records):
            active.update(status='interrupted', detail='runner stopped before checkpoint; workspace preserved; usage unknown',
                          recovered_at=datetime.now(timezone.utc).isoformat())
            append_record(records_path, active)
            records.append(active)
        active_path.unlink()
    return records


def command_fingerprint(template):
    # Include existing script/config arguments so changing a wrapper at the
    # same path cannot silently change the experiment on resume.
    files = {}
    for part in shlex.split(template):
        if '{' not in part:
            p = Path(part).expanduser()
            if p.is_file():
                files[str(p.resolve())] = file_hash(p)
    relevant_env = {k: hashlib.sha256(os.environ[k].encode()).hexdigest()
                    for k in ('EVAL_API_BASE', 'EVAL_MODEL', 'EVAL_API_TYPE') if k in os.environ}
    return hashlib.sha256(json.dumps(dict(template=template, files=files, environment=relevant_env), sort_keys=True).encode()).hexdigest()


def next_attempt(run_dir, task_id, previous_count):
    numbers = [previous_count]
    for p in (run_dir / 'workspaces' / task_id).glob('attempt-*'):
        suffix = p.name.removeprefix('attempt-')
        if suffix.isdigit():
            numbers.append(int(suffix))
    return max(numbers) + 1


def summarize(tasks, records):
    rows = []
    for task in tasks:
        attempts = [r for r in records if r['id'] == task['id']]
        independent = [r for r in attempts if not r.get('assisted')]
        assisted = [r for r in attempts if r.get('assisted')]
        original = independent[0] if independent else None
        rows.append(dict(id=task['id'], title=task['title'], category=task['category'],
                         difficulty=task['difficulty'], status=original['status'] if original else 'not_run',
                         assisted_status=assisted[-1]['status'] if assisted else None,
                         attempts=len(attempts), first_attempt=original))
    passed = sum(r['status'] == 'pass' for r in rows)
    executed = sum(r['status'] in ('pass', 'fail') for r in rows)
    summary = dict(total=len(tasks), independent_passed=passed,
                   completion_rate=passed / len(tasks) if tasks else 0,
                   executed_success_rate=passed / executed if executed else None,
                   not_run=sum(r['status'] == 'not_run' for r in rows),
                   assisted_passed=sum(r['assisted_status'] == 'pass' for r in rows),
                   status_counts=dict(Counter(r['status'] for r in rows)),
                   attempt_count=len(records), rows=rows)
    for key in METRIC_FIELDS:
        known = [r.get('metrics', {}).get(key) for r in records]
        reported = [v for v in known if v is not None]
        summary[key] = sum(reported) if reported else None
        summary[key + '_reported_attempts'] = len(reported)
    summary['total_wall_s'] = round(sum(r.get('wall_s') or 0 for r in records), 3)
    for group in ('category', 'difficulty'):
        summary['by_' + group] = {
            value: dict(total=sum(r[group] == value for r in rows),
                        passed=sum(r[group] == value and r['status'] == 'pass' for r in rows))
            for value in sorted({r[group] for r in rows})}
    return summary


def save_reports(run_dir, tasks, records):
    summary = summarize(tasks, records)
    write_json(run_dir / 'summary.json', summary)
    lines = ['# Realworld48 评测结果', '',
             f'独立完成：{summary["independent_passed"]}/{summary["total"]}；未运行：{summary["not_run"]}；干预后通过：{summary["assisted_passed"]}。', '',
             '首次独立结果固定保留。Token 合计仅覆盖已报告用量的尝试；缺失值为 null。', '',
             '| ID | 任务 | 难度 | 首次独立结果 | 干预后结果 |', '|---|---|---|---|---|']
    lines += [f'| {r["id"]} | {r["title"]} | {r["difficulty"]} | {r["status"]} | {r["assisted_status"] or "—"} |' for r in summary['rows']]
    lines += ['', '详细错误、耗时和指标见 summary.json 与 attempts.jsonl。']
    (run_dir / 'report.md').write_text('\n'.join(lines) + '\n', encoding='utf-8')
    return summary


def suite_fingerprint():
    paths = [ROOT / 'runner.py'] + [ROOT / (name + '.py') for name in MODULE_NAMES]
    return hashlib.sha256(''.join(file_hash(p) for p in paths).encode()).hexdigest()


def prepare_task(task, workspace):
    workspace.mkdir(parents=True, exist_ok=False)
    module = importlib.import_module(task['module'])
    module.prepare(task['id'], workspace)
    hashes = protected_hashes(task, workspace)
    all_hashes = {str(p.relative_to(workspace)): file_hash(p) for p in sorted(workspace.rglob('*')) if p.is_file() or p.is_symlink()}
    return dict(protected=hashes, fixture_sha256=hashlib.sha256(json.dumps(all_hashes, sort_keys=True).encode()).hexdigest())


def verify_task(task, workspace, hashes):
    if sys.flags.optimize:
        raise RuntimeError('optimized Python disables grader assertions; use normal Python')
    changed = changed_protected(workspace, hashes)
    if changed:
        return 'fail', 'protected input changed: ' + ', '.join(changed)
    module = importlib.import_module(task['module'])
    # Check a copy so test runs and mutation testing cannot damage the delivery.
    with tempfile.TemporaryDirectory(prefix='realworld48-grade-') as td:
        copy = Path(td) / 'workspace'
        shutil.copytree(workspace, copy, symlinks=True)
        try:
            result, detail = module.verify(task['id'], copy)
        except Exception as error:
            return 'grader_error', f'{type(error).__name__}: {error}'
        changed = changed_protected(copy, hashes)
        if changed:
            return 'fail', 'protected input changed during verification: ' + ', '.join(changed)
    return ('pass' if result else 'fail'), str(detail)


def service_for(task, workspace):
    module = importlib.import_module(task['module'])
    return module.service(task['id'], workspace) if int(task['id'][1:]) >= 41 else nullcontext(None)


def format_prompt(task, workspace, endpoint):
    # Only replace known markers: JSON examples with braces remain literal.
    prompt = task['prompt'].replace('{workspace}', str(workspace)).replace('{D}', str(workspace))
    prompt = prompt.replace('{endpoint}', endpoint or '')
    protected = ', '.join(task['protected']) or '无'
    return f'工作目录：{workspace}\n{prompt}\n受保护输入（不得修改或删除）：{protected}。仅处理本题目录，不访问评测器或其他题目录。'


def selected(tasks, ids):
    if not ids:
        return tasks
    unknown = set(ids) - {t['id'] for t in tasks}
    if unknown:
        raise ValueError(f'unknown tasks: {sorted(unknown)}')
    return [t for t in tasks if t['id'] in ids]


def initialize_run(run_dir, tasks):
    run_dir.mkdir(parents=True, exist_ok=False)
    (run_dir / 'workspaces').mkdir()
    (run_dir / 'state').mkdir()
    write_json(run_dir / 'run.json', dict(version=VERSION, suite_fingerprint=suite_fingerprint(),
               created_at=datetime.now(timezone.utc).isoformat(), seed=20261029,
               python_version=platform.python_version(), platform=platform.platform()))
    for task in tasks:
        workspace = run_dir / 'workspaces' / task['id'] / 'attempt-001'
        state = prepare_task(task, workspace)
        state['workspace'] = str(workspace)
        write_json(run_dir / 'state' / (task['id'] + '.json'), state)
        (workspace / 'TASK.md').write_text(format_prompt(task, workspace, '{endpoint}') + '\n', encoding='utf-8')
    save_reports(run_dir, tasks, [])


def run_agent(args, tasks):
    run_dir = Path(args.run_dir).resolve()
    if not run_dir.exists():
        initialize_run(run_dir, tasks)
    config_path = run_dir / 'run.json'
    config = json.loads(config_path.read_text())
    if config['suite_fingerprint'] != suite_fingerprint():
        raise ValueError('suite changed; start a new run directory')
    # Compare a hash rather than persist a command that may contain credentials.
    invocation = dict(command_sha256=command_fingerprint(args.agent_command), timeout=args.timeout,
                      model_label=args.model_label)
    if 'invocation' in config and config['invocation'] != invocation:
        raise ValueError('agent command/model/timeout changed; use a new run directory')
    config['invocation'] = invocation
    write_json(config_path, config)
    records_path = run_dir / 'attempts.jsonl'
    records = recover_interrupted(run_dir)
    if records and not (args.resume or args.assisted):
        raise ValueError('run already has attempts; use --resume or --assisted')
    if args.assisted and not args.intervention_note:
        raise ValueError('--assisted requires --intervention-note')
    started = time.monotonic()
    for task in selected(tasks, args.task):
        previous = [r for r in records if r['id'] == task['id']]
        retry_interrupted = bool(previous and previous[-1]['status'] == 'interrupted')
        if previous and not args.assisted and not retry_interrupted:
            continue
        if args.max_wall_seconds and time.monotonic() - started >= args.max_wall_seconds:
            break
        known_tokens = sum((r.get('metrics', {}).get('input_tokens') or 0) + (r.get('metrics', {}).get('output_tokens') or 0) for r in records)
        if args.max_known_tokens and known_tokens >= args.max_known_tokens:
            break
        attempt = next_attempt(run_dir, task['id'], len(previous)) if previous else 1
        state_path = run_dir / 'state' / (task['id'] + '.json')
        if not previous:
            state = json.loads(state_path.read_text())
            workspace = Path(state['workspace'])
        else:
            workspace = run_dir / 'workspaces' / task['id'] / f'attempt-{attempt:03d}'
            state = prepare_task(task, workspace)
            state['workspace'] = str(workspace)
        attempt_id = f'{task["id"]}-{attempt:03d}'
        env = dict(os.environ)
        env.update(BLOCKCELL_EVAL_WORKSPACE=str(workspace), BLOCKCELL_EVAL_TASK_ID=task['id'],
                   BLOCKCELL_EVAL_ATTEMPT_ID=attempt_id)
        secrets = env_secrets(env) + ['local-demo-access-48']
        record = dict(id=task['id'], attempt_id=attempt_id, assisted=args.assisted or retry_interrupted,
                      intervention_note=redact(args.intervention_note or ('automatic restart after interrupted attempt' if retry_interrupted else ''), secrets),
                      fixture_sha256=state['fixture_sha256'], started_at=datetime.now(timezone.utc).isoformat(),
                      workspace=str(workspace), metrics=dict.fromkeys(METRIC_FIELDS))
        write_json(run_dir / 'active.json', record)
        try:
            with service_for(task, workspace) as endpoint:
                prompt = format_prompt(task, workspace, endpoint)
                (workspace / 'TASK.md').write_text(prompt + '\n', encoding='utf-8')
                env['BLOCKCELL_EVAL_PROMPT'] = prompt
                if endpoint:
                    env['BLOCKCELL_EVAL_ENDPOINT'] = endpoint
                result = execute(command_argv(args.agent_command, prompt, workspace, task['id']), workspace, env, args.timeout)
                record.update({k: v for k, v in result.items() if k != 'output'})
                record['metrics'] = load_metrics(workspace)
                record['output_tail'] = redact(result['output'], secrets)
                if result['launch_error']:
                    record.update(status='environment_error', detail=redact(result['launch_error'], secrets))
                elif result['timed_out']:
                    record.update(status='timeout', detail='agent exceeded per-task timeout')
                else:
                    status, detail = verify_task(task, workspace, state['protected'])
                    record.update(status=status, detail=redact(detail, secrets))
                    if result['returncode'] and status == 'pass':
                        record['status'] = 'fail'
                        record['detail'] = f'agent exited {result["returncode"]}; artifacts otherwise passed'
        except (OSError, RuntimeError) as error:
            record.update(status='environment_error', detail=redact(str(error), secrets))
        append_record(records_path, record)
        (run_dir / 'active.json').unlink(missing_ok=True)
        records.append(record)
        save_reports(run_dir, tasks, records)
        print(f'{task["id"]} {record["status"]} {record.get("wall_s", 0):.2f}s', flush=True)
    summary = save_reports(run_dir, tasks, records)
    print(f'report: {run_dir / "report.md"}; independent {summary["independent_passed"]}/48', flush=True)


def selfcheck(args, tasks):
    results = []
    for task in selected(tasks, args.task):
        module = importlib.import_module(task['module'])
        with tempfile.TemporaryDirectory(prefix='realworld48-selfcheck-') as td:
            workspace = Path(td) / task['id']
            state = prepare_task(task, workspace)
            with service_for(task, workspace) as endpoint:
                before = os.environ.get('BLOCKCELL_EVAL_ENDPOINT')
                if endpoint:
                    os.environ['BLOCKCELL_EVAL_ENDPOINT'] = endpoint
                try:
                    baseline, baseline_detail = verify_task(task, workspace, state['protected'])
                    module.solve(task['id'], workspace)
                    reference, detail = verify_task(task, workspace, state['protected'])
                    # Remove all unprotected deliveries to prove absence is rejected.
                    bad = Path(td) / 'bad'
                    shutil.copytree(workspace, bad, symlinks=True)
                    for p in sorted(bad.rglob('*'), reverse=True):
                        rel = str(p.relative_to(bad))
                        if (p.is_file() or p.is_symlink()) and rel not in state['protected']:
                            p.unlink()
                    corrupt, corrupt_detail = verify_task(task, bad, state['protected'])
                    mutation = 'not_applicable'
                    if state['protected']:
                        rel = next(iter(state['protected']))
                        p = workspace / rel
                        if p.is_symlink():
                            p.unlink()
                            p.symlink_to('changed-target')
                        else:
                            p.write_bytes(p.read_bytes() + b'\nchanged\n')
                        mutation, _ = verify_task(task, workspace, state['protected'])
                    passed = baseline == 'fail' and reference == 'pass' and corrupt == 'fail' and mutation in ('fail', 'not_applicable')
                    rec = dict(id=task['id'], passed=passed, baseline=baseline, reference=reference,
                               missing_delivery=corrupt, protected_mutation=mutation,
                               detail=detail, baseline_detail=baseline_detail, missing_detail=corrupt_detail)
                    results.append(rec)
                    print(f'{task["id"]} selfcheck {"PASS" if passed else "FAIL"}: {detail[:160]}', flush=True)
                finally:
                    if before is None:
                        os.environ.pop('BLOCKCELL_EVAL_ENDPOINT', None)
                    else:
                        os.environ['BLOCKCELL_EVAL_ENDPOINT'] = before
    report = dict(kind='harness_selfcheck_not_agent_evaluation', suite_fingerprint=suite_fingerprint(),
                  tasks_checked=len(results), passed=sum(r['passed'] for r in results), results=results)
    if args.output:
        write_json(Path(args.output), report)
    return 0 if all(r['passed'] for r in results) else 1


def main():
    if sys.flags.optimize:
        print('error: optimized Python is unsupported; omit -O/-OO and PYTHONOPTIMIZE', file=sys.stderr)
        return 2
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='command', required=True)
    ls = sub.add_parser('list'); ls.add_argument('--json', action='store_true')
    prep = sub.add_parser('prepare'); prep.add_argument('--run-dir', required=True)
    check = sub.add_parser('selfcheck'); check.add_argument('--task', action='append'); check.add_argument('--output')
    check = sub.add_parser('check'); check.add_argument('--run-dir', required=True); check.add_argument('--task', action='append')
    report = sub.add_parser('report'); report.add_argument('--run-dir', required=True)
    run = sub.add_parser('run')
    run.add_argument('--run-dir', required=True); run.add_argument('--agent-command', required=True)
    run.add_argument('--task', action='append'); run.add_argument('--resume', action='store_true')
    run.add_argument('--timeout', type=float, default=600); run.add_argument('--model-label', default='unspecified')
    run.add_argument('--max-wall-seconds', type=float); run.add_argument('--max-known-tokens', type=int)
    run.add_argument('--assisted', action='store_true'); run.add_argument('--intervention-note')
    args = p.parse_args()
    try:
        tasks = catalog()
        if args.command == 'list':
            if args.json:
                print(json.dumps(dict(version=VERSION, tasks=tasks), ensure_ascii=False, indent=2))
            else:
                for t in tasks:
                    print(f'{t["id"]} {t["difficulty"]:6} {t["category"]}: {t["title"]}')
        elif args.command == 'prepare':
            initialize_run(Path(args.run_dir).resolve(), tasks)
            print(f'prepared 48 tasks: {args.run_dir}')
        elif args.command == 'selfcheck':
            return selfcheck(args, tasks)
        elif args.command == 'run':
            for label, value in [('timeout', args.timeout), ('max-wall-seconds', args.max_wall_seconds),
                                 ('max-known-tokens', args.max_known_tokens)]:
                if value is not None and (not math.isfinite(value) or value <= 0):
                    raise ValueError(f'{label} must be finite and positive')
            run_agent(args, tasks)
        elif args.command == 'report':
            d = Path(args.run_dir).resolve()
            save_reports(d, tasks, read_records(d / 'attempts.jsonl'))
            print(d / 'report.md')
        elif args.command == 'check':
            d = Path(args.run_dir).resolve()
            for task in selected(tasks, args.task):
                state = json.loads((d / 'state' / (task['id'] + '.json')).read_text())
                if int(task['id'][1:]) >= 41:
                    print(task['id'], 'HTTP trace grading requires live run; inspect recorded attempt instead')
                else:
                    print(task['id'], *verify_task(task, Path(state['workspace']), state['protected']))
        return 0
    except (ValueError, OSError, KeyError) as error:
        print(f'error: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
