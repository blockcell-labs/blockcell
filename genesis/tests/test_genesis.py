import contextlib
import http.server
import io
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest.mock import patch

from genesis import diagnosis, gates
from genesis.archive import Archive
from genesis.agent_adapter import AgentAdapter
from genesis.adapters import bfcl, cargo
from genesis.config import load
from genesis.isolation import Meter, Proxy, build_sandbox, clean_env
from genesis.selector import Selector
from genesis.source import Source, command
from genesis.supervisor import Supervisor


AGENT = '''import json,sys,time
from pathlib import Path
if sys.argv[1] == '--help':
    print('fake blockcell help'); sys.exit()
if sys.argv[1] == '--score':
    print(VERSION); sys.exit()
prompt = sys.argv[1]
with open(LOG,'a') as f: f.write(json.dumps({'version':VERSION,'prompt':prompt[:100]})+'\\n')
fault=Path(FAULT).read_text()
if 'MODE: selfcheck' in prompt:
    Path('subject.py').write_text('def answer(): return 42\\n')
elif 'MODE: diagnose' in prompt:
    parent=json.loads(prompt.split('\\n')[2].split('\\nPrior errors:')[0])['parent']
    h={'id':'h','lineage_parent':parent,'failure_ids':['coding_eval/t2'],'evidence':['failed assertion'],'suspected_files':['crates/level.txt'],'root_cause':'low level','proposed_change':'increase level','expected_effect':'+2','verification':['eval'],'risk':'low','not_duplicate_of':[]}
    print('Here is JSON: '+json.dumps([dict(h,id='a'),dict(h,id='b',proposed_change='alternate level increase')]))
elif 'MODE: patch' in prompt:
    if fault=='timeout': time.sleep(30)
    Path('crates/level.txt').write_text(str(VERSION+1))
    if fault=='compile': Path('crates/broken').write_text('broken')
    if fault=='protected': Path('.github/workflow').write_text('tampered')
    if fault=='tamper': Path('tests/coding_eval/eval.txt').write_text('tampered')
    if fault=='highrisk': Path('crates/tools/src/sandbox.rs').write_text('changed')
'''


class RepositoryCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.repo = self.base / 'repo'
        self.repo.mkdir()
        for name in ('crates/tools/src', '.github', 'tests/coding_eval', 'tests/eab60'):
            (self.repo / name).mkdir(parents=True, exist_ok=True)
        for name, value in {'crates/level.txt':'0', 'crates/tools/src/sandbox.rs':'original', '.github/workflow':'original', 'tests/coding_eval/eval.txt':'original'}.items():
            (self.repo / name).write_text(value)
        command(['git', 'init'], self.repo)
        command(['git', 'add', '--', 'crates', '.github', 'tests'], self.repo)
        command(['git','-c','user.name=Test','-c','user.email=test@localhost','commit','-m','base'], self.repo)
        sha = command(['git','rev-parse','HEAD'], self.repo).strip()
        self.fault = self.base / 'fault'
        self.fault.write_text('none')
        self.log = self.base / 'calls.jsonl'
        body = 'LOG='+repr(str(self.log))+'\nFAULT='+repr(str(self.fault))+'\n'+AGENT
        build = self.base / 'build.py'
        build.write_text("import os,sys\nfrom pathlib import Path\nif Path('crates/broken').exists(): sys.exit(1)\nlevel=Path('crates/level.txt').read_text()\np=Path(os.environ['CARGO_TARGET_DIR'])/'release/blockcell'\np.parent.mkdir(parents=True,exist_ok=True)\np.write_text('#!/usr/bin/python3\\nVERSION='+level+'\\n'+"+repr(body)+")\np.chmod(0o755)\n")
        evaluate = self.base / 'eval.py'
        evaluate.write_text("import json,sys,subprocess\nfrom pathlib import Path\nv=int(subprocess.check_output([sys.argv[1],'--score']))\nfault=Path("+repr(str(self.fault))+").read_text()\nrows=[{'task_id':f't{i}','passed':i<2+v*2,'input_tokens':10,'output_tokens':10,'duration_ms':10,'tool_calls':1} for i in range(12)]\nif fault=='regression' and v: \n for r in rows: r['passed']=False\nPath(sys.argv[2]).write_text(json.dumps({'status':'ok','results':rows}))\n")
        self.config = load()
        self.config['_root'] = str(self.base / 'state')
        self.config['genesis'].update(repository=str(self.repo), base_sha=sha, genesis0_sha=sha)
        self.config['agent'].update(command=['{binary}','{prompt}'], require_bwrap=False, timeout=3)
        self.config['build'].update(command=[sys.executable,str(build)], timeout=10)
        self.config['benchmarks']['L0'] = [[sys.executable,'-c','pass']]
        self.config['benchmarks']['audit_command'] = []
        for name in ('coding_eval','eab60'):
            self.config['benchmarks']['L3'][name]['command'] = [sys.executable,str(evaluate),'{binary}','{output}']
        self.config['selector'].update(exploit_probability=1,top_k=1)
        self.supervisor = Supervisor(self.config)
        self.addCleanup(self.supervisor.archive.db.close)
        with contextlib.redirect_stdout(io.StringIO()):
            self.supervisor.init()

    def candidates(self):
        return [a for a in self.supervisor.archive.agents() if a['parent_id']]

    def test_three_generations_use_new_parent_binary(self):
        self.supervisor.run(3)
        candidates = self.candidates()
        self.assertEqual(len(candidates),6)
        self.assertTrue(all(a['status']=='PARENT' for a in candidates),[(a['status'],a['reason']) for a in candidates])
        records=[json.loads(line) for line in self.log.read_text().splitlines()]
        versions=[r['version'] for r in records if r['prompt'].startswith('MODE: diagnose')]
        self.assertEqual(versions,[0,1,2])
        for a in candidates:
            self.assertTrue(Path(a['binary']).is_file())
        self.assertTrue((self.base/'state/frozen').is_dir())

    def test_compile_failure(self):
        self.fault.write_text('compile')
        self.supervisor.run(1)
        self.assertEqual([a['status'] for a in self.candidates()],['REJECTED']*2)
        self.assertTrue(all('L0' in a['reason'] for a in self.candidates()))

    def test_protected_path(self):
        self.fault.write_text('protected')
        self.supervisor.run(1)
        self.assertTrue(all(a['status']=='REJECTED' and 'protected path' in a['reason'] for a in self.candidates()))

    def test_tampered_frozen_evaluator(self):
        self.fault.write_text('tamper')
        self.supervisor.run(1)
        self.assertTrue(all(a['status']=='REJECTED' for a in self.candidates()))
        self.assertEqual((self.base/'state/frozen/tests/coding_eval/eval.txt').read_text(),'original')

    def test_regression(self):
        self.fault.write_text('regression')
        self.supervisor.run(1)
        self.assertTrue(all(a['status']=='REJECTED' for a in self.candidates()))

    def test_timeout(self):
        self.fault.write_text('timeout')
        self.config['agent']['timeout']=.1
        self.supervisor.run(1)
        self.assertTrue(all(a['status']=='REJECTED' and 'timed out' in a['reason'] for a in self.candidates()))

    def test_highrisk_max_archive(self):
        self.fault.write_text('highrisk')
        self.supervisor.run(1)
        self.assertEqual([a['status'] for a in self.candidates()],['ARCHIVE']*2)

    def test_required_skipped_inconclusive(self):
        self.config['benchmarks']['L3']['eab60'].pop('command')
        self.supervisor.run(1)
        self.assertEqual([a['status'] for a in self.candidates()],['INCONCLUSIVE']*2)

    def test_kill_resume_after_promotion_does_not_duplicate(self):
        config_path=self.base/'config.json'
        config_path.write_text(json.dumps(self.config))
        checkpoint=self.base/'committed'
        script=("import json,time\nfrom pathlib import Path\nfrom genesis.supervisor import Supervisor\n"
                "s=Supervisor(json.loads(Path("+repr(str(config_path))+").read_text()))\noriginal=s.source.promote\n"
                "def interrupt(*args):\n result=original(*args)\n Path("+repr(str(checkpoint))+").write_text(result)\n time.sleep(60)\n return result\n"
                "s.source.promote=interrupt\ns.run(1)\n")
        proc=subprocess.Popen([sys.executable,'-c',script],start_new_session=True)
        try:
            deadline=time.monotonic()+45
            while not checkpoint.exists() and time.monotonic()<deadline and proc.poll() is None:
                time.sleep(.02)
            self.assertTrue(checkpoint.exists(),'child did not reach promotion checkpoint')
        finally:
            if proc.poll() is None: os.killpg(proc.pid,signal.SIGKILL)
            proc.wait(timeout=10)
        self.assertEqual(self.supervisor.archive.state()[0],'PROMOTE')
        self.supervisor.run(resume=True)
        branches=command(['git','branch','--list','genesis/gen-*'],self.supervisor.source.repo).splitlines()
        self.assertEqual(len(branches),2)
        self.assertEqual(len(self.candidates()),2)
        self.assertTrue(all(a['status']=='PARENT' for a in self.candidates()))
        self.supervisor.run(resume=True)
        self.assertEqual(len(self.candidates()),2)

    def test_parent_cache_rekeyed(self):
        self.supervisor.run(1)
        child=self.candidates()[0]
        result=self.supervisor.archive.cached(child,'coding_eval',0,self.config)
        self.assertEqual(sum(t['passed'] for t in result['results']),4)

    def test_git_pointer_tampering_rejected(self):
        sha=self.supervisor.archive.agent('genesis-0')['commit_sha']
        work=self.supervisor.source.worktree('tampered',sha)
        (work/'.git').write_text('gitdir: /not-the-trusted-repository\n')
        with self.assertRaisesRegex(RuntimeError,'git pointer'):
            self.supervisor.source.diff(work)


class SmallTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which('bwrap'), 'bubblewrap not installed')
    def test_bfcl_external_command_isolated(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            frozen=root/'frozen'; frozen.mkdir()
            output=root/'output/report.json'; output.parent.mkdir()
            binary=root/'binary'; binary.write_text('#!/bin/sh\necho fake-help\n'); binary.chmod(0o755)
            external=root/'external.py'
            external.write_text("import json,os,sys,subprocess\nfrom pathlib import Path\nassert 'SF_KEY' not in os.environ\nsubprocess.check_call([sys.argv[1],'--help'])\nPath(sys.argv[2]).write_text(json.dumps({'results':[{'task_id':'bfcl-1','passed':True}]}))\n")
            config=load(); config['_root']=str(root)
            config['benchmarks']['L3']['bfcl'].update(enabled=True,command=[sys.executable,str(external),'{binary}','{output}'])
            with patch.dict(os.environ,{'SF_KEY':'supervisor-only'}):
                result=bfcl.evaluate(str(binary),output,frozen,config,Meter(1))
            self.assertTrue(result['results'][0]['passed'])

    def test_bfcl_empty_and_error_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            output=root/'report.json'
            config=load(); config['_root']=str(root); config['agent']['require_bwrap']=False
            options=config['benchmarks']['L3']['bfcl']
            options.update(enabled=True,command=[sys.executable,'-c',"from pathlib import Path; Path("+repr(str(output))+").write_text('{\"results\":[]}')"])
            with self.assertRaisesRegex(ValueError,'nonempty results'):
                bfcl.evaluate('/unused',output,root,config,Meter(1))
            options['command']=[sys.executable,'-c','raise SystemExit(2)']
            with self.assertRaisesRegex(RuntimeError,'command failed'):
                bfcl.evaluate('/unused',output,root,config,Meter(1))

    def test_diagnosis_retries_bounded(self):
        from unittest.mock import Mock
        adapter=Mock(); adapter.call.return_value='not JSON'
        result,errors=diagnosis.diagnose(adapter,{'agent_id':'p','binary':'fake'},Path('.'),[],[],load())
        self.assertEqual(result,[])
        self.assertEqual(len(errors),3)
        self.assertEqual(adapter.call.call_count,3)

    def test_audit_compares_parent_and_fails_closed(self):
        config=load(); config['agent']['require_bwrap']=False
        config['benchmarks']['L0']=[]
        existing=json.dumps({'vulnerabilities':{'list':[{'advisory':{'id':'OLD'}}]}})
        added=json.dumps({'vulnerabilities':{'list':[{'advisory':{'id':'OLD'}},{'advisory':{'id':'NEW'}}]}})
        with patch('genesis.adapters.cargo.command',side_effect=[existing,existing]):
            self.assertIn('OLD',cargo.evaluate(Path('.'),config,Path('.')))
        with patch('genesis.adapters.cargo.command',side_effect=[added,existing]):
            with self.assertRaisesRegex(RuntimeError,'NEW'): cargo.evaluate(Path('.'),config,Path('.'))
        with patch('genesis.adapters.cargo.command',return_value='{}'):
            with self.assertRaisesRegex(RuntimeError,'no vulnerability report'): cargo.evaluate(Path('.'),config,Path('.'))

    def test_build_output_cannot_follow_host_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            config=load(); config['_root']=str(root/'state')
            config['agent']['require_bwrap']=False
            config['build']['command']=[sys.executable,'-c',"import os; from pathlib import Path; p=Path(os.environ['CARGO_TARGET_DIR'])/'release/blockcell'; p.parent.mkdir(parents=True); p.symlink_to('/proc/self/environ')"]
            with self.assertRaisesRegex(RuntimeError,'escaped shared target'):
                Source(config).build('candidate',root)

    def test_holdout_not_in_diagnosis_evidence(self):
        from genesis.evaluator import Evaluator
        obj=object.__new__(Evaluator)
        failed={'status':'ok','results':[{'task_id':'secret','passed':False,'agent_output_tail':'holdout secret'}]}
        self.assertEqual(obj.evidence({'eab60_holdout':[failed]}),[])

    @unittest.skipUnless(shutil.which('bwrap'), 'bubblewrap not installed')
    def test_sandbox_blocks_host_files_network_and_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            workspace=root/'workspace'; workspace.mkdir()
            sentinel=root/'host-secret'; sentinel.write_text('not mounted')
            binary=root/'fake'
            binary.write_text('#!/usr/bin/python3\nimport os,socket\nfrom pathlib import Path\nassert "SF_KEY" not in os.environ\nassert not Path('+repr(str(sentinel))+').exists()\ns=socket.socket(); s.settimeout(.1)\ntry: s.connect(("1.1.1.1",80))\nexcept OSError: print("isolated")\nelse: raise AssertionError("network escaped")\n')
            binary.chmod(0o755)
            config=load(); config['_root']=str(root)
            config['agent']['command']=['{binary}']
            with patch.dict(os.environ,{'SF_KEY':'supervisor-only'}):
                output=AgentAdapter(config,root/'calls',Meter(1)).call(str(binary),'',workspace)
            self.assertIn('isolated',output)

    @unittest.skipUnless(shutil.which('bwrap') and shutil.which('cargo'), 'Rust/bubblewrap not installed')
    def test_build_sandbox_preserves_rust_toolchain(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            env=clean_env(root/'home','SF_KEY')
            argv=build_sandbox(['cargo','--version'],root,root/'target',root/'home',env)
            self.assertIn('cargo ',command(argv,root,10,env))

    def test_selector_explores_archive(self):
        config=load()
        config['selector']['exploit_probability']=0
        pool=[{'agent_id':'old','status':'PARENT','pass_count':100,'children_count':100000,'no_promote_rounds':0},
              {'agent_id':'explore','status':'ARCHIVE','pass_count':0,'children_count':0,'no_promote_rounds':0}]
        self.assertEqual(Selector(config,random.Random(1)).select(pool)['agent_id'],'explore')

    def test_bad_json_and_empty_hypotheses(self):
        for value in ('nonsense','[]','{"id": "bad"}'):
            with self.assertRaises(ValueError): diagnosis.extract(value)

    def test_similarity(self):
        h={'suspected_files':['crates/a.rs'],'proposed_change':'add retries'}
        self.assertTrue(diagnosis.similar(h,[{'hypothesis':h}],.8))
        self.assertFalse(diagnosis.similar(h,[],.8))

    def test_clean_env_excludes_credentials(self):
        with patch.dict(os.environ,{'SF_KEY':'never-in-child','OTHER_SECRET':'never-either'}):
            env=clean_env('/independent','SF_KEY')
        self.assertNotIn('SF_KEY',env)
        self.assertNotIn('OTHER_SECRET',env)
        self.assertEqual(env['HOME'],'/independent')

    def test_patch_limits_and_allowlist(self):
        config=load()
        self.assertTrue(gates.patch_gate(['crates/a.rs'],401,'',config)[0])
        self.assertTrue(gates.patch_gate(['README.md'],1,'',config)[0])
        self.assertTrue(gates.patch_gate(['Cargo.lock'],1,'+source = "evil"',config)[0])

    def test_paired_mismatch(self):
        self.assertIsNone(gates.paired([],[]))
        p=[{'status':'ok','results':[{'task_id':'a','passed':True}]}]
        c=[{'status':'ok','results':[{'task_id':'b','passed':True}]}]
        self.assertIsNone(gates.paired(p,c))

    def test_proxy_json_sse_meter_and_budget(self):
        received=[]
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                received.append(self.headers.get('Authorization'))
                payload=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                usage={'prompt_tokens':7,'completion_tokens':3}
                self.send_response(200)
                self.send_header('Content-Type','text/event-stream' if payload.get('stream') else 'application/json')
                self.end_headers()
                self.wfile.write((('data: '+json.dumps({'usage':usage})+'\n\ndata: [DONE]\n\n') if payload.get('stream') else json.dumps({'usage':usage})).encode())
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        try:
            meter=Meter(.00002,input_rate=1,output_rate=1)
            with Proxy(f'http://127.0.0.1:{server.server_port}/v1','upstream-only',meter) as proxy:
                for stream in (False,True):
                    req=urllib.request.Request(proxy.url+'/chat/completions',data=json.dumps({'stream':stream}).encode(),headers={'Content-Type':'application/json'})
                    with urllib.request.urlopen(req) as response: data=response.read()
                    self.assertIn(b'usage',data)
                self.assertEqual((meter.input_tokens,meter.output_tokens),(14,6))
                self.assertAlmostEqual(meter.cost,.00002)
                with self.assertRaises(urllib.error.HTTPError) as raised:
                    urllib.request.urlopen(req)
                self.assertEqual(raised.exception.code,429)
            self.assertEqual(received,['Bearer upstream-only']*2)
        finally:
            server.shutdown(); server.server_close(); thread.join()


if __name__=='__main__': unittest.main()
