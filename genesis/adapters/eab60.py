import json
import os
import sys
import uuid
from pathlib import Path

from ..source import command
from ..isolation import Proxy, clean_env, sandbox


def evaluate(frozen, binary, root, config, split, meter):
    options = config["benchmarks"]["L3"]["eab60"]
    kit = options.get("kit") or os.environ.get("EAB60_KIT")
    if not kit:
        return {"status": "skipped", "reason": "EAB60_KIT not configured", "results": []}
    ids = options[split]
    if not ids:
        return {"status": "skipped", "reason": "EAB60 split has no task IDs", "results": []}
    root.mkdir(parents=True, exist_ok=True)
    home = root / "home"
    home.mkdir(exist_ok=True)
    env = clean_env(home, config["agent"]["key_env"])
    env.update(EAB60_BIN=str(binary), EAB60_ROOT=str(root), EAB60_KIT=str(Path(kit).resolve()))
    settings = config["agent"]
    sockets = Path(config["_root"]) / "sockets"
    sockets.mkdir(exist_ok=True)
    socket_path = sockets / (uuid.uuid4().hex + ".sock") if settings["require_bwrap"] else None
    launcher = Path(__file__).resolve().parents[1] / "eab_launcher.py"
    with Proxy(settings["upstream"], os.environ.get(settings["key_env"], ""), meter, socket_path, settings['model']) as proxy:
        context = root / "context.json"
        context.write_text(json.dumps({"runner": str(frozen / "tests/eab60/run_all.py"), "model": settings["model"], "proxy_url": proxy.url, "ids": ids}))
        argv = [sys.executable, str(launcher), str(context)]
        if settings["require_bwrap"]:
            argv = sandbox(["/usr/bin/python3", str(launcher), str(context)], root, home, binary, socket_path, (frozen, launcher, Path(kit).resolve()))
        command(argv, root, config["genesis"]["max_cycle_minutes"] * 60, env)
    return parse_report(root, ids)


def parse_report(root, ids):
    records = json.loads((root / "reports/results.json").read_text())
    rows = []
    unavailable = False
    for record in records:
        grade = record["grade"]
        events_path = root / 'evidence' / record['id'] / 'events.json'
        events = json.loads(events_path.read_text()) if events_path.exists() else []
        provider_failure = any(e.get('kind') == 'model/request-error' and 'HTTP_STATUS=402' in e.get('payload', {}).get('error', '') for e in events)
        unavailable = unavailable or provider_failure
        usages = [e['payload'].get('usage', {}) for e in events if e.get('kind') == 'model/response']
        rows.append({"task_id": record["id"], "passed": grade["status"] == "PASS",
                     "duration_ms": record.get("duration_s", 0) * 1000,
                     "input_tokens": sum(u.get('input_tokens', 0) for u in usages),
                     "output_tokens": sum(u.get('output_tokens', 0) for u in usages),
                     "tool_calls": len(record.get('tool_calls', [])),
                     "tool_errors": sum(bool(t.get('is_error')) for t in record.get('tool_results', [])),
                     "trace_path": str(root / 'evidence' / record['id'] / 'events.json'),
                     "agent_output_tail": record.get('final_text', '')[-2000:],
                     "failure_reason": 'upstream HTTP 402 Payment Required' if provider_failure else '; '.join(
                         grade.get('notes', []) + [c['name'] + ': ' + str(c.get('detail', '')) for c in grade.get('checks', []) if not c['ok']])})
    if {r["task_id"] for r in rows} != set(ids):
        raise ValueError("EAB60 missing requested task IDs")
    inconclusive = unavailable or any(r["grade"]["status"] == "INCONCLUSIVE" for r in records)
    return {"status": "inconclusive" if inconclusive else "ok", "results": rows}
