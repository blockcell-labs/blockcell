import http.client
import http.server
import json
import os
import shutil
import socket
import socketserver
import threading
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from .http_transport import open_upstream


class BudgetExceeded(RuntimeError):
    pass


class ProviderUnavailable(RuntimeError):
    pass


class Meter:
    def __init__(self, limit, input_rate=0, output_rate=0, token_limit=1000000, on_update=None, concurrency=1, retry_429_seconds=300, user_agent='Mozilla/5.0 (compatible; Genesis/0.1.4)'):
        if type(concurrency) is not int or not 1 <= concurrency <= 5:
            raise ValueError('concurrency must be an integer from 1 to 5')
        self.slots = threading.BoundedSemaphore(concurrency)
        self.retry_429_seconds = retry_429_seconds
        self.user_agent = user_agent
        self.active_requests = self.peak_requests = self.requests = self.retries_429 = 0
        self.limit = limit
        self.input_rate = input_rate
        self.output_rate = output_rate
        self.token_limit = token_limit
        self.input_tokens = self.output_tokens = 0
        self.cost = 0.0
        self.lock = threading.Lock()
        self.on_update = on_update
        self.upstream_status = None
        self.missing_usage_requests = 0
        self.reserved_cost = 0.0
        self.reservations = []
        self.upstream_stream = True
        self.last_proxy_error = None
        self.last_invalid_usage = None

    @property
    def spent(self):
        return self.cost + self.reserved_cost

    def check(self):
        if self.missing_usage_requests:
            raise ProviderUnavailable('upstream usage missing; reconcile unknown request costs before resuming')
        if self.upstream_status in (401, 402, 403, 429) or (self.upstream_status is not None and 500 <= self.upstream_status <= 599):
            raise ProviderUnavailable(f'upstream HTTP {self.upstream_status}; restore provider access, then resume')
        if self.spent >= self.limit or self.input_tokens + self.output_tokens >= self.token_limit:
            raise BudgetExceeded("LLM budget exhausted")

    def charge(self, usage):
        with self.lock:
            inputs = usage.get('prompt_tokens', usage.get('input_tokens')) if isinstance(usage, dict) else None
            outputs = usage.get('completion_tokens', usage.get('output_tokens')) if isinstance(usage, dict) else None
            if any(type(value) is not int or value < 0 for value in (inputs, outputs)):
                self.missing_usage_requests += 1
                self.last_invalid_usage = {'type': type(usage).__name__, 'time': time.time(),
                    'input_type': type(inputs).__name__, 'output_type': type(outputs).__name__,
                    'input_numeric': inputs if type(inputs) is int else None,
                    'output_numeric': outputs if type(outputs) is int else None}
            else:
                self.input_tokens += inputs
                self.output_tokens += outputs
            self.cost = (self.input_tokens * self.input_rate + self.output_tokens * self.output_rate) / 1000000
            if self.on_update:
                self.on_update(self)


def completion_sse(data):
    """Convert one complete OpenAI response without altering its semantics."""
    choices = data.get('choices')
    if not isinstance(choices, list) or not choices:
        raise ValueError('upstream completion has no choices')
    converted = []
    for index, choice in enumerate(choices):
        message = choice.get('message')
        if not isinstance(message, dict) or not isinstance(choice.get('finish_reason'), str):
            raise ValueError('incomplete upstream completion')
        delta = dict(message)
        if delta.get('tool_calls'):
            delta['tool_calls'] = [dict(tool, index=i) for i, tool in enumerate(delta['tool_calls'])]
        converted.append({'index': choice.get('index', index), 'delta': delta, 'finish_reason': choice['finish_reason']})
    metadata = {key: data[key] for key in ('id', 'model', 'created') if key in data}
    events = [dict(metadata, object='chat.completion.chunk', choices=converted),
              dict(metadata, object='chat.completion.chunk', choices=[], usage=data.get('usage'))]
    return (''.join('data: ' + json.dumps(event, ensure_ascii=False) + '\n\n' for event in events)
            + 'data: [DONE]\n\n').encode()


