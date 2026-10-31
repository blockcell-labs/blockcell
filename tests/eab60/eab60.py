"""EAB-60 harness core: isolated gateway lifecycle, chat client, evidence collection.

Everything here was derived from the checked-out source, not from the kit's
assumptions:

* `POST /v1/chat` is asynchronous.  `bin/blockcell/src/commands/gateway/chat.rs`
  (handle_chat) queues an InboundMessage and answers 202
  `{"status":"accepted","session_id":...}`.  The assistant reply must be read
  back from `GET /v1/sessions/:id` (sessions.rs::handle_session_get).
* The reliable "turn finished" signal is `turn/end` in the JSONL session event
  log (`crates/storage/src/session_event.rs`), located at
  `$HOME/.blockcell/session_events/<session_file_stem(session_key)>.jsonl`
  with `session_key = "ws:<chat_id>"` (`crates/core/src/session_key.rs`).
* Audit records live in `$HOME/.blockcell/audit/YYYY-MM-DD.jsonl`
  (`crates/storage/src/audit.rs`), each line an `AuditRecord` with
  `prev_hash`/`hash`.
* Hooks are shell commands whose `{token}` placeholders are expanded and
  shell-quoted (`crates/agent/src/hooks.rs`); there is no stdin payload.
"""

import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ROOT = Path(os.environ.get("EAB60_ROOT", REPO / "tmp/eab60-optimized")).resolve()
if ROOT == (REPO / "tmp/eab60").resolve():
    raise ValueError("EAB60_ROOT must preserve the original report and evidence")
KIT = Path(os.environ.get("EAB60_KIT", REPO / "tmp/BlockCell-EAB60-Codex-Acceptance-Kit"))
HOME = ROOT / "runtime/isolated-home"
BLOCKCELL_DIR = HOME / ".blockcell"
WORKSPACE = ROOT / "runtime/workspace"
REPORTS = ROOT / "reports"
EVIDENCE = ROOT / "evidence"
LOGS = ROOT / "logs"
SCRIPTS = Path(__file__).resolve().parent

BINARY = Path(os.environ.get("EAB60_BIN", REPO / "target/release/blockcell"))
BASE_URL = "http://127.0.0.1:18790"
API_TOKEN = "eab60-test-token-local"
API_KEY = "123456"

# Files that the runtime itself creates inside the workspace.  They are not
# fixtures and are excluded from "did the agent write something" diffs, but they
# are still recorded in the raw snapshot so the report can show them.
RUNTIME_WORKSPACE_ARTIFACTS = {
    ".system_events.json",
    ".system_summary_queue.json",
    "evo_workflow.db",
    "evo_workflow.db-wal",
    "evo_workflow.db-shm",
    "skill_evolution_workflow.db",
    "skill_evolution_workflow.db-wal",
    "skill_evolution_workflow.db-shm",
}
RUNTIME_WORKSPACE_DIRS = {
    ".blockcell",
    "agents",
    "downloads",
    "evolved_tools",
    "evolution_records",
    "ghost",
    "logs",
    "memory",
    "skills",
    "tool_artifacts",
    "tool_evolution_records",
}


def fixture_names():
    return sorted(p.name for p in (KIT / "fixtures/workspace").iterdir())


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------------------
# Gateway lifecycle
# ---------------------------------------------------------------------------


def _env():
    env = {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "HOME": str(HOME),
        "EAB60_API_KEY": API_KEY,
        "EAB60_API_TOKEN": API_TOKEN,
        "NO_PROXY": "127.0.0.1,localhost",
        "no_proxy": "127.0.0.1,localhost",
        "LANG": "en_US.UTF-8",
        "TZ": "Asia/Shanghai",
    }
    return env


