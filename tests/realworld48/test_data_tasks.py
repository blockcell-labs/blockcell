"""Contract and adversarial checks for the data/SQL/operations task group."""
import importlib.util
import pathlib
import tempfile
import unittest

HERE = pathlib.Path(__file__).parent


class DataTasksTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('task_data', HERE / 'task_data.py')
        cls.m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.m)

    def test_catalog(self):
        self.assertEqual([x['id'] for x in self.m.TASKS], [f't{x:02}' for x in range(13, 31)])

    def test_baseline_reference_and_wrong_answer(self):
        for task in self.m.TASKS:
            with self.subTest(task=task['id']), tempfile.TemporaryDirectory() as tmp:
                d = pathlib.Path(tmp)
                self.m.prepare(task['id'], d)
                self.assertFalse(self.m.verify(task['id'], d)[0], 'unfinished baseline passed')
                self.m.solve(task['id'], d)
                self.assertTrue(self.m.verify(task['id'], d)[0], self.m.verify(task['id'], d))
                (d / task['output']).write_text('{}', encoding='utf-8')
                self.assertFalse(self.m.verify(task['id'], d)[0], 'wrong output accepted')

    def test_migration_rejects_orders_schema_damage(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = pathlib.Path(tmp)
            self.m.prepare('t23', d)
            self.m.solve('t23', d)
            import sqlite3
            c = sqlite3.connect(d / 'legacy.db')
            c.execute('ALTER TABLE orders RENAME COLUMN total_cents TO lost_column')
            c.close()
            self.assertFalse(self.m.verify('t23', d)[0], 'damaged orders schema accepted')

    def test_migration_rejects_missing_transaction(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = pathlib.Path(tmp)
            self.m.prepare('t23', d)
            self.m.solve('t23', d)
            source = (d / 'migrate.py').read_text()
            (d / 'migrate.py').write_text(source.replace("c.execute('BEGIN IMMEDIATE')", "pass # no transaction"))
            self.assertFalse(self.m.verify('t23', d)[0], 'non-atomic migration accepted')

    def test_dry_run_refuses_mutated_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = pathlib.Path(tmp)
            self.m.prepare('t30', d)
            self.m.solve('t30', d)
            import os
            os.utime(d / 'tree/old.log', (0, 0))
            self.assertFalse(self.m.verify('t30', d)[0])

    def test_sql_hardcoding_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = pathlib.Path(tmp)
            self.m.prepare('t20', d)
            self.m.solve('t20', d)
            (d / 'query.sql').write_text("SELECT 4 customer_id,'陈' name UNION ALL SELECT 5,'Dana'")
            self.assertFalse(self.m.verify('t20', d)[0])

    def test_gold_exact_type_and_extra_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = pathlib.Path(tmp)
            self.m.prepare('t13', d)
            self.m.solve('t13', d)
            import json
            path = d / 'result.json'
            result = json.loads(path.read_text())
            result['extra'] = 1
            path.write_text(json.dumps(result))
            self.assertFalse(self.m.verify('t13', d)[0])


if __name__ == '__main__':
    unittest.main()
