"""Regression gates for the twelve isolated code-task graders."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

_spec = importlib.util.spec_from_file_location('task_code', Path(__file__).with_name('task_code.py'))
m = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(m)


class CodeTaskTests(unittest.TestCase):
    def test_catalogue(self):
        self.assertEqual([f't{i:02d}' for i in range(1, 13)], [t['id'] for t in m.TASKS])

    def test_baselines_references_and_bad_code(self):
        for task in m.TASKS:
            with self.subTest(task=task['id']), tempfile.TemporaryDirectory() as tmp:
                d = Path(tmp)
                m.prepare(task['id'], d)
                protected = {p: (d / p).read_bytes() for p in task['protected']}
                ok, detail = m.verify(task['id'], d)
                self.assertFalse(ok, 'unfinished baseline passed: ' + detail)
                m.solve(task['id'], d)
                ok, detail = m.verify(task['id'], d)
                self.assertTrue(ok, detail)
                self.assertEqual(protected, {p: (d / p).read_bytes() for p in task['protected']})
                (d / task['output']).write_text('raise RuntimeError("not implemented")\n')
                ok, detail = m.verify(task['id'], d)
                self.assertFalse(ok, 'broken code passed: ' + detail)


    def test_behavioral_mutants(self):
        mutations = {
            't01': ("else (s[n//2-1]+s[n//2])/2", "else s[n//2]"),
            't02': ("(-item[1],item[0])", "(item[1],item[0])"),
            't03': ("ROUND_HALF_UP", "ROUND_HALF_EVEN"),
            't04': ("dt.astimezone(ZoneInfo('Asia/Shanghai')).date()", "dt.date()"),
            't05': ("unique.setdefault(event['id'],event)", "unique.__setitem__(event['id'],event)"),
            't06': ("result=deepcopy(defaults)", "result=defaults.copy()"),
            't07': ("[:max_len].rstrip('-')", "[:max_len]"),
            't08': ("mark='x' if row['done'] else ' '", "mark=' '"),
            't09': ("(1 if op=='in' else -1)", "1"),
            't10': ("customers.get(row['customer_id'],'')", "customers.get(row['customer_id'],'UNKNOWN')"),
            't11': ("self.entries.move_to_end(key)", "pass"),
            't12': ("key.lower() in MASKS", "key in MASKS"),
        }
        for task in m.TASKS:
            with self.subTest(task=task['id']), tempfile.TemporaryDirectory() as tmp:
                d = Path(tmp)
                m.prepare(task['id'], d)
                m.solve(task['id'], d)
                target = d / task['output']
                before = target.read_text()
                old, new = mutations[task['id']]
                self.assertIn(old, before)
                target.write_text(before.replace(old, new))
                ok, detail = m.verify(task['id'], d)
                self.assertFalse(ok, 'behavioral mutant survived: ' + detail)

    def test_redaction_rejects_leaked_fixture_secrets(self):
        task = next(t for t in m.TASKS if t['id'] == 't12')
        for secret in ['private@example.invalid', '13800000000', 'secret-key-1',
                       'pw-fixture-secret-2', 'nested-secret-3',
                       'Bearer private-auth-token-4']:
            with self.subTest(secret=secret), tempfile.TemporaryDirectory() as tmp:
                d = Path(tmp)
                m.prepare('t12', d)
                m.solve('t12', d)
                path = d / task['output']
                reference = path.read_text()
                path.write_text(reference.replace(
                    "if __name__=='__main__': sys.exit(main())",
                    "if __name__=='__main__':\n    print(" + repr(secret) + ")\n    sys.exit(main())"))
                ok, detail = m.verify('t12', d)
                self.assertFalse(ok, 'raw sensitive output survived: ' + detail)

if __name__ == '__main__':
    unittest.main()
