import hashlib
import json
import re
import shutil
from collections import Counter
from contextlib import contextmanager
from pathlib import Path

from ..source import command
from ..isolation import build_sandbox, clean_env


@contextmanager
def runtime(worktree, destination, before=None):
    root = Path(worktree).resolve()
    before = set(root.rglob('.blockcell')) if before is None else set(before)
    try:
        yield
    finally:
        for path in set(root.rglob('.blockcell')) - before:
            if path.is_symlink() or not path.parent.resolve().is_relative_to(root):
                continue
            target = Path(destination) / path.relative_to(root)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(path), target)


def environment(config):
    env = clean_env(config['_root'] + '/build-home', config['agent']['key_env'])
    env.update(CARGO_TARGET_DIR=config['_root'] + '/target', CARGO_NET_OFFLINE='true', CARGO_BUILD_JOBS='2')
    return env


def rust_files(worktree, names):
    root = Path(worktree).resolve()
    files = []
    for name in names:
        path = root / name
        if path.suffix != '.rs' or not path.exists():
            continue
        if not path.resolve().is_relative_to(root) or path.is_symlink():
            raise RuntimeError('Rust file escaped candidate worktree: ' + name)
        files.append(name)
    return sorted(files)


def format_changed(worktree, names, config, check=False):
    files = rust_files(worktree, names)
    if not files:
        return ''
    argv = ['rustfmt', '--edition', '2021', '--config', 'skip_children=true']
    if check:
        argv.append('--check')
    return execute(argv + ['--'] + files, worktree, config, environment(config))


def execute(argv, worktree, config, env, output_path=None):
    if config['agent']['require_bwrap']:
        argv = build_sandbox(argv, Path(worktree), Path(env['CARGO_TARGET_DIR']), Path(env['HOME']), env)
    return command(argv, worktree, config['build']['timeout'], env, output_path=output_path)


def diagnostics(output, worktree, errors_only=False):
    failures = Counter()
    root = str(Path(worktree).resolve()) + '/'
    for line in output.splitlines():
        if line.startswith('{'):
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get('reason') != 'compiler-message':
                continue
            message = row['message']
            if message.get('level') not in ('warning', 'error'):
                continue
            if errors_only and message['level'] != 'error':
                continue
            spans = [(s['file_name'].removeprefix(root), re.sub(r'\s+', '', ''.join(
                         t['text'][t['highlight_start'] - 1:t['highlight_end'] - 1] for t in s.get('text', []))))
                     for s in message.get('spans', []) if s.get('is_primary')]
            key = json.dumps([message.get('code'), message['message'].replace(root, ''), spans], sort_keys=True)
            failures[key] += 1
        elif re.match(r'^test .+ \.\.\. FAILED$', line):
            failures[re.sub(r'\(line \d+\)', '(line)', line.replace(root, ''))] += 1
    return failures


def check_command(argv, relaxed=False):
    argv = list(argv)
    if argv[:2] not in (['cargo', 'clippy'], ['cargo', 'test']):
        return argv
    split = argv.index('--') if '--' in argv else len(argv)
    if not any(a.startswith('--message-format') for a in argv):
        argv.insert(split, '--message-format=json')
    if relaxed:
        if '--' not in argv:
            argv.append('--')
        argv.append('--cap-lints=warn')
    return argv


def capture(argv, worktree, config, path):
    passed = True
    try:
        output = execute(argv, worktree, config, environment(config), path)
    except RuntimeError as exc:
        passed = False
        output = path.read_text() if path.exists() else str(exc)
    return {'argv': argv, 'passed': passed, 'diagnostics': dict(diagnostics(output, worktree)),
            'failures': dict(diagnostics(output, worktree, errors_only=True)),
            'log': str(path), 'tail': output[-4000:]}


def baseline(worktree, config):
    sha = command(['git', 'rev-parse', 'HEAD'], worktree).strip()
    key = hashlib.sha256(json.dumps(config['benchmarks']['L0'], sort_keys=True).encode()).hexdigest()[:16]
    folder = Path(config['_root']) / 'L0-baselines' / (sha + '-' + key)
    folder.mkdir(parents=True, exist_ok=True)
    saved = folder / 'checks.json'
    if saved.exists():
        return json.loads(saved.read_text())
    results = []
    for index, argv in enumerate(config['benchmarks']['L0']):
        result = capture(check_command(argv), worktree, config, folder / f'{index}.log')
        result['relaxed'] = argv[:2] == ['cargo', 'clippy'] and not result['passed']
        if result['relaxed']:
            result['relative'] = capture(check_command(argv, True), worktree, config, folder / f'{index}-relative.log')
        results.append(result)
    saved.write_text(json.dumps(results, indent=2))
    return results


def evaluate(worktree, config, parent_worktree=None):
    previous = baseline(parent_worktree, config) if parent_worktree and config['benchmarks']['L0'] else []
    names = command(['git', 'diff', '--name-only', 'HEAD'], worktree).splitlines() if any(argv[:2] == ['cargo', 'fmt'] for argv in config['benchmarks']['L0']) else []
    folder = Path(config['_root']) / 'runs' / Path(worktree).name
    folder.mkdir(parents=True, exist_ok=True)
    outputs = []
    for index, argv in enumerate(config['benchmarks']['L0']):
        if argv[:2] == ['cargo', 'fmt']:
            outputs.append(format_changed(worktree, names, config, check=True))
            continue
        parent = previous[index] if previous else {'passed': True, 'diagnostics': {}}
        relative = parent.get('relative', parent)
        current = capture(check_command(argv, parent.get('relaxed', False)), worktree, config, folder / f'L0-{index}.log')
        new = Counter(current['diagnostics']) - Counter(relative['diagnostics'])
        if new or (not current['passed'] and (relative['passed'] or not current.get('failures'))):
            raise RuntimeError('new L0 failures vs Parent: ' + json.dumps(argv) + '\n' + current['tail'])
        outputs.append(json.dumps({'command': argv, 'parent_passed': parent['passed'],
                                   'candidate_passed': current['passed'], 'new_failures': dict(new)}))
    if config['benchmarks'].get('audit_command'):
        env = environment(config)
        current, output = audit(worktree, config, env)
        previous, _ = audit(parent_worktree, config, env) if parent_worktree else (set(), '')
        if current - previous:
            raise RuntimeError('new cargo audit vulnerabilities: ' + ', '.join(sorted(current - previous)))
        outputs.append(output)
    return '\n'.join(outputs)


def audit(worktree, config, env):
    argv = config['benchmarks']['audit_command']
    folder = Path(config['_root']) / 'audit-reports'
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / ((Path(worktree).name or 'worktree') + '.json')
    path.unlink(missing_ok=True)
    try:
        output = execute(argv, worktree, config, env, path)
    except RuntimeError as exc:
        output = path.read_text() if path.exists() else str(exc).split('\n', 1)[-1]
    data = json.loads(output)
    if 'vulnerabilities' not in data:
        raise RuntimeError('cargo audit returned no vulnerability report')
    vulnerabilities = data['vulnerabilities'].get('list', [])
    return {row['advisory']['id'] for row in vulnerabilities}, output
