import argparse
import json
import sys

from .config import load
from .report import write
from .supervisor import Supervisor


def main():
    parser = argparse.ArgumentParser(description="Blockcell Genesis v0.1")
    parser.add_argument("--config", help="genesis.json path")
    sub = parser.add_subparsers(dest="action", required=True)
    for name in ("init", "baseline", "resume", "status", "report"):
        sub.add_parser(name)
    run = sub.add_parser("run")
    run.add_argument("--cycles", type=int, default=1)
    args = parser.parse_args()
    if args.action == "run" and args.cycles < 1:
        parser.error("--cycles must be positive")
    supervisor = Supervisor(load(args.config))
    try:
        if args.action == "init":
            supervisor.init()
        elif args.action == "baseline":
            print(json.dumps(supervisor.baseline(), ensure_ascii=False, indent=2))
        elif args.action in ("run", "resume"):
            print(supervisor.run(getattr(args, "cycles", 1), resume=args.action == "resume"))
        elif args.action == "status":
            print(json.dumps({"state": supervisor.archive.state(), "agents": supervisor.archive.agents()}, ensure_ascii=False, indent=2))
        else:
            print(write(supervisor.archive, supervisor.root))
        return 0
    except (RuntimeError, TimeoutError, ValueError, OSError) as exc:
        print("Genesis error: " + str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
