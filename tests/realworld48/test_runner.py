"""Infrastructure tests use stdlib only; no model calls."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
import subprocess

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))


class RunnerTests(unittest.TestCase):
    def load(self):
        self.assertTrue((ROOT / 'runner.py').exists(), 'runner is not implemented')
        import runner
        return runner

    def test_unknown_metrics_are_null(self):
        r = self.load()
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(r.load_metrics(Path(td))['input_tokens'])
            (Path(td) / '.eval-metrics.json').write_text('{"input_tokens": 0, "output_tokens": -4}')
            m = r.load_metrics(Path(td))
            self.assertEqual(m['input_tokens'], 0)
            self.assertIsNone(m['output_tokens'])

    def test_command_arguments_never_shell_expanded(self):
        r = self.load()
        prompt = '中文 "a b" $(touch /tmp/do-not-create-realworld48) {nested}'
        args = r.command_argv('python3 wrapper.py "{prompt}" "{workspace}"', prompt, Path('/tmp/a b'), 't01')
        self.assertEqual(args, ['python3', 'wrapper.py', prompt, '/tmp/a b'])

    def test_summary_preserves_not_run_and_assisted(self):
        r = self.load()
        tasks = [dict(id='t01', category='code', difficulty='easy', title='one'),
                 dict(id='t02', category='data', difficulty='hard', title='two')]
        records = [dict(id='t01', status='fail', assisted=False, metrics={}),
                   dict(id='t01', status='pass', assisted=True, metrics={})]
        s = r.summarize(tasks, records)
        self.assertEqual(s['total'], 2)
        self.assertEqual(s['independent_passed'], 0)
        self.assertEqual(s['not_run'], 1)
        self.assertEqual(s['assisted_passed'], 1)
        self.assertIsNone(s['input_tokens'])

    def test_hashes_do_not_follow_symlinks(self):
        r = self.load()
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            (d / 'target').write_text('x')
            (d / 'link').symlink_to('target')
            h = r.file_hash(d / 'link')
            (d / 'target').write_text('y')
            self.assertEqual(h, r.file_hash(d / 'link'))
            (d / 'link').unlink()
            (d / 'link').symlink_to('/tmp/elsewhere')
            self.assertNotEqual(h, r.file_hash(d / 'link'))

    def test_redaction(self):
        r = self.load()
        text = r.redact('Bearer abc-secret key=abc-secret', ['abc-secret'])
        self.assertNotIn('abc-secret', text)

    def test_timeout_kills_child(self):
        r = self.load()
        with tempfile.TemporaryDirectory() as td:
            result = r.execute([sys.executable, '-c', 'import time; time.sleep(30)'], Path(td), dict(os.environ), .05)
            self.assertTrue(result['timed_out'])

    def test_protected_input_mutation_is_failure(self):
        r = self.load()
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            (d / 'input.csv').write_text('old')
            expected = {'input.csv': r.file_hash(d / 'input.csv')}
            self.assertEqual(r.changed_protected(d, expected), [])
            (d / 'input.csv').write_text('new')
            self.assertEqual(r.changed_protected(d, expected), ['input.csv'])

    def test_malformed_checkpoint_rejected(self):
        r = self.load()
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / 'attempts.jsonl'
            p.write_text('{"id":"t01"}\ntruncated')
            with self.assertRaises(ValueError):
                r.read_records(p)


class AdditionalRunnerTests(unittest.TestCase):
    def test_corrupt_metrics_are_ignored(self):
        import runner as r
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            for text in ('[]', 'null', '{bad', '{"input_tokens":true,"output_tokens":NaN}'):
                (d / '.eval-metrics.json').write_text(text)
                self.assertEqual(r.load_metrics(d), dict.fromkeys(r.METRIC_FIELDS))

    def test_manifest_has_48_balanced_tasks(self):
        import runner as r
        if not all((ROOT / (m + '.py')).exists() for m in r.MODULE_NAMES):
            self.skipTest('task modules still being prepared')
        from collections import Counter
        tasks = r.catalog()
        self.assertEqual([t['id'] for t in tasks], [f't{i:02d}' for i in range(1,49)])
        self.assertEqual(Counter(t['difficulty'] for t in tasks), dict(easy=16, medium=16, hard=16))

    def test_failed_agent_can_report_valid_artifact_without_passing(self):
        from contextlib import redirect_stdout
        import io
        from types import SimpleNamespace
        from unittest.mock import patch
        import runner as r
        tasks = r.catalog()
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            wrapper = d / 'fake_agent.py'
            source = '''def mean(xs):
    if not xs: raise ValueError()
    return sum(xs)/len(xs)
def median(xs):
    if not xs: raise ValueError()
    s=sorted(xs);n=len(xs)
    return s[n//2] if n%2 else (s[n//2-1]+s[n//2])/2
def variance(xs):
    m=mean(xs)
    return sum((x-m)**2 for x in xs)/len(xs)
'''
            wrapper.write_text('from pathlib import Path\nimport os,sys\n'
                               + 'Path("stats.py").write_text(' + repr(source) + ')\n'
                               + 'print("Bearer fake-test-token")\n'
                               + 'sys.exit(int(os.environ.get("R48_TEST_EXIT", "0")))\n')
            args = SimpleNamespace(run_dir=str(d / 'run'), agent_command=f'{sys.executable} {wrapper}',
                                   model_label='fake-no-api', timeout=10, task=['t01'], resume=False,
                                   assisted=False, intervention_note=None, max_wall_seconds=None, max_known_tokens=None)
            with patch.dict(os.environ, {'R48_TEST_EXIT': '1'}), redirect_stdout(io.StringIO()):
                r.run_agent(args, tasks)
            summary = json.loads((d / 'run/summary.json').read_text())
            self.assertEqual(summary['status_counts'], {'fail': 1, 'not_run': 47})
            self.assertEqual(summary['independent_passed'], 0)
            self.assertIsNone(summary['input_tokens'])
            self.assertNotIn('fake-test-token', (d / 'run/attempts.jsonl').read_text())
            args.resume = True
            with redirect_stdout(io.StringIO()):
                r.run_agent(args, tasks)
            self.assertEqual(len(r.read_records(d / 'run/attempts.jsonl')), 1)
            args.assisted = True
            args.intervention_note = 'fake transport repair'
            with patch.dict(os.environ, {'R48_TEST_EXIT': '0'}), redirect_stdout(io.StringIO()):
                r.run_agent(args, tasks)
            summary = json.loads((d / 'run/summary.json').read_text())
            self.assertEqual(summary['independent_passed'], 0)
            self.assertEqual(summary['assisted_passed'], 1)
            self.assertEqual(summary['attempt_count'], 2)
            self.assertTrue((d / 'run/workspaces/t01/attempt-001/stats.py').is_file())
            self.assertTrue((d / 'run/workspaces/t01/attempt-002/stats.py').is_file())

    def test_interrupted_attempt_recovered_once(self):
        import runner as r
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            active = dict(id='t01', attempt_id='t01-001', assisted=False, metrics={})
            r.write_json(d / 'active.json', active)
            records = r.recover_interrupted(d)
            self.assertEqual(records[0]['status'], 'interrupted')
            self.assertFalse((d / 'active.json').exists())
            self.assertEqual(len(r.recover_interrupted(d)), 1)

    def test_wrapper_content_is_fingerprinted(self):
        import runner as r
        with tempfile.TemporaryDirectory() as td:
            wrapper = Path(td) / 'wrapper.py'
            wrapper.write_text('print(1)')
            before = r.command_fingerprint(f'{sys.executable} {wrapper}')
            wrapper.write_text('print(2)')
            self.assertNotEqual(before, r.command_fingerprint(f'{sys.executable} {wrapper}'))

    def test_optimized_mode_is_rejected(self):
        p = subprocess.run([sys.executable, '-O', str(ROOT / 'runner.py'), 'list'], capture_output=True, text=True)
        self.assertNotEqual(p.returncode, 0)
        self.assertIn('optim', p.stderr.lower())

    def test_nan_timeout_is_rejected_before_workspace_created(self):
        with tempfile.TemporaryDirectory() as td:
            dest = Path(td) / 'run'
            p = subprocess.run([sys.executable, str(ROOT / 'runner.py'), 'run', '--run-dir', str(dest),
                                '--agent-command', sys.executable, '--timeout', 'nan', '--task', 't01'],
                               capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn('finite', p.stderr)
            self.assertFalse(dest.exists())

    def test_http_agent_subprocess_end_to_end(self):
        from contextlib import redirect_stdout
        import io
        from types import SimpleNamespace
        import runner as r
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            wrapper = d / 'http_agent.py'
            wrapper.write_text('''import os,json
from pathlib import Path
from http.client import HTTPConnection
from urllib.parse import urlsplit
from collections import Counter
url=urlsplit(os.environ['BLOCKCELL_EVAL_ENDPOINT'])
p='/tickets?page=1';rows={};calls=0
while p:
    c=HTTPConnection(url.hostname,url.port,timeout=5)
    c.request('GET',p);r=c.getresponse();assert r.status==200
    data=json.loads(r.read());c.close();calls+=1
    for row in data['items']: rows.setdefault(row['id'],row)
    p=data['next']
items=sorted(rows.values(),key=lambda x:x['id'])
Path('result.json').write_text(json.dumps({'tickets':items,'counts':dict(Counter(x['status'] for x in items))}))
Path('.eval-metrics.json').write_text(json.dumps({'input_tokens':17,'output_tokens':4,'tool_calls':calls}))
''')
            args = SimpleNamespace(run_dir=str(d / 'run'), agent_command=f'{sys.executable} {wrapper}',
                                   model_label='fake-http-no-model', timeout=10, task=['t43'], resume=False,
                                   assisted=False, intervention_note=None, max_wall_seconds=None, max_known_tokens=None)
            with redirect_stdout(io.StringIO()):
                r.run_agent(args, r.catalog())
            s = json.loads((d / 'run/summary.json').read_text())
            self.assertEqual(s['independent_passed'], 1)
            self.assertEqual(s['not_run'], 47)
            self.assertEqual(s['input_tokens'], 17)
            self.assertEqual(s['tool_calls'], 3)

    def test_next_attempt_skips_orphan_directories(self):
        import runner as r
        with tempfile.TemporaryDirectory() as td:
            base = Path(td) / 'workspaces/t01'
            (base / 'attempt-001').mkdir(parents=True)
            (base / 'attempt-002').mkdir()
            self.assertEqual(r.next_attempt(Path(td), 't01', 1), 3)


if __name__ == '__main__':
    unittest.main()
