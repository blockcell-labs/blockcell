import json
import pathlib
import tempfile
import unittest
import task_misc as m

class MiscTasksTest(unittest.TestCase):
    def test_catalogue(self):
        self.assertEqual([t['id'] for t in m.TASKS], [f't{i:02}' for i in range(31,49)])
    def test_baseline_and_references(self):
        for t in m.TASKS:
            with self.subTest(tid=t['id']), tempfile.TemporaryDirectory() as tmp:
                d=pathlib.Path(tmp);m.prepare(t['id'],d)
                self.assertFalse(m.verify(t['id'],d)[0])
                m.solve(t['id'],d)
                ok,detail=m.verify(t['id'],d)
                self.assertTrue(ok,detail)
                if t.get('output'):
                    (d/t['output']).write_text('{}')
                    self.assertFalse(m.verify(t['id'],d)[0])

    def test_http_requires_real_requests(self):
        for i in range(41,49):
            tid=f't{i:02}'
            with self.subTest(tid=tid),tempfile.TemporaryDirectory() as tmp:
                d=pathlib.Path(tmp);m.prepare(tid,d)
                (d/'result.json').write_text(json.dumps(m._http_gold(tid)))
                with m.service(tid,d):
                    self.assertFalse(m.verify(tid,d)[0])
    def test_bank_requires_transfer_coverage(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=pathlib.Path(tmp);m.prepare('t33',d)
            source="import unittest\nfrom bank import Account, InsufficientFunds\nclass Tests(unittest.TestCase):\n"
            bodies=["self.assertEqual(Account('a',1).deposit(2),3)","self.assertEqual(Account('a',5).withdraw(2),3)","self.assertRaises(InsufficientFunds,Account('a',1).withdraw,2)","self.assertRaises(ValueError,Account('a').withdraw,0)","self.assertRaises(ValueError,Account('a').deposit,0)","self.assertRaises(ValueError,Account('a').deposit,-1)"]
            for i,b in enumerate(bodies):source+=f'    def test_{i}(self): {b}\n'
            (d/'test_bank.py').write_text(source)
            self.assertFalse(m.verify('t33',d)[0])
    def test_demo_token_must_not_be_written(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=pathlib.Path(tmp);m.prepare('t45',d)
            m.solve('t45',d)
            (d/'debug.log').write_text(m._TOKEN)
            self.assertFalse(m.verify('t45',d)[0])
    def test_refactor_accepts_module_and_alias_imports(self):
        for mode in ['module','alias']:
            with self.subTest(mode=mode),tempfile.TemporaryDirectory() as tmp:
                d=pathlib.Path(tmp);m.prepare('t34',d);m.solve('t34',d)
                for file,fun in [('invoice.py','invoice_total'),('checkout.py','checkout_total')]:
                    source=("import money as cash\n"+f"def {fun}(values): return cash.format_money(cash.sum_money(values))\n") if mode=='module' else ("from money import format_money as fmt, sum_money\n"+f"def {fun}(values): return fmt(sum_money(values))\n")
                    (d/file).write_text(source)
                ok,detail=m.verify('t34',d);self.assertTrue(ok,detail)
    def test_refactor_keeps_harmless_historical_comments(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=pathlib.Path(tmp);m.prepare('t32',d);m.solve('t32',d)
            p=d/'users/repo.py';p.write_text(p.read_text()+"# Previously get_usr\nnote='get_usr legacy spelling'\n")
            ok,detail=m.verify('t32',d);self.assertTrue(ok,detail)
    def test_retry_audit_rejects_fast_or_excessive_attempts(self):
        def row(path,status,t):return dict(path=path,status=status,method='GET',monotonic=t)
        m._AUDITS['t44']=[row('/orders?page=1',429,0),row('/orders?page=1',200,0.1),row('/orders?page=2',200,0.2)]
        self.assertFalse(m._audit_ok('t44')[0])
        m._AUDITS['t44'][1]['monotonic']=1.1
        self.assertTrue(m._audit_ok('t44')[0])
        m._AUDITS['t44'] += [row('/orders?page=1',200,2+i) for i in range(3)]
        self.assertFalse(m._audit_ok('t44')[0])
    def test_audit_rejects_writes_and_wrong_document_order(self):
        m._AUDITS['t41']=[dict(path='/repo',status=200,method='POST',monotonic=0)]
        self.assertFalse(m._audit_ok('t41')[0])
        paths=['/clients','/docs','/clients?cursor=c2']
        m._AUDITS['t45']=[dict(path=p,status=200,method='GET',monotonic=i) for i,p in enumerate(paths)]
        self.assertFalse(m._audit_ok('t45')[0])
    def test_toml_rejects_numeric_type_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=pathlib.Path(tmp);m.prepare('t37',d);m.solve('t37',d)
            p=d/'config.toml';p.write_text(p.read_text().replace('port = 8080','port = 8080.0'))
            self.assertFalse(m.verify('t37',d)[0])

if __name__=='__main__':unittest.main()