class Proxy:
    def __init__(self, upstream, key, meter, socket_path=None, allowed_model=None):
        self.upstream = upstream.rstrip("/")
        self.key = key
        self.meter = meter
        self.closing = False
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *args):
                pass

            def send_error(self, *args, **kwargs):
                try:
                    if getattr(self, 'streaming_started', False):
                        self.wfile.write(('data: ' + json.dumps({'error': {'message': 'upstream unavailable', 'code': args[0]}}) + '\n\ndata: [DONE]\n\n').encode())
                        self.wfile.flush()
                        return
                    super().send_error(*args, **kwargs)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def do_POST(self):
                with owner.meter.slots:
                    with owner.meter.lock:
                        owner.meter.active_requests += 1
                        owner.meter.peak_requests = max(owner.meter.peak_requests, owner.meter.active_requests)
                        owner.meter.requests += 1
                    try:
                        self.forward()
                    finally:
                        with owner.meter.lock:
                            owner.meter.active_requests -= 1

            def forward(self):
                sent_headers = False
                upstream_attempted = accounted = False
                response_shape = None
                def record_error(exc):
                    with owner.meter.lock:
                        owner.meter.last_proxy_error = {'type': type(exc).__name__,
                            'upstream_attempted': upstream_attempted, 'accounted': accounted,
                            'response_shape': response_shape, 'time': time.time()}
                        if owner.meter.on_update:
                            owner.meter.on_update(owner.meter)
                if owner.closing:
                    self.send_error(503, 'proxy closing')
                    return
                try:
                    with owner.meter.lock:
                        owner.meter.check()
                    length = int(self.headers.get("Content-Length", "0"))
                    if length > 16 * 1024 * 1024:
                        self.send_error(413)
                        return
                    body = self.rfile.read(length)
                    payload = json.loads(body)
                    if allowed_model and payload.get('model') != allowed_model:
                        self.send_error(403, 'model not allowed')
                        return
                    client_stream = bool(payload.get('stream'))
                    buffered = client_stream and not owner.meter.upstream_stream
                    if client_stream:
                        if buffered:
                            payload['stream'] = False
                            payload.pop('stream_options', None)
                        else:
                            payload["stream_options"] = {"include_usage": True}
                        body = json.dumps(payload).encode()
                    path = self.path.split("?", 1)[0]
                    if path not in ("/v1/chat/completions", "/chat/completions", "/v1/embeddings", "/embeddings"):
                        self.send_error(403, "unsupported proxy endpoint")
                        return
                    suffix = path.removeprefix("/v1")
                    waited, attempt = 0, 0
                    while True:
                        try:
                            upstream_attempted = True
                            response = open_upstream(owner.upstream + suffix, body, owner.key, owner.meter.user_agent,
                                                     getattr(owner.meter, 'transport_command', None))
                            break
                        except urllib.error.HTTPError as exc:
                            if exc.code != 429 or waited >= owner.meter.retry_429_seconds:
                                raise
                            try:
                                retry_after = float(exc.headers.get('Retry-After', 0))
                            except ValueError:
                                retry_after = 0
                            exc.close()
                            delay = min(max(min(2 ** min(attempt, 6), 30), retry_after), owner.meter.retry_429_seconds - waited)
                            with owner.meter.lock:
                                owner.meter.retries_429 += 1
                            if client_stream:
                                if not sent_headers:
                                    self.send_response(200)
                                    self.send_header('Content-Type', 'text/event-stream')
                                    self.end_headers()
                                    sent_headers = True
                                    self.streaming_started = True
                                remaining = delay
                                while remaining > 0:
                                    try:
                                        self.wfile.write(b': upstream rate limited; retrying\n\n')
                                        self.wfile.flush()
                                    except (BrokenPipeError, ConnectionResetError):
                                        pass
                                    time.sleep(min(remaining, 10))
                                    remaining -= min(remaining, 10)
                            else:
                                time.sleep(delay)
                            waited += delay
                            attempt += 1
                            owner.meter.check()
                    with response:
                        content_type = response.headers.get("Content-Type", "application/json")
                        if 'text/event-stream' not in content_type:
                            data = response.read(16 * 1024 * 1024 + 1)
                            if len(data) > 16 * 1024 * 1024:
                                raise ValueError('upstream response exceeds limit')
                            completion = json.loads(data)
                            if not isinstance(completion, dict):
                                raise ValueError('invalid upstream response')
                            response_shape = {'status': response.status, 'has_choices': bool(completion.get('choices')),
                                              'has_error': 'error' in completion, 'has_usage': 'usage' in completion}
                            owner.meter.charge(completion.get('usage'))
                            accounted = True
                            if owner.meter.missing_usage_requests:
                                raise ProviderUnavailable('upstream usage missing or invalid')
                            if client_stream:
                                data = completion_sse(completion)
                                content_type = 'text/event-stream'
                            if not sent_headers:
                                self.send_response(response.status)
                                self.send_header('Content-Type', content_type)
                                self.end_headers()
                            try:
                                self.wfile.write(data)
                            except (BrokenPipeError, ConnectionResetError):
                                pass
                            return
                        if buffered:
                            raise ValueError('upstream ignored non-streaming mode')
                        disconnected = False
                        try:
                            if not sent_headers:
                                self.send_response(response.status)
                                self.send_header("Content-Type", content_type)
                                self.end_headers()
                        except (BrokenPipeError, ConnectionResetError):
                            disconnected = True
                        usage = None
                        if "text/event-stream" in content_type:
                            for line in response:
                                if not disconnected:
                                    try:
                                        self.wfile.write(line)
                                        self.wfile.flush()
                                    except (BrokenPipeError, ConnectionResetError):
                                        disconnected = True
                                if line.startswith(b"data:"):
                                    try:
                                        usage = json.loads(line[5:].strip()).get("usage") or usage
                                    except (ValueError, AttributeError):
                                        pass
                        # Missing usage is fail-closed for all subsequent requests.
                        owner.meter.charge(usage)
                        accounted = True
                except BudgetExceeded as exc:
                    self.send_error(429, str(exc))
                except ProviderUnavailable as exc:
                    record_error(exc)
                    self.send_error(owner.meter.upstream_status or 503, str(exc))
                except urllib.error.HTTPError as exc:
                    record_error(exc)
                    if exc.code in (401, 402, 403, 429) or 500 <= exc.code <= 599:
                        with owner.meter.lock:
                            owner.meter.upstream_status = exc.code
                            if owner.meter.on_update:
                                owner.meter.on_update(owner.meter)
                    self.send_error(exc.code, "upstream request failed")
                except (BrokenPipeError, ConnectionResetError) as exc:
                    if upstream_attempted and not accounted:
                        owner.meter.charge(None)
                        with owner.meter.lock:
                            owner.meter.upstream_status = 502
                        record_error(exc)
                    return
                except (OSError, ValueError) as exc:
                    # A transport error may occur after the provider accepted
                    # the request. Its cost is unknown, not zero.
                    if upstream_attempted and not accounted:
                        owner.meter.charge(None)
                    if upstream_attempted:
                        with owner.meter.lock:
                            owner.meter.upstream_status = 502
                    record_error(exc)
                    self.send_error(502, "proxy request failed")

        if socket_path:
            class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
                daemon_threads = True
            self.server = Server(str(socket_path), Handler)
            self.url = "http://127.0.0.1:18788/v1"
        else:
            self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            self.url = f"http://127.0.0.1:{self.server.server_port}/v1"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.server.daemon_threads = False

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.closing = True
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        if self.meter.upstream_status is not None:
            self.meter.check()


