import os
import signal
import shutil
import subprocess
import time
from contextvars import ContextVar
from pathlib import Path
from .config import expand
from .isolation import BudgetExceeded


cycle_deadline = ContextVar('cycle_deadline', default=None)


def remaining_cycle_time():
    deadline = cycle_deadline.get()
    return float('inf') if deadline is None else deadline - time.monotonic()


def command(argv, cwd, timeout=900, env=None, check=None, output_path=None):
    deadline = time.monotonic() + timeout
    cycle_limit = cycle_deadline.get()
    cycle_limited = cycle_limit is not None and cycle_limit <= deadline
    if cycle_limited:
        deadline = cycle_limit
    if cycle_limited and time.monotonic() >= deadline:
        raise BudgetExceeded('cycle time limit')
    proc = subprocess.Popen([str(x) for x in argv], cwd=cwd, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, start_new_session=True)
    try:
        while True:
            if check:
                check()
            try:
                remaining = max(0, deadline - time.monotonic())
                output, _ = proc.communicate(timeout=min(1, remaining) if check else remaining)
                break
            except subprocess.TimeoutExpired:
                if not check or time.monotonic() >= deadline:
                    raise
    except BaseException as exc:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            output, _ = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            output, _ = proc.communicate()
        if output_path:
            Path(output_path).write_text(output)
        if isinstance(exc, subprocess.TimeoutExpired):
            if cycle_limited:
                raise BudgetExceeded('cycle time limit') from exc
            raise TimeoutError(f"command timed out: {argv[0]}\n{output[-4000:]}") from exc
        raise
    if output_path:
        Path(output_path).write_text(output)
    if proc.returncode:
        raise RuntimeError(f"command failed ({proc.returncode}): {argv}\n{output[-8000:]}")
    return output


class Source:
    def __init__(self, config):
        self.config = config
        self.root = Path(config["_root"])
        self.repo = self.root / "repository"
        self.target = self.root / "target"

    def initialize(self):
        self.root.mkdir(parents=True, exist_ok=True)
        if not self.repo.exists():
            command(["git", "clone", "--no-hardlinks", self.config["genesis"]["repository"], self.repo], self.root, 300)
        sha = command(["git", "rev-parse", self.config["genesis"]["genesis0_sha"] + "^{commit}"], self.repo).strip()
        base = command(["git", "rev-parse", self.config["genesis"]["base_sha"] + "^{commit}"], self.repo).strip()
        if base != sha:
            raise RuntimeError("Genesis v0.1 requires base_sha == genesis0_sha")
        if not (self.root / "frozen").exists():
            command(["git", "worktree", "add", "--detach", self.root / "frozen", sha], self.repo)
        elif command(["git", "rev-parse", "HEAD"], self.root / "frozen").strip() != sha:
            raise RuntimeError("frozen SHA mismatch: use a new workspace_root")
        return sha

    def worktree(self, agent_id, sha):
        path = self.root / "worktrees" / agent_id
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            command(["git", "worktree", "add", "--detach", path, sha], self.repo)
        return path

    def build(self, agent_id, worktree):
        dest = self.root / "agents" / agent_id / "blockcell"
        dest.parent.mkdir(parents=True, exist_ok=True)
        from .isolation import build_sandbox, clean_env
        home = self.root / "build-home"
        home.mkdir(exist_ok=True)
        env = clean_env(home, self.config["agent"]["key_env"])
        env["CARGO_TARGET_DIR"] = str(self.target)
        env['CARGO_NET_OFFLINE'] = 'true'
        started = time.monotonic()
        argv = [expand(str(x), worktree=worktree, target=self.target) for x in self.config["build"]["command"]]
        if agent_id != "genesis-0" and self.config["agent"]["require_bwrap"]:
            argv = build_sandbox(argv, Path(worktree), self.target, home, env)
        try:
            output = command(argv, worktree, self.config["build"]["timeout"], env)
        except (RuntimeError, TimeoutError) as exc:
            (dest.parent / "build.log").write_text(str(exc))
            raise
        (dest.parent / "build.log").write_text(output)
        built = self.target / self.config["build"]["binary"]
        if not built.resolve().is_relative_to(self.target.resolve()) or not built.is_file():
            raise RuntimeError("build output escaped shared target or is not a regular file")
        shutil.copy2(built, dest)
        dest.chmod(0o755)
        return str(dest), time.monotonic() - started

    def diff(self, path):
        self.validate(path)
        # Include untracked files in the trusted index, not only tracked changes.
        command(["git", "add", "-N", "--force", "--", "."], path)
        names = command(["git", "diff", "--name-only", "HEAD"], path).splitlines()
        patch = command(["git", "diff", "--binary", "HEAD"], path)
        stats = command(["git", "diff", "--numstat", "HEAD"], path)
        lines = 0
        for row in stats.splitlines():
            added, deleted, _ = row.split("\t", 2)
            lines += int(added) + int(deleted) if added != "-" else 10**9
        return names, lines, patch

    def promote(self, agent_id, path):
        self.validate(path)
        # Detached worktrees and archive SHAs retain lineage without creating branches.
        return self.snapshot(agent_id, path)

    def snapshot(self, agent_id, path):
        self.validate(path)
        if command(["git", "status", "--porcelain"], path).strip():
            command(["git", "add", "--all", "--", "."], path)
            command(["git", "-c", "user.name=Genesis", "-c", "user.email=genesis@localhost", "commit", "-m", "Genesis " + agent_id], path)
        return command(["git", "rev-parse", "HEAD"], path).strip()

    def validate(self, path):
        path = Path(path)
        pointer = path / ".git"
        expected = self.repo / ".git/worktrees" / path.name
        if pointer.is_symlink() or not pointer.is_file():
            raise RuntimeError("candidate altered trusted worktree git pointer")
        value = pointer.read_text().strip()
        gitdir = Path(value.removeprefix('gitdir: '))
        if not gitdir.is_absolute():
            gitdir = path / gitdir
        if not value.startswith('gitdir: ') or gitdir.resolve() != expected.resolve():
            raise RuntimeError("candidate altered trusted worktree git pointer")
