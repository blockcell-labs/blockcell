import json
from pathlib import Path

from ..source import command
from ..isolation import build_sandbox, clean_env


def evaluate(worktree, config, parent_worktree=None):
    env = clean_env(config["_root"] + "/build-home", config["agent"]["key_env"])
    env["CARGO_TARGET_DIR"] = config["_root"] + "/target"
    outputs = []
    for argv in config["benchmarks"]["L0"]:
        if config["agent"]["require_bwrap"]:
            argv = build_sandbox(argv, Path(worktree), Path(env["CARGO_TARGET_DIR"]), Path(env["HOME"]), env)
        outputs.append(command(argv, worktree, config["build"]["timeout"], env))
    if config["benchmarks"].get("audit_command"):
        current, output = audit(worktree, config, env)
        previous, _ = audit(parent_worktree, config, env) if parent_worktree else (set(), "")
        if current - previous:
            raise RuntimeError("new cargo audit vulnerabilities: " + ", ".join(sorted(current - previous)))
        outputs.append(output)
    return "\n".join(outputs)


def audit(worktree, config, env):
    argv = config["benchmarks"]["audit_command"]
    if config["agent"]["require_bwrap"]:
        argv = build_sandbox(argv, Path(worktree), Path(env["CARGO_TARGET_DIR"]), Path(env["HOME"]), env)
    try:
        output = command(argv, worktree, config["build"]["timeout"], env)
    except RuntimeError as exc:
        output = str(exc).split("\n", 1)[-1]
    data = json.loads(output)
    if "vulnerabilities" not in data:
        raise RuntimeError("cargo audit returned no vulnerability report")
    vulnerabilities = data["vulnerabilities"].get("list", [])
    return {row["advisory"]["id"] for row in vulnerabilities}, output