def clean_env(home, key_env):
    allowed = ("PATH", "LANG", "LC_ALL", "TZ", "CARGO_HOME", "RUSTUP_HOME")
    env = {k: os.environ[k] for k in allowed if k in os.environ and k != key_env}
    env.update(HOME=str(home), CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
               RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
    from .preflight import toolchain_bin
    pinned = toolchain_bin()
    if pinned:
        env['PATH'] = pinned + os.pathsep + env.get('PATH', '/usr/bin:/bin')
    return env


def prepare_home(home, workspace, config, proxy_url):
    home = Path(home)
    directory = home / ".blockcell"
    directory.mkdir(parents=True, exist_ok=True)
    agent = config["agent"]
    data = {"providers": {agent["provider"]: {"apiBase": proxy_url, "apiKey": "genesis-proxy-only", "apiType": "openai", "proxy": ""}},
            "agents": {"defaults": {"workspace": str(workspace), "model": agent["model"], "provider": agent["provider"], "modelPool": []}, "ghost": {"enabled": False}},
            "channels": {}, "autoUpgrade": {"enabled": False},
            "selfImprove": {"nudge": {"enabled": False}, "review": {"enabled": False}},
            "memory": {"learningSystem": {"enabled": False}},
            "tools": {"exec": {"restrictToWorkspace": True, "allowUnsandboxedShell": False}}}
    if agent['require_bwrap'] and sys.platform == 'darwin':
        # Every descendant already inherits the outer Seatbelt profile. Avoid a
        # second sandbox_init, which macOS refuses under a restrictive sandbox.
        data['tools']['exec']['sandboxPolicy'] = 'full-access'
        data['tools']['exec']['approvalPolicy'] = 'workspace-auto'
    from .preflight import toolchain_bin
    executables = [sys.executable, str(Path(sys.executable).resolve()), '/usr/bin/python3']
    pinned = toolchain_bin()
    if pinned:
        executables += [str(Path(pinned) / tool) for tool in ('cargo', 'rustc', 'rustfmt', 'clippy-driver')]
    policy = {'version': 1, 'default_policy': 'confirm', 'builtin_protected_paths': True,
              'rules': [{'name': 'genesis-test-executables', 'action': 'allow', 'ops': ['exec'], 'paths': executables}]}
    (directory / 'path_access.json5').write_text(json.dumps(policy))
    (directory / "config.json5").write_text(json.dumps(data, ensure_ascii=False, indent=2))
    env = clean_env(home, agent["key_env"])
    temporary = home / 'tmp'
    temporary.mkdir(exist_ok=True)
    env['TMPDIR'] = str(temporary.resolve())
    return env


def sandbox(argv, workspace, home, binary, socket_path, readonly=(), rust_target=None, proxy_url=None):
    if sys.platform == 'darwin':
        from .native_sandbox import macos_command
        roots = [workspace, home, binary, *readonly]
        writable = [workspace, home]
        if rust_target is not None:
            roots += [Path(os.environ.get('RUSTUP_HOME', str(Path.home() / '.rustup')))]
            host_cargo = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
            cargo = Path(workspace) / '.genesis-cargo'
            cargo.mkdir(exist_ok=True)
            for name in ('bin', 'registry', 'git'):
                source = host_cargo / name
                if source.exists():
                    target = cargo / name
                    if not target.exists():
                        target.symlink_to(source.resolve(), target_is_directory=True)
                    roots.append(source)
            target = Path(workspace) / 'target'
            if not target.exists():
                target.symlink_to(Path(rust_target).resolve(), target_is_directory=True)
            writable.append(rust_target)
        return macos_command(argv, roots, writable, proxy_url)
    if not shutil.which("bwrap"):
        raise RuntimeError("bubblewrap missing: install with sudo apt-get install bubblewrap; refusing untrusted agent execution")
    helper = Path(__file__).with_name("sandbox_bridge.py").resolve()
    args = ["bwrap", "--die-with-parent", "--new-session", "--unshare-all", "--cap-drop", "ALL"]
    for path in ("/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"):
        if Path(path).exists():
            args += ["--ro-bind", path, path]
    args += ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
             "--bind", str(workspace), str(workspace), "--bind", str(home), str(home),
             "--ro-bind", str(binary), str(binary), "--ro-bind", str(helper), "/genesis_bridge.py",
             "--ro-bind", str(socket_path), "/genesis_proxy.sock", "--chdir", str(workspace)]
    for path in readonly:
        args += ["--ro-bind", str(path), str(path)]
    if rust_target is not None:
        host_cargo = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
        rustup = Path(os.environ.get('RUSTUP_HOME', str(Path.home() / '.rustup')))
        cargo_home = Path(home) / 'cargo'
        cargo_home.mkdir(exist_ok=True)
        args += ['--bind', str(cargo_home), str(Path(workspace) / '.genesis-cargo'),
                 '--ro-bind', str(rustup), str(rustup),
                 '--bind', str(rust_target), str(Path(workspace) / 'target')]
        for name in ('bin', 'registry', 'git'):
            if (host_cargo / name).exists():
                args += ['--ro-bind', str(host_cargo / name), str(Path(workspace) / '.genesis-cargo' / name)]
    return args + ["/usr/bin/python3", "/genesis_bridge.py", *argv]