class Gateway:
    def __init__(self, log_name="gateway.log"):
        self.proc = None
        self.log_path = LOGS / log_name

    def _port_open(self):
        with socket.socket() as sock:
            sock.settimeout(0.4)
            return sock.connect_ex(("127.0.0.1", 18790)) == 0

    def stop(self, timeout=15):
        if self.proc:
            try:
                self.proc.send_signal(signal.SIGINT)
                self.proc.wait(timeout=timeout)
            except Exception:
                try:
                    self.proc.kill()
                    self.proc.wait(timeout=5)
                except Exception:
                    pass
            self.proc = None
    def start(self, wait=45):
        self.stop()
        if self._port_open():
            raise RuntimeError("Port 18790 is already in use; no existing process was stopped")
        LOGS.mkdir(parents=True, exist_ok=True)
        handle = open(self.log_path, "ab")
        try:
            self.proc = subprocess.Popen(
                [str(BINARY), "gateway", "--host", "127.0.0.1", "--port", "18790"],
                cwd=str(REPO),
                env=_env(),
                stdout=handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        finally:
            handle.close()
        deadline = time.time() + wait
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(
                    "gateway exited early: " + tail(self.log_path, 15)
                )
            try:
                health()
                # Catch listeners that exit immediately after the health endpoint
                # opens, including a supervisor-triggered shutdown.
                self.assert_survives()
                return True
            except Exception:
                time.sleep(0.5)
        raise RuntimeError("gateway did not become healthy: " + tail(self.log_path, 15))

    def assert_survives(self, seconds=3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.proc is None or self.proc.poll() is not None:
                raise RuntimeError("gateway stopped after startup: " + tail(self.log_path, 15))
            health()
            time.sleep(0.2)


def tail(path, lines=20):
    try:
        with open(path, "r", errors="replace") as fh:
            return "".join(fh.readlines()[-lines:])
    except Exception:
        return ""


# ---------------------------------------------------------------------------
# HTTP client
# ---------------------------------------------------------------------------


def _request(path, body=None, timeout=60, method=None):
    data = None
    headers = {"Authorization": "Bearer " + API_TOKEN}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(
        BASE_URL + path, data=data, headers=headers, method=method
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(req, timeout=timeout) as resp:
        raw = resp.read().decode("utf-8", "replace")
        try:
            return resp.status, json.loads(raw)
        except Exception:
            return resp.status, {"raw": raw}


def health():
    return _request("/v1/health", timeout=5)[1]


def post_chat(chat_id, content):
    return _request(
        "/v1/chat",
        {"content": content, "chat_id": chat_id, "channel": "ws"},
        timeout=30,
    )


def get_session(chat_id):
    try:
        return _request("/v1/sessions/" + urllib.request.quote(chat_id), timeout=20)[1]
    except urllib.error.HTTPError as exc:
        return {"error": exc.code}


def get_tasks():
    try:
        return _request("/v1/tasks", timeout=20)[1]
    except Exception as exc:
        return {"error": str(exc)}


# ---------------------------------------------------------------------------
# Evidence readers
# ---------------------------------------------------------------------------


def session_key(chat_id):
    return "ws:" + chat_id


def event_file(chat_id):
    return BLOCKCELL_DIR / "session_events" / ("ws_" + chat_id + ".jsonl")


def read_events(chat_id):
    path = event_file(chat_id)
    out = []
    if not path.exists():
        return out
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except Exception:
                pass
    return out


def event_seq(chat_id):
    events = read_events(chat_id)
    return events[-1]["seq"] if events else 0


def audit_file():
    date = time.strftime("%Y-%m-%d", time.gmtime())
    return BLOCKCELL_DIR / "audit" / (date + ".jsonl")


def read_audit():
    out = []
    for path in sorted((BLOCKCELL_DIR / "audit").glob("*.jsonl")):
        with open(path, "r", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except Exception:
                    pass
    return out


def read_hooks():
    path = REPORTS / "hook_events.jsonl"
    out = []
    if not path.exists():
        return out
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except Exception:
                pass
    return out


# ---------------------------------------------------------------------------
# Turn execution
# ---------------------------------------------------------------------------


class TurnTimeout(RuntimeError):
    pass


def run_turn(chat_id, content, timeout=240, poll=0.6):
    """Send one user message and wait for the runtime to emit turn/end."""
    before = event_seq(chat_id)
    started = time.time()
    status, body = post_chat(chat_id, content)
    if status not in (200, 202):
        raise RuntimeError("chat rejected: %s %s" % (status, body))
    deadline = started + timeout
    while time.time() < deadline:
        events = read_events(chat_id)
        if any(e.get("seq", 0) > before and e.get("kind") == "turn/end" for e in events):
            break
        time.sleep(poll)
    else:
        raise TurnTimeout("no turn/end within %ss for %s" % (timeout, chat_id))
    duration = time.time() - started
    events = [e for e in read_events(chat_id) if e.get("seq", 0) > before]
    # The session store is written after turn/end; reading immediately can return
    # an empty history, which used to be recorded as an empty delivered answer.
    messages = []
    for _ in range(8):
        messages = get_session(chat_id).get("messages", []) or []
        if messages:
            break
        time.sleep(0.25)
    final_text = last_assistant_text(messages)
    source = "session"
    if not final_text:
        final_text = last_assistant_event_text(events)
        source = "event" if final_text else "empty"
    return {
        "chat_id": chat_id,
        "prompt": content,
        "http_status": status,
        "accepted": body,
        "duration_s": round(duration, 3),
        "messages": messages,
        "events": events,
        "final_text": final_text,
        "final_text_source": source,
    }


def last_assistant_event_text(events):
    for event in reversed(events):
        if event.get("kind") == "assistant/message":
            content = (event.get("payload") or {}).get("content") or ""
            if content.strip():
                return content
    return ""


def last_assistant_text(messages):
    for msg in reversed(messages):
        if msg.get("role") == "assistant" and (msg.get("content") or "").strip():
            return msg["content"]
    return ""


def tool_calls_of(events):
    return [
        {
            "tool": e["payload"].get("tool"),
            "arguments": e["payload"].get("arguments"),
            "call_id": e["payload"].get("call_id"),
            "environment": e["payload"].get("environment"),
        }
        for e in events
        if e.get("kind") == "tool/call"
    ]


def tool_results_of(events):
    return [
        {
            "call_id": e["payload"].get("call_id"),
            "outcome": e["payload"].get("outcome"),
            "is_error": e["payload"].get("is_error"),
            "result": e["payload"].get("result"),
            "duration_ms": e["payload"].get("duration_ms"),
        }
        for e in events
        if e.get("kind") == "tool/result"
    ]


# ---------------------------------------------------------------------------
# Workspace snapshots / reset
# ---------------------------------------------------------------------------


def snapshot_workspace():
    files = {}
    for path in sorted(WORKSPACE.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(WORKSPACE).as_posix()
        try:
            files[rel] = {"size": path.stat().st_size, "sha256": sha256_file(path)}
        except OSError:
            files[rel] = {"size": None, "sha256": None}
    return files


def is_runtime_artifact(rel):
    parts = rel.split("/")
    if parts[0] in RUNTIME_WORKSPACE_DIRS:
        return True
    if parts[0] in RUNTIME_WORKSPACE_ARTIFACTS:
        return True
    return False


def diff_snapshots(before, after):
    added = sorted(k for k in after if k not in before)
    removed = sorted(k for k in before if k not in after)
    changed = sorted(
        k
        for k in after
        if k in before and after[k]["sha256"] != before[k]["sha256"]
    )
    return {
        "added": added,
        "removed": removed,
        "changed": changed,
        "added_agent": [p for p in added if not is_runtime_artifact(p)],
        "changed_agent": [p for p in changed if not is_runtime_artifact(p)],
        "removed_agent": [p for p in removed if not is_runtime_artifact(p)],
    }


def reset_workspace(extra_files=None):
    """Restore the fixture set and drop everything the agent produced."""
    fixtures = KIT / "fixtures/workspace"
    keep = set(fixture_names())
    for path in list(WORKSPACE.rglob("*")):
        rel = path.relative_to(WORKSPACE).as_posix()
        if path.is_dir():
            continue
        if is_runtime_artifact(rel):
            continue
        if rel in keep:
            continue
        try:
            path.unlink()
        except OSError:
            pass
    for name in keep:
        src = fixtures / name
        dst = WORKSPACE / name
        if src.is_file():
            shutil.copy2(src, dst)
    out_dir = WORKSPACE / "output"
    if out_dir.exists():
        for path in list(out_dir.rglob("*")):
            if path.is_file():
                path.unlink()
    else:
        out_dir.mkdir(parents=True, exist_ok=True)
    for name, content in (extra_files or {}).items():
        target = WORKSPACE / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_text(content, encoding="utf-8")


def write_workspace(rel, content):
    path = WORKSPACE / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, bytes):
        path.write_bytes(content)
    else:
        path.write_text(content, encoding="utf-8")
    return path


def collect_artifacts(dest, diff, limit=40, max_bytes=400_000):
    """Copy produced/changed files into the evidence dir.

    The workspace is reset before the next case, so without this the artifacts a
    case produced would be gone by the time a reviewer (or an offline re-grade)
    looks at them.
    """
    dest.mkdir(parents=True, exist_ok=True)
    copied = []
    rels = (diff.get("added_agent") or []) + (diff.get("changed_agent") or [])
    for rel in rels[:limit]:
        src = WORKSPACE / rel
        if not src.is_file():
            continue
        try:
            if src.stat().st_size > max_bytes:
                continue
            target = dest / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(src.read_bytes())
            copied.append(rel)
        except OSError:
            pass
    return copied


# ---------------------------------------------------------------------------
# Misc helpers
# ---------------------------------------------------------------------------


def read_text(rel):
    path = WORKSPACE / rel
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return ""


def save_json(path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=2)


def contains_number(text, number):
    """True when the literal integer/decimal appears as a standalone token.

    Thousands separators are normalised away first: models routinely answer
    "5,100" for 5100 and that is the same value, not a wrong one.
    """
    import re

    if number is None:
        return False
    normalized = re.sub(r"(?<=\d),(?=\d{3}\b)", "", text)
    candidates = {str(int(number))}
    if isinstance(number, float) and number != int(number):
        candidates.add(("%.2f" % number).rstrip("0").rstrip("."))
        candidates.add(str(number))
    for cand in candidates:
        if re.search(r"(?<![\d.])" + re.escape(cand) + r"(?![\d])", normalized):
            return True
    return False


def xlsx_read(path):
    """Minimal xlsx reader (no third-party deps): returns {sheet: [[cell,...]]}."""
    import re
    import zipfile
    from xml.etree import ElementTree as ET

    ns = {"m": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    out = {}
    with zipfile.ZipFile(path) as zf:
        names = zf.namelist()
        shared = []
        if "xl/sharedStrings.xml" in names:
            root = ET.fromstring(zf.read("xl/sharedStrings.xml"))
            for si in root.findall("m:si", ns):
                shared.append("".join(t.text or "" for t in si.iter(
                    "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}t")))
        workbook = ET.fromstring(zf.read("xl/workbook.xml"))
        rels = {}
        if "xl/_rels/workbook.xml.rels" in names:
            rel_root = ET.fromstring(zf.read("xl/_rels/workbook.xml.rels"))
            for rel in rel_root:
                rels[rel.get("Id")] = rel.get("Target")
        sheets = []
        for sheet in workbook.find("m:sheets", ns):
            rid = sheet.get(
                "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id"
            )
            target = rels.get(rid, "worksheets/sheet1.xml")
            if not target.startswith("xl/"):
                target = "xl/" + target.lstrip("/")
            sheets.append((sheet.get("name"), target))
        for name, target in sheets:
            if target not in names:
                continue
            root = ET.fromstring(zf.read(target))
            rows = []
            for row in root.iter(
                "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}row"
            ):
                cells = {}
                for cell in row:
                    ref = cell.get("r") or ""
                    col = re.match(r"([A-Z]+)", ref)
                    idx = 0
                    if col:
                        for ch in col.group(1):
                            idx = idx * 26 + (ord(ch) - 64)
                        idx -= 1
                    value = cell.find("m:v", ns)
                    text = value.text if value is not None else None
                    if cell.get("t") == "s" and text is not None:
                        text = shared[int(text)]
                    elif cell.get("t") == "inlineStr":
                        node = cell.find("m:is", ns)
                        text = "".join(t.text or "" for t in node.iter()) if node is not None else ""
                    cells[idx] = text
                width = max(cells) + 1 if cells else 0
                rows.append([cells.get(i) for i in range(width)])
            out[name] = rows
    return out
