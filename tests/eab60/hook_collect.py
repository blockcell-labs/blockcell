#!/usr/bin/env python3
"""EAB-60 hook collector.

BlockCell hooks are shell commands whose template variables are substituted
(and shell-quoted) before execution; there is no stdin payload.  See
crates/agent/src/hooks.rs::HookManager::expand_command.  This script therefore
takes every observable field as argv.

argv: out_path event session_id tool_name file_path command result
"""
import json
import sys
import time


def norm(value):
    """A template token that had no value stays literal (e.g. "{tool_name}")."""
    if value is None:
        return None
    if len(value) >= 2 and value.startswith("{") and value.endswith("}"):
        return None
    return value


def main():
    if len(sys.argv) < 8:
        print("usage: hook_collect.py OUT EVENT SESSION TOOL FILE COMMAND RESULT", file=sys.stderr)
        return 2
    out, event, session, tool, file_path, command, result = sys.argv[1:8]
    record = {
        "ts": round(time.time(), 6),
        "event": norm(event),
        "session_id": norm(session),
        "tool_name": norm(tool),
        "file_path": norm(file_path),
        "command": norm(command),
        "result": norm(result),
        "pid": None,
    }
    line = json.dumps(record, ensure_ascii=False)
    with open(out, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
