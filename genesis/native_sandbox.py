"""macOS Seatbelt isolation with explicit file roots and optional proxy access."""
import json
import os
import shutil
import sys
from pathlib import Path
from urllib.parse import urlsplit


def _path_rule(operation, path):
    return f'({operation} (subpath {json.dumps(str(Path(path).resolve()))}))'


def macos_command(argv, read_roots, write_roots=(), proxy_url=None, listen_ports=(), local_service_ports=()):
    if not shutil.which('sandbox-exec'):
        raise RuntimeError('macOS sandbox-exec unavailable; refusing untrusted execution')
    host_cargo = Path(os.environ.get('CARGO_HOME', str(Path.home()/'.cargo')))
    granted = [Path(root).resolve() for root in (*read_roots, *write_roots)]
    cargo_directories = {host_cargo.resolve(), (Path.home()/'.cargo').resolve()}
    for root in granted:
        cargo_directories.update(parent/'.cargo' for parent in root.parents
                                 if not any((parent/'.cargo').is_relative_to(scope) for scope in granted))
    hidden = sorted({str((directory/name).resolve())
                     for directory in cargo_directories
                     for name in ('config', 'config.toml', 'credentials', 'credentials.toml')})
    metadata = '(allow file-read-metadata (require-all ' + ' '.join(
        f'(require-not (literal {json.dumps(path)}))' for path in hidden) + '))'
    profile = ['(version 1)', '(deny default)', '(allow process-exec process-fork)',
               '(allow signal (target self) (target same-sandbox))', '(allow sysctl-read)', '(allow mach-lookup)',
               metadata, '(allow file-read* (literal "/") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))',
               '(allow file-write* (literal "/dev/null"))',
               '(allow file-read* file-write* (subpath "/dev/fd") (literal "/dev/tty"))']
    system = ['/usr', '/bin', '/sbin', '/System', '/Library/Frameworks',
              '/Library/Developer', '/Library/Apple', '/Applications/Xcode.app/Contents',
              '/Library/Preferences/com.apple.dt.Xcode.plist',
              '/private/var/db/dyld', '/private/var/db/timezone', '/private/etc/localtime',
              '/private/etc/ssl']
    interpreter = Path(sys.executable).resolve().parent.parent
    for path in [*system, interpreter, *read_roots, *write_roots]:
        profile.append(_path_rule('allow file-read*', path))
    for path in write_roots:
        profile.append(_path_rule('allow file-write*', path))
    # Cargo walks workspace ancestors looking for .cargo/config, independently
    # of CARGO_HOME. Hide host config metadata as well as its contents so that
    # discovery skips it instead of failing on an unreadable configuration.
    for path in hidden:
        profile.append(f'(deny file-read* (literal {json.dumps(path)}))')
    if proxy_url:
        parsed = urlsplit(proxy_url)
        if parsed.hostname != '127.0.0.1' or parsed.scheme != 'http' or not parsed.port:
            raise ValueError('sandbox permits only an explicit local metering proxy')
        profile.append(f'(allow network-outbound (remote ip "localhost:{parsed.port}"))')
    for port in local_service_ports:
        if type(port) is not int or not 0 < port < 65536:
            raise ValueError('invalid local service port')
        profile.append(f'(allow network-outbound (remote ip "localhost:{port}"))')
    for port in listen_ports:
        if type(port) is not int or not 0 < port < 65536:
            raise ValueError('invalid local listener port')
        profile.append(f'(allow network-bind network-inbound (local ip "localhost:{port}"))')
    return ['/usr/bin/sandbox-exec', '-p', '\n'.join(profile), *map(str, argv)]


def cargo_environment(home, env):
    """Share only tools/caches. Never share Cargo config or credential files."""
    host = Path(env['CARGO_HOME']).resolve()
    isolated = Path(home).resolve() / 'cargo'
    isolated.mkdir(parents=True, exist_ok=True)
    roots = []
    for name in ('bin', 'registry', 'git', 'advisory-db'):
        source = host / name
        if source.exists():
            target = isolated / name
            if not target.exists() and not target.is_symlink():
                target.symlink_to(source, target_is_directory=True)
            if target.resolve() != source.resolve():
                raise RuntimeError('isolated Cargo cache pointer changed')
            roots.append(source)
    env['CARGO_HOME'] = str(isolated)
    env['CARGO_NET_OFFLINE'] = 'true'
    env['CARGO_BUILD_JOBS'] = '2'
    env['TMPDIR'] = str(Path(home).resolve() / 'tmp')
    Path(env['TMPDIR']).mkdir(exist_ok=True)
    return roots
