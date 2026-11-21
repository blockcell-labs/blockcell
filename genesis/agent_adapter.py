import os
import uuid
from pathlib import Path

from .isolation import Proxy, prepare_home, sandbox
from .config import expand
from .source import command


class AgentAdapter:
    def __init__(self, config, run_root, meter):
        self.config = config
        self.run_root = Path(run_root)
        self.meter = meter

    def call(self, binary, prompt, worktree, mode="agent", task_id="", argv_override=None):
        self.meter.check()
        run = self.run_root / (mode + "-" + uuid.uuid4().hex)
        run.mkdir(parents=True)
        home = run / "home"
        settings = self.config["agent"]
        sockets = Path(self.config["_root"]) / "sockets"
        sockets.mkdir(exist_ok=True)
        socket_path = sockets / (uuid.uuid4().hex + ".sock") if settings["require_bwrap"] else None
        key = os.environ.get(settings["key_env"], "")
        with Proxy(settings["upstream"], key, self.meter, socket_path) as proxy:
            env = prepare_home(home, worktree, self.config, proxy.url)
            values = dict(binary=binary, prompt=prompt, workspace=worktree, task_id=task_id)
            argv = argv_override or [expand(part, **values) for part in settings["command"]]
            if settings["require_bwrap"]:
                argv = sandbox(argv, worktree, home, binary, socket_path)
            try:
                output = command(argv, worktree, settings["timeout"], env)
                (run / "output.log").write_text(output)
                return output
            except (RuntimeError, TimeoutError) as exc:
                (run / "error.log").write_text(str(exc))
                raise

    def help(self, binary, cwd):
        workspace = Path(self.config["_root"]) / "runs" / "help" / uuid.uuid4().hex
        workspace.mkdir(parents=True)
        return self.call(binary, "", workspace, "help", argv_override=[binary, "--help"])
