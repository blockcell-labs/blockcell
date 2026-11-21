import json
import os
import uuid
from pathlib import Path

from ..source import command
from ..config import expand
from ..isolation import Proxy, prepare_home, sandbox


def evaluate(binary, output, cwd, config, meter):
    options = config["benchmarks"]["L3"]["bfcl"]
    if not options["enabled"]:
        return {"status": "skipped", "reason": "BFCL disabled", "results": []}
    if not options["command"]:
        return {"status": "skipped", "reason": "BFCL external command not configured", "results": []}
    argv = [expand(s, binary=binary, output=output) for s in options["command"]]
    settings = config["agent"]
    sockets = Path(config["_root"]) / "sockets"
    sockets.mkdir(exist_ok=True)
    socket_path = sockets / (uuid.uuid4().hex + ".sock") if settings["require_bwrap"] else None
    home = output.parent / "home"
    with Proxy(settings["upstream"], os.environ.get(settings["key_env"], ""), meter, socket_path) as proxy:
        env = prepare_home(home, output.parent, config, proxy.url)
        if settings["require_bwrap"]:
            readonly = [cwd, *map(Path, options.get("readonly_paths", []))]
            readonly += [Path(part) for part in argv if Path(part).is_absolute() and Path(part).is_file() and not Path(part).is_relative_to(output.parent) and part != str(binary)]
            argv = sandbox(argv, output.parent, home, binary, socket_path, readonly)
        command(argv, output.parent, config["genesis"]["max_cycle_minutes"] * 60, env)
    data = json.loads(output.read_text())
    if not isinstance(data.get("results"), list) or not data["results"]:
        raise ValueError("external benchmark requires nonempty results")
    for row in data["results"]:
        if not isinstance(row.get("task_id"), str) or type(row.get("passed")) is not bool:
            raise ValueError("external benchmark requires task_id:string / passed:bool")
    data["status"] = "ok"
    return data
