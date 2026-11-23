"""Read-only dependency checks for Genesis."""
import os
import shutil
import subprocess
import sys
from pathlib import Path


def toolchain_bin():
    rustup = shutil.which('rustup')
    if rustup:
        result = subprocess.run([rustup, 'which', 'cargo'], capture_output=True, text=True, timeout=10)
        if result.returncode == 0:
            return str(Path(result.stdout.strip()).parent)
    return ''


def inspect_environment(config, require_key=False):
    errors = []
    tools = {}
    for tool in ('git', 'cargo', 'rustc', 'rustfmt', 'clippy-driver'):
        tools[tool] = shutil.which(tool)
        if not tools[tool]:
            errors.append('missing executable: ' + tool)
    backend = 'sandbox-exec' if sys.platform == 'darwin' else 'bwrap'
    if config['agent']['require_bwrap'] and not shutil.which(backend):
        errors.append('missing native sandbox: ' + backend)
    audit = config['benchmarks'].get('audit_command')
    if audit and audit[:2] == ['cargo', 'audit'] and not shutil.which('cargo-audit'):
        errors.append('missing cargo-audit; install it and prepare its offline advisory database')
    eab = config['benchmarks']['L3']['eab60']
    kit = eab.get('kit') or os.environ.get('EAB60_KIT', '')
    if eab.get('required') and not (kit and (Path(kit) / 'cases/blockcell_eab60.jsonl').is_file()):
        errors.append('EAB60 kit missing; set EAB60_KIT or benchmarks.L3.eab60.kit')
    key_env = config['agent']['key_env']
    if require_key and not os.environ.get(key_env):
        errors.append('missing credential environment variable: ' + key_env)
    transport = config['agent'].get('http_transport_command')
    if transport and (not isinstance(transport, list) or not transport or not shutil.which(transport[0])):
        errors.append('native HTTP transport executable unavailable')
    return {'ready': not errors, 'platform': sys.platform, 'sandbox': backend,
            'tools': tools, 'rustup_toolchain_bin': toolchain_bin(), 'errors': errors}
