"""Separate EAB's candidate gateway from its controller, kit and grader."""
import json
import os
import shutil
import sys
from pathlib import Path

from genesis.native_sandbox import macos_command


def gateway_command(argv, root, binary, proxy_url, hook_script=None):
    root = Path(root)
    workspace = root / 'runtime/workspace'
    home = root / 'runtime/isolated-home'
    workspace.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    hook_reads = [Path(hook_script).resolve()] if hook_script else []
    hook_writes = [root / 'reports/hook_events.jsonl'] if hook_script else []
    if hook_script:
        hook_writes[0].parent.mkdir(parents=True, exist_ok=True)
        hook_writes[0].touch(exist_ok=True)
    if sys.platform == 'darwin':
        return macos_command(argv, [workspace, home, binary, *hook_reads], [workspace, home, *hook_writes], proxy_url,
                             listen_ports=[18790, 18791], local_service_ports=[18789])
    if not shutil.which('bwrap'):
        raise RuntimeError('bubblewrap unavailable; refusing untrusted EAB gateway')
    # The controller is already in the outer network namespace. Share only that
    # namespace, which contains the metering bridge and local test services.
    args = ['bwrap', '--die-with-parent', '--new-session', '--unshare-all', '--share-net', '--cap-drop', 'ALL']
    for path in ('/usr', '/bin', '/sbin', '/lib', '/lib64', '/etc', binary):
        if Path(path).exists():
            args += ['--ro-bind', str(path), str(path)]
    for path in hook_reads:
        args += ['--ro-bind', str(path), str(path)]
    for path in hook_writes:
        args += ['--bind', str(path), str(path)]
    return args + ['--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp',
                   '--bind', str(workspace), str(workspace), '--bind', str(home), str(home),
                   '--chdir', str(workspace), *map(str, argv)]


if __name__ == '__main__':
    settings = json.loads(Path(sys.argv[1]).read_text())
    env = dict(os.environ)
    temporary = Path(settings['root']) / 'runtime/isolated-home/tmp'
    temporary.mkdir(parents=True, exist_ok=True)
    env['TMPDIR'] = str(temporary.resolve())
    runner = settings.get('runner')
    hook_script = Path(runner).parent / 'hook_collect.py' if runner else None
    argv = gateway_command([settings['binary'], *sys.argv[2:]], settings['root'], settings['binary'], settings['proxy_url'], hook_script)
    os.chdir(Path(settings['root'])/'runtime/workspace')
    # Keep the harness-owned PID: stop signals must reach the gateway itself,
    # rather than killing a wrapper and leaving its listener behind.
    os.execvpe(argv[0], argv, env)
