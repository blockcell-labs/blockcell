import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from genesis import gates
from genesis.config import evaluation_hash, load
from genesis.isolation import BudgetExceeded
from genesis.source import command
from genesis.supervisor import Supervisor


def scorecard(*rows):
    return [{'status': 'ok', 'results': [dict(task_id=task, passed=passed, **metrics)
                                         for task, passed, metrics in rows]}]


class ReviewRegressions(unittest.TestCase):
    def test_persistent_safety_failure_blocks_promotion(self):
        config = load()
        config['benchmarks']['L3']['eab60']['required'] = False
        parent = {'coding_eval': scorecard(('security-check', False, {}), ('first', False, {}), ('second', False, {}))}
        candidate = {'coding_eval': scorecard(('security-check', False, {}), ('first', True, {}), ('second', True, {}))}
        self.assertEqual(gates.decide(parent, candidate, config)[0], 'REJECT')

    def test_successful_repair_does_not_count_as_same_outcome_overhead(self):
        parent = scorecard(('task', False, {}))
        candidate = scorecard(('task', True, {'duration_ms': 100, 'input_tokens': 25, 'tool_calls': 1}))
        self.assertEqual(gates.performance(parent, candidate, 100, 100, load()), (True, ''))

    def test_missing_metrics_and_measured_zero_are_distinct(self):
        candidate = scorecard(('task', True, {'tool_calls': 1}))
        self.assertTrue(gates.performance(scorecard(('task', True, {})), candidate, 100, 100, load())[0])
        self.assertFalse(gates.performance(scorecard(('task', True, {'tool_calls': 0})), candidate, 100, 100, load())[0])

    def test_eab_content_changes_invalidate_evaluation_hash(self):
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {'EAB60_KIT': temporary}):
            grader = Path(temporary) / 'grader.py'
            grader.write_text('first')
            config = load()
            before = evaluation_hash(config)
            grader.write_text('other')
            self.assertNotEqual(before, evaluation_hash(config))

    def test_command_obeys_cycle_deadline(self):
        from genesis.source import cycle_deadline
        token = cycle_deadline.set(time.monotonic() + 0.1)
        try:
            with self.assertRaises(BudgetExceeded):
                command([sys.executable, '-c', 'import time; time.sleep(10)'], Path.cwd())
        finally:
            cycle_deadline.reset(token)

    def test_unfit_fallback_cannot_enter_parent_evaluation(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = load()
            config['_root'] = temporary
            supervisor = Supervisor(config)
            genesis = {'agent_id': 'genesis-0', 'binary': 'genesis-bin'}
            selected = {'agent_id': 'selected', 'binary': 'selected-bin'}
            with patch.object(supervisor.archive, 'agent', return_value=genesis), \
                 patch.object(supervisor.selector, 'select', return_value=selected), \
                 patch.object(supervisor.adapter, 'help'), \
                 patch.object(supervisor.evaluator, 'selfcheck', return_value=False) as selfcheck, \
                 patch.object(supervisor.evaluator, 'full') as evaluate, \
                 patch.object(supervisor.archive, 'update'), \
                 patch('genesis.supervisor.report.write'):
                supervisor.run()
            evaluate.assert_not_called()
            self.assertEqual([call.args[0] for call in selfcheck.call_args_list], ['selected-bin', 'genesis-bin'])
            self.assertEqual(supervisor.archive.state()[0], 'STOPPED')
