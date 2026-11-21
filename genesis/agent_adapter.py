import os
import json
import shutil
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
        before = (self.meter.input_tokens, self.meter.output_tokens, self.meter.cost)
        with Proxy(settings["upstream"], key, self.meter, socket_path, settings["model"]) as proxy:
            env = prepare_home(home, worktree, self.config, proxy.url)
            values = dict(binary=binary, prompt=prompt, workspace=worktree, task_id=task_id)
            argv = argv_override or [expand(part, **values) for part in settings["command"]]
            runtime = []
            if mode in ('diagnose', 'patch', 'review'):
                names = ['agents', 'logs', '.tool_results', 'skills/.skills_prompt_snapshot.json']
                for name in ('evo_workflow.db', 'skill_evolution_workflow.db', 'memory/memory.db', 'memory/knowledge_index.db'):
                    names.extend(name + suffix for suffix in ('', '-wal', '-shm', '-journal'))
                runtime = [Path(worktree) / name for name in names
                           if not (Path(worktree) / name).exists() and not (Path(worktree) / name).is_symlink()]
            if settings["require_bwrap"]:
                argv = sandbox(argv, worktree, home, binary, socket_path)
            try:
                output = command(argv, worktree, settings["timeout"], env, check=self.meter.check)
                (run / "output.log").write_text(output)
                events = []
                for path in (home / '.blockcell/session_events').glob('*.jsonl'):
                    events.extend(json.loads(line) for line in path.read_text().splitlines() if line.strip())
                self.last_metrics = {'task_id': task_id,
                                     'input_tokens': self.meter.input_tokens - before[0],
                                     'output_tokens': self.meter.output_tokens - before[1],
                                     'cost_usd': self.meter.cost - before[2],
                                     'tool_calls': sum(e.get('kind') == 'tool/call' for e in events),
                                     'tool_errors': sum(e.get('kind') == 'tool/result' and e.get('payload', {}).get('is_error', False) for e in events),
                                     'trace_path': str(home / '.blockcell/session_events')}
                (run / 'metrics.json').write_text(json.dumps(self.last_metrics))
                return output
            except (RuntimeError, TimeoutError) as exc:
                (run / "error.log").write_text(str(exc))
                raise
            finally:
                for path in runtime:
                    if not path.parent.resolve().is_relative_to(Path(worktree).resolve()):
                        continue
                    if path.exists() or path.is_symlink():
                        dest = run / 'runtime' / path.relative_to(worktree)
                        dest.parent.mkdir(parents=True, exist_ok=True)
                        shutil.move(str(path), dest)

    def help(self, binary, cwd):
        workspace = Path(self.config["_root"]) / "runs" / "help" / uuid.uuid4().hex
        workspace.mkdir(parents=True)
        return self.call(binary, "", workspace, "help", argv_override=[binary, "--help"])