def build_sandbox(argv, workspace, target, home, env):
    from .native_sandbox import cargo_environment, macos_command
    caches = cargo_environment(home, env)
    target.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    if sys.platform == 'darwin':
        return macos_command(argv, [workspace, home, env['RUSTUP_HOME'], *caches], [workspace, target, home])
    if not shutil.which("bwrap"):
        raise RuntimeError("bubblewrap missing: sudo apt-get install bubblewrap")
    args = ["bwrap", "--die-with-parent", "--new-session", "--unshare-all", "--cap-drop", "ALL"]
    for path in ("/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", env["RUSTUP_HOME"], *caches):
        if Path(path).exists():
            args += ["--ro-bind", str(path), str(path)]
    target.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    for name in (".package-cache", ".package-cache-mutate"):
        lock = home / name
        lock.touch(exist_ok=True)
        args += ["--bind", str(lock), str(Path(env["CARGO_HOME"]) / name)]
    args += ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
             "--bind", str(workspace), str(workspace), "--bind", str(target), str(target),
             "--bind", str(home), str(home), "--chdir", str(workspace)]
    return args + [str(x) for x in argv]


def offline_sandbox(argv, workspace, home, readonly=()):
    """Run candidate-derived verification offline, never in the host interpreter."""
    Path(home).mkdir(parents=True, exist_ok=True)
    if sys.platform == 'darwin':
        from .native_sandbox import macos_command
        return macos_command(argv, [workspace, home, *readonly], [workspace, home])
    if not shutil.which('bwrap'):
        raise RuntimeError('bubblewrap unavailable; refusing untrusted verification')
    args = ['bwrap', '--die-with-parent', '--new-session', '--unshare-all', '--cap-drop', 'ALL']
    for path in ('/usr', '/bin', '/sbin', '/lib', '/lib64', '/etc', *readonly):
        if Path(path).exists():
            args += ['--ro-bind', str(path), str(path)]
    return args + ['--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp',
                   '--bind', str(workspace), str(workspace), '--bind', str(home), str(home),
                   '--chdir', str(workspace), *map(str, argv)]
