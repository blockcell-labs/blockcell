"""Trusted coding_eval subprocess entry point; prompts remain argv data."""
import json
import sys
from pathlib import Path

if __name__ == "__main__":
    context = json.loads(Path(sys.argv[1]).read_text())
    sys.path.insert(0, context["package_root"])
    from genesis.agent_adapter import AgentAdapter
    from genesis.isolation import Meter
    meter = Meter(**context["meter"])
    adapter = AgentAdapter(context["config"], context["run_root"], meter)
    print(adapter.call(context["binary"], sys.argv[2], Path(sys.argv[3]), "eval", sys.argv[4]))
