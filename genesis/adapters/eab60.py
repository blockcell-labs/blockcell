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
    with Proxy(settings["upstream"], os.environ.get(settings["key_env"], ""), meter, socket_path) as proxy:
        context = root / "context.json"
        context.write_text(json.dumps({"runner": str(frozen / "tests/eab60/run_all.py"), "model": settings["model"], "proxy_url": proxy.url, "ids": ids}))
        argv = [sys.executable, str(launcher), str(context)]
        if settings["require_bwrap"]:
            argv = sandbox(["/usr/bin/python3", str(launcher), str(context)], root, home, binary, socket_path, (frozen, launcher, Path(kit).resolve()))
        command(argv, root, config["genesis"]["max_cycle_minutes"] * 60, env)
    records = json.loads((root / "reports/results.json").read_text())
    rows = []
    for record in records:
        grade = record["grade"]
        rows.append({"task_id": record["id"], "passed": grade["status"] == "PASS",
                     "duration_ms": record.get("duration_s", 0) * 1000,
                     "failure_reason": "; ".join(grade.get("notes", []))})
    if {r["task_id"] for r in rows} != set(ids):
        raise ValueError("EAB60 missing requested task IDs")
    inconclusive = any(r["grade"]["status"] == "INCONCLUSIVE" for r in records)
    return {"status": "inconclusive" if inconclusive else "ok", "results": rows}
