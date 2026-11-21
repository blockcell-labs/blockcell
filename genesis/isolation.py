import http.client
import http.server
import json
import os
import shutil
import socket
import socketserver
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path


class BudgetExceeded(RuntimeError):
    pass


class ProviderUnavailable(RuntimeError):
    pass


class Meter:
    def __init__(self, limit, input_rate=0, output_rate=0, token_limit=1000000, on_update=None, concurrency=1, retry_429_seconds=300, user_agent='Mozilla/5.0 (compatible; Genesis/0.1.3)'):
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

    def check(self):
        if self.upstream_status in (401, 402, 403, 429):
            raise ProviderUnavailable(f'upstream HTTP {self.upstream_status}; restore provider access, then resume')
        if self.cost >= self.limit or self.input_tokens + self.output_tokens >= self.token_limit:
            raise BudgetExceeded("LLM budget exhausted")

    def charge(self, usage):
        with self.lock:
            self.input_tokens += int(usage.get("prompt_tokens", usage.get("input_tokens", 0)))
            self.output_tokens += int(usage.get("completion_tokens", usage.get("output_tokens", 0)))
            self.cost = (self.input_tokens * self.input_rate + self.output_tokens * self.output_rate) / 1000000
            if self.on_update:
                self.on_update(self)


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
                    if payload.get("stream"):
                        payload["stream_options"] = {"include_usage": True}
                        body = json.dumps(payload).encode()
                    path = self.path.split("?", 1)[0]
                    if path not in ("/v1/chat/completions", "/chat/completions", "/v1/embeddings", "/embeddings"):
                        self.send_error(403, "unsupported proxy endpoint")
                        return
                    suffix = path.removeprefix("/v1")
                    req = urllib.request.Request(owner.upstream + suffix, data=body, headers={
                        "Content-Type": "application/json", "Authorization": "Bearer " + owner.key,
                        "User-Agent": owner.meter.user_agent})
                    waited, attempt = 0, 0
                    while True:
                        try:
                            response = urllib.request.urlopen(req, timeout=120)
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
                            if payload.get('stream'):
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
                        else:
                            data = response.read()
                            usage = json.loads(data).get("usage")
                            owner.meter.charge(usage or {"prompt_tokens": owner.meter.token_limit})
                            try:
                                self.wfile.write(data)
                            except (BrokenPipeError, ConnectionResetError):
                                pass
                            return
                        # Missing usage is fail-closed for all subsequent requests.
                        if usage is None:
                            usage = {"prompt_tokens": owner.meter.token_limit}
                        owner.meter.charge(usage)
                except BudgetExceeded as exc:
                    self.send_error(429, str(exc))
                except ProviderUnavailable as exc:
                    self.send_error(owner.meter.upstream_status or 503, str(exc))
                except urllib.error.HTTPError as exc:
                    if exc.code in (401, 402, 403, 429):
                        with owner.meter.lock:
                            owner.meter.upstream_status = exc.code
                            if owner.meter.on_update:
                                owner.meter.on_update(owner.meter)
                    self.send_error(exc.code, "upstream request failed")
                except (BrokenPipeError, ConnectionResetError):
                    return
                except (OSError, ValueError):
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
        if self.meter.upstream_status in (401, 402, 403, 429):
            self.meter.check()


def clean_env(home, key_env):
    allowed = ("PATH", "LANG", "LC_ALL", "TZ", "CARGO_HOME", "RUSTUP_HOME")
    env = {k: os.environ[k] for k in allowed if k in os.environ and k != key_env}
    env.update(HOME=str(home), CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
               RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
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
    (directory / "config.json5").write_text(json.dumps(data, ensure_ascii=False, indent=2))
    return clean_env(home, agent["key_env"])


def sandbox(argv, workspace, home, binary, socket_path, readonly=()):
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
    return args + ["/usr/bin/python3", "/genesis_bridge.py", *argv]


def build_sandbox(argv, workspace, target, home, env):
    if not shutil.which("bwrap"):
        raise RuntimeError("bubblewrap missing: sudo apt-get install bubblewrap")
    args = ["bwrap", "--die-with-parent", "--new-session", "--unshare-all", "--cap-drop", "ALL"]
    for path in ("/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", env["CARGO_HOME"], env["RUSTUP_HOME"]):
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
