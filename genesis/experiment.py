"""Serial, metered real-model smoke checks using the Supervisor credentials."""
import argparse
import json
import time
from pathlib import Path

from .config import load
from .supervisor import Supervisor


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('stage', choices=['agent', 'coding', 'eab'])
    args = parser.parse_args()
    supervisor = Supervisor(load())
    agent = supervisor.archive.agent('genesis-0')
    start = time.monotonic()
    before = (supervisor.meter.input_tokens, supervisor.meter.output_tokens, supervisor.meter.cost)
    folder = supervisor.root / 'real_smoke'
    folder.mkdir(exist_ok=True)
    if args.stage == 'agent':
        workspace = folder / 'workspace'
        workspace.mkdir(exist_ok=True)
        result = {'output': supervisor.adapter.call(agent['binary'], 'Reply exactly GENESIS_REAL_SMOKE_OK. Do not call tools.', workspace, 'real_smoke')}
    elif args.stage == 'coding':
        supervisor.config['benchmarks']['L3']['coding_eval']['task_ids'] = ['bugfix-01-divide']
        result = supervisor.evaluator.evaluate(agent, 'coding_eval', 99)
    else:
        supervisor.config['benchmarks']['L3']['eab60']['dev'] = ['A01', 'B02', 'D05']
        result = supervisor.evaluator.evaluate(agent, 'eab60_dev', 99)
    summary = {'stage': args.stage, 'seconds': time.monotonic() - start,
               'input_tokens': supervisor.meter.input_tokens - before[0],
               'output_tokens': supervisor.meter.output_tokens - before[1],
               'cost_usd': supervisor.meter.cost - before[2], 'result': result}
    (folder / (args.stage + '.json')).write_text(json.dumps(summary, ensure_ascii=False, indent=2))
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
