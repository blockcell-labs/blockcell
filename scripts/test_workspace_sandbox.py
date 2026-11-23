#!/usr/bin/env python3
"""Run trusted Rust workspace tests in a credential-free macOS test sandbox.

Production Genesis build/candidate profiles are unchanged. This test-only
profile permits localhost fixtures and literal ancestor directory reads needed
by descriptor-relative archive traversal, while denying external networking.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO))
from genesis.adapters import cargo
from genesis.config import load
from genesis.isolation import build_sandbox


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=REPO / 'tmp/sandbox-workspace-tests')
    parser.add_argument('cargo_args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('this runner validates macOS Seatbelt only')
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    config = load()
    config['_root'] = str(root)
    env = cargo.environment(config)
    target = REPO / 'target'
    env['CARGO_TARGET_DIR'] = str(target)
    argv = ['cargo', 'test', '--workspace', '--no-fail-fast', '--offline']
    if args.cargo_args:
        extra = args.cargo_args[1:] if args.cargo_args[0] == '--' else args.cargo_args
        argv = ['cargo', 'test', '--offline', *extra]
    sandbox = build_sandbox(argv, REPO, target, root / 'build-home', env)
    # Test HTTP/WebSocket fixtures bind ephemeral loopback ports; never allow
    # external hosts. This exception is confined to this explicit test runner.
    profile = sandbox[2]
    profile += '\n(allow network-bind network-inbound (local ip "localhost:*"))'
    profile += '\n(allow network-outbound (remote ip "localhost:*"))'
    # openat/O_NOFOLLOW archive traversal opens each ancestor directory. Only
    # directory objects are granted, not the contents of their subdirectories.
    for parent in REPO.parents:
        profile += '\n(allow file-read* (literal ' + json.dumps(str(parent)) + '))'
    sandbox[2] = profile
    (root / 'profile.sb').write_text(profile)
    (root / 'command.json').write_text(json.dumps(argv))
    with (root / 'workspace.log').open('w') as log:
        result = subprocess.run(sandbox, cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT)
    print('Test log:', root / 'workspace.log', flush=True)
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
