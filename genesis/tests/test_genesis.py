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
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from genesis import diagnosis, gates
from genesis.archive import Archive
from genesis.agent_adapter import AgentAdapter
from genesis.adapters import bfcl, cargo, coding_eval, eab60
from genesis.config import load, evaluation_hash
from genesis.evaluator import Evaluator
from genesis.isolation import Meter, Proxy, ProviderUnavailable, build_sandbox, clean_env
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
    if fault=='highrisk': Path('crates/tools/src/sandbox.rs').write_text('pub fn changed() {}')
'''


class ConfigTests(unittest.TestCase):
    def test_openai_environment_overrides_change_evaluation_identity(self):
        with patch.dict(os.environ, {}, clear=True):
            original = load()
        with patch.dict(os.environ, {
            'OPENAI_BASE_URL': 'https://compatible.example/v1',
            'OPENAI_MODEL': 'test-model',
            'OPENAI_API_KEY': 'secret-not-for-config',
        }, clear=True):
            configured = load()
        self.assertEqual(configured['agent']['upstream'], 'https://compatible.example/v1')
        self.assertEqual(configured['agent']['model'], 'test-model')
        self.assertNotEqual(configured['_hash'], original['_hash'])
        self.assertNotIn('secret-not-for-config', json.dumps(configured))

    def test_empty_openai_overrides_preserve_configured_values(self):
        with patch.dict(os.environ, {}, clear=True):
            original = load()
        with patch.dict(os.environ, {'OPENAI_BASE_URL': '', 'OPENAI_MODEL': ''}, clear=True):
            configured = load()
        self.assertEqual(configured, original)


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
        build.write_text("import os,sys\nfrom pathlib import Path\nif Path('crates/broken').exists(): sys.exit(1)\nlevel=Path('crates/level.txt').read_text()\np=Path(os.environ['CARGO_TARGET_DIR'])/'release/blockcell'\np.parent.mkdir(parents=True,exist_ok=True)\np.write_text('#!'+sys.executable+'\\nVERSION='+level+'\\n'+"+repr(body)+")\np.chmod(0o755)\n")
        evaluate = self.base / 'eval.py'
        evaluate.write_text("import json,sys,subprocess\nfrom pathlib import Path\nv=int(subprocess.check_output([sys.argv[1],'--score']))\nfault=Path("+repr(str(self.fault))+").read_text()\nrows=[{'task_id':f't{i}','passed':i<2+v*2,'input_tokens':10,'output_tokens':10,'duration_ms':10,'tool_calls':1} for i in range(12)]\nif fault=='regression' and v: \n for r in rows: r['passed']=False\nPath(sys.argv[2]).write_text(json.dumps({'status':'ok','results':rows}))\n")
        self.config = load()
        self.config['promotion']['commit_enabled'] = True
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

    def test_patch_prompt_limits_and_format_stage(self):
        with patch.object(self.supervisor.adapter, 'call', wraps=self.supervisor.adapter.call) as calls, patch.object(cargo, 'format_changed', wraps=cargo.format_changed) as formatting:
            self.supervisor.run(1)
        prompts = [call.args[1] for call in calls.call_args_list if call.args[1].startswith('MODE: patch')]
        self.assertEqual(len(prompts), 2)
        self.assertTrue(all('8 files' in prompt and '400 added+deleted lines' in prompt and 'cargo check' in prompt and 'cargo test' in prompt for prompt in prompts))
        self.assertEqual(formatting.call_count, 2)

    def test_no_commit_policy_archives_without_writing_history(self):
        self.config['promotion']['commit_enabled']=False
        with patch.object(self.supervisor.source,'snapshot',side_effect=AssertionError('unexpected commit')), patch.object(self.supervisor.source,'promote',side_effect=AssertionError('unexpected promotion')):
            self.supervisor.run(1)
        self.assertEqual([a['status'] for a in self.candidates()],['ARCHIVE','ARCHIVE'])
        self.assertTrue(all('no-commit policy' in a['reason'] for a in self.candidates()))
        self.assertEqual(self.supervisor.selector.select(self.supervisor.archive.agents(),True)['agent_id'],'genesis-0')

    def test_legacy_placeholders_receive_real_hypotheses(self):
        for index in range(2):
            self.supervisor.archive.add(f'agent-0001-{index}','old','genesis-0',status='INCONCLUSIVE')
        self.supervisor.run(1)
        hypotheses=[json.loads(a['hypothesis_json']) for a in self.candidates()]
        self.assertEqual({h['id'] for h in hypotheses},{'a','b'})
        self.assertTrue(all(h['suspected_files']==['crates/level.txt'] for h in hypotheses))

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
        self.config['agent']['timeout']=1
        self.supervisor.run(1)
        self.assertTrue(all(a['status']=='REJECTED' and 'timed out' in a['reason'] for a in self.candidates()))

    def test_provider_failure_preserves_stage_for_resume(self):
        with patch.object(self.supervisor.evaluator,'full',side_effect=ProviderUnavailable('HTTP 402')):
            with self.assertRaises(ProviderUnavailable):
                self.supervisor.run(1)
        stage,payload=self.supervisor.archive.state()
        self.assertEqual(stage,'PARENT_EVAL')
        self.assertEqual(payload['blocked_reason'],'HTTP 402')
        self.assertEqual(self.candidates(),[])
        self.supervisor.run(resume=True)
        self.assertEqual(self.supervisor.archive.state()[0],'IDLE')
        self.assertTrue(all(a['status']=='PARENT' for a in self.candidates()))

    def test_highrisk_max_archive(self):
        self.fault.write_text('highrisk')
        self.supervisor.run(1)
        self.assertEqual([a['status'] for a in self.candidates()],['ARCHIVE']*2)

    def test_required_skipped_inconclusive(self):
        self.config['benchmarks']['L3']['eab60'].pop('command')
        self.config['benchmarks']['L3']['eab60']['kit']=''
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
        self.assertEqual(len(branches),0)
        self.assertEqual(len(self.candidates()),2)
        self.assertTrue(all(a['status']=='PARENT' for a in self.candidates()))
        self.supervisor.run(resume=True)
        self.assertEqual(len(self.candidates()),2)

    def test_parent_cache_rekeyed(self):
        self.supervisor.run(1)
        child=self.candidates()[0]
        cache_config = dict(self.config, _hash=evaluation_hash(self.config))
        result=self.supervisor.archive.cached(child,'coding_eval',0,cache_config)
        self.assertEqual(sum(t['passed'] for t in result['results']),4)

    def test_git_pointer_tampering_rejected(self):
        sha=self.supervisor.archive.agent('genesis-0')['commit_sha']
        work=self.supervisor.source.worktree('tampered',sha)
        (work/'.git').write_text('gitdir: /not-the-trusted-repository\n')
        with self.assertRaisesRegex(RuntimeError,'git pointer'):
            self.supervisor.source.diff(work)


class SmallTests(unittest.TestCase):
    def test_git_pointer_accepts_canonical_repository_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            real = root / 'real'
            real.mkdir()
            alias = root / 'alias'
            alias.symlink_to(real, target_is_directory=True)
            worktree = alias / 'worktrees' / 'candidate'
            worktree.mkdir(parents=True)
            gitdir = real / 'repository/.git/worktrees/candidate'
            gitdir.mkdir(parents=True)
            (worktree / '.git').write_text('gitdir: ' + str(gitdir.resolve()) + '\n')
            config = load()
            config['_root'] = str(alias)
            Source(config).validate(worktree)
            (worktree / '.git').write_text('gitdir: ' + str(root / 'untrusted') + '\n')
            with self.assertRaisesRegex(RuntimeError, 'git pointer'):
                Source(config).validate(worktree)

    @unittest.skipUnless(shutil.which('bwrap'), 'bubblewrap not installed')
    def test_bfcl_external_command_isolated(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            frozen=root/'frozen'; frozen.mkdir()
            output=root/'output/report.json'; output.parent.mkdir()
            binary=root/'binary'; binary.write_text('#!/bin/sh\necho fake-help\n'); binary.chmod(0o755)
            external=root/'external.py'
            external.write_text("import json,os,sys,subprocess\nfrom pathlib import Path\nassert 'OPENAI_API_KEY' not in os.environ\nsubprocess.check_call([sys.argv[1],'--help'])\nPath(sys.argv[2]).write_text(json.dumps({'results':[{'task_id':'bfcl-1','passed':True}]}))\n")
            config=load(); config['_root']=str(root)
            config['benchmarks']['L3']['bfcl'].update(enabled=True,command=[sys.executable,str(external),'{binary}','{output}'])
            with patch.dict(os.environ,{'OPENAI_API_KEY':'supervisor-only'}):
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
        prompt=adapter.call.call_args.args[1]
        self.assertIn('"verification": []',prompt)
        self.assertIn('"not_duplicate_of": []',prompt)
        self.assertIn('tests/eab60/',prompt)

    def test_negative_experience_excludes_empty_placeholders(self):
        with tempfile.TemporaryDirectory() as directory:
            archive=Archive(Path(directory))
            try:
                archive.add('p','sha')
                archive.add('placeholder','sha','p',status='INCONCLUSIVE')
                self.assertEqual(archive.failures('p'),[])
                archive.add('real','sha','p',{'id':'h'},status='REJECTED')
                self.assertEqual([f['hypothesis'] for f in archive.failures('p')],[{'id':'h'}])
            finally:
                archive.db.close()

    def test_eab_report_metrics_reasons_and_provider_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            (root/'reports').mkdir()
            evidence=root/'evidence/A01'
            evidence.mkdir(parents=True)
            records=[{'id':'A01','grade':{'status':'PARTIAL','checks':[{'name':'clarifies','ok':False,'detail':'no clarification'}]},'final_text':'reply','duration_s':1.5}]
            (root/'reports/results.json').write_text(json.dumps(records))
            events=[{'kind':'model/response','payload':{'usage':{'input_tokens':7,'output_tokens':3}}}]
            (evidence/'events.json').write_text(json.dumps(events))
            result=eab60.parse_report(root,['A01'])
            row=result['results'][0]
            self.assertEqual(result['status'],'ok')
            self.assertEqual((row['input_tokens'],row['output_tokens']),(7,3))
            self.assertIn('clarifies: no clarification',row['failure_reason'])
            self.assertEqual(row['agent_output_tail'],'reply')
            events.append({'kind':'model/request-error','payload':{'error':'HTTP_STATUS=402'}})
            (evidence/'events.json').write_text(json.dumps(events))
            result=eab60.parse_report(root,['A01'])
            self.assertEqual(result['status'],'inconclusive')
            self.assertIn('402',result['results'][0]['failure_reason'])
            for error in ('HTTP_STATUS=503', 'Stream header timeout after 60 seconds'):
                events[-1]['payload']['error']=error
                (evidence/'events.json').write_text(json.dumps(events))
                result=eab60.parse_report(root,['A01'])
                self.assertEqual(result['status'],'inconclusive')
                self.assertTrue(result['results'][0]['infrastructure_error'])
                self.assertIn(error,result['results'][0]['failure_reason'])
            events.append({'kind':'model/response','payload':{'usage':{}}})
            (evidence/'events.json').write_text(json.dumps(events))
            self.assertEqual(eab60.parse_report(root,['A01'])['status'],'ok')
            events[-2]['payload']['error']='HTTP_STATUS=402'
            (evidence/'events.json').write_text(json.dumps(events))
            self.assertEqual(eab60.parse_report(root,['A01'])['status'],'inconclusive')
            with self.assertRaisesRegex(ValueError,'missing requested'):
                eab60.parse_report(root,['A01','A02'])
            (root/'reports/results.json').write_text('[]')
            with self.assertRaisesRegex(ValueError,'missing requested'):
                eab60.parse_report(root,['A01'])

    def test_eab_retries_use_independent_runtime_roots(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            config=load();config['_root']=str(root);config['agent']['require_bwrap']=False
            config['benchmarks']['L3']['eab60'].update(kit=str(root),dev=['A01'])
            homes=[]
            def run(argv,cwd,timeout,env,check=None):
                homes.append(env['HOME'])
                self.assertNotIn('OPENAI_API_KEY',env)
                (cwd/'reports').mkdir()
                (cwd/'reports/results.json').write_text(json.dumps([{'id':'A01','grade':{'status':'PASS'}}]))
            with patch('genesis.adapters.eab60.command',side_effect=run):
                for _ in range(2):
                    self.assertEqual(eab60.evaluate(root,'unused',root/'evaluation',config,'dev',Meter(1))['status'],'ok')
            self.assertEqual(len(set(homes)),2)

    def test_diagnosis_excludes_infrastructure_failures(self):
        evaluator=Evaluator(load(),None,None)
        self.assertEqual(evaluator.evidence({}),[])
        results={'eab60_dev':[{'results':[
            {'task_id':'A01','passed':False,'infrastructure_error':True},
            {'task_id':'A02','passed':False,'failure_reason':'incorrect tool'},
            {'task_id':'A03','passed':True}]}]}
        evidence=evaluator.evidence(results)
        self.assertEqual([e['task_id'] for e in evidence],['eab60_dev/A02'])
        self.assertEqual(evidence[0]['failure_reason'],'incorrect tool')

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
            binary.write_text('#!/usr/bin/python3\nimport os,socket\nfrom pathlib import Path\nassert "OPENAI_API_KEY" not in os.environ\nassert not Path('+repr(str(sentinel))+').exists()\ns=socket.socket(); s.settimeout(.1)\ntry: s.connect(("1.1.1.1",80))\nexcept OSError: print("isolated")\nelse: raise AssertionError("network escaped")\n')
            binary.chmod(0o755)
            config=load(); config['_root']=str(root)
            config['agent']['command']=['{binary}']
            with patch.dict(os.environ,{'OPENAI_API_KEY':'supervisor-only'}):
                output=AgentAdapter(config,root/'calls',Meter(1)).call(str(binary),'',workspace)
            self.assertIn('isolated',output)

    @unittest.skipUnless(shutil.which('bwrap') and shutil.which('cargo'), 'Rust/bubblewrap not installed')
    def test_build_sandbox_preserves_rust_toolchain(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            env=clean_env(root/'home','OPENAI_API_KEY')
            argv=build_sandbox(['cargo','--version'],root,root/'target',root/'home',env)
            self.assertIn('cargo ',command(argv,root,10,env))

    @unittest.skipUnless(shutil.which('bwrap'), 'bubblewrap not installed')
    def test_agent_runtime_artifacts_are_archived_not_patched(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            binary=root/'fake'
            binary.write_text('#!/usr/bin/python3\nfrom pathlib import Path\nimport sqlite3,sys\nPath("logs").mkdir()\nPath("logs/agent.log").write_text("runtime")\nPath("memory").mkdir()\nc=sqlite3.connect("memory/memory.db"); c.execute("create table sample(value)"); c.commit(); c.close()\nPath("source.txt").write_text("changed")\nraise SystemExit(int(sys.argv[1]))\n')
            binary.chmod(0o755)
            config=load(); config['_root']=str(root)
            for code in (0,1):
                workspace=root/str(code); workspace.mkdir()
                (workspace/'agents').mkdir()
                (workspace/'agents/source.txt').write_text('keep')
                config['agent']['command']=['{binary}',str(code)]
                expected=contextlib.nullcontext() if not code else self.assertRaises(RuntimeError)
                with expected: AgentAdapter(config,root/'calls',Meter(1)).call(str(binary),'',workspace,'patch')
                self.assertEqual((workspace/'source.txt').read_text(),'changed')
                self.assertEqual((workspace/'agents/source.txt').read_text(),'keep')
                self.assertFalse((workspace/'memory/memory.db').exists())
                self.assertFalse((workspace/'logs').exists())
            self.assertEqual(len(list((root/'calls').glob('*/runtime/memory/memory.db'))),2)

    def test_provider_failure_is_not_parent_unfit(self):
        obj=object.__new__(Evaluator)
        obj.config=load()
        obj.adapter=unittest.mock.Mock()
        obj.adapter.call.side_effect=ProviderUnavailable('upstream HTTP 402')
        with tempfile.TemporaryDirectory() as directory:
            obj.config['_root']=directory
            obj.root=Path(directory)
            with self.assertRaises(ProviderUnavailable): obj.selfcheck('fake')

    def test_selector_explores_archive(self):
        config=load()
        config['promotion']['commit_enabled']=True
        config['selector']['exploit_probability']=0
        pool=[{'agent_id':'old','status':'PARENT','pass_count':100,'children_count':100000,'no_promote_rounds':0},
              {'agent_id':'explore','status':'ARCHIVE','parent_eligible':1,'pass_count':0,'children_count':0,'no_promote_rounds':0}]
        self.assertEqual(Selector(config,random.Random(1)).select(pool)['agent_id'],'explore')

    def test_bad_json_and_empty_hypotheses(self):
        for value in ('nonsense','[]','{"id": "bad"}'):
            with self.assertRaises(ValueError): diagnosis.extract(value)

    def test_similarity(self):
        h={'suspected_files':['crates/a.rs'],'proposed_change':'add retries'}
        self.assertTrue(diagnosis.similar(h,[{'hypothesis':h}],.8))
        self.assertFalse(diagnosis.similar(h,[],.8))

    def test_extract_skips_invalid_schema_echo(self):
        h={key:'value' for key in diagnosis.REQUIRED}
        for key in ('failure_ids','evidence','suspected_files','verification','not_duplicate_of'):
            h[key]=['value']
        for invalid in (dict(h,suspected_files=[]),dict(h,verification='not a list')):
            output='echoed prompt: '+json.dumps(invalid)+'\nFinal response: '+json.dumps([h,dict(h,id='second')])
            self.assertEqual(len(diagnosis.extract(output)),2)
        echoed=dict(h,id='old hypothesis')
        result=diagnosis.extract(json.dumps(echoed)+'\n'+json.dumps([dict(h,id='new A'),dict(h,id='new B')]))
        self.assertEqual([item['id'] for item in result],['new A','new B'])

    def test_clean_env_excludes_credentials(self):
        with patch.dict(os.environ,{'OPENAI_API_KEY':'never-in-child','OTHER_SECRET':'never-either'}):
            env=clean_env('/independent','OPENAI_API_KEY')
        self.assertNotIn('OPENAI_API_KEY',env)
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

    def test_proxy_retries_429_and_restricts_model(self):
        calls=[]
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                calls.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                self.send_response(429 if len(calls)<10 else 200)
                self.end_headers()
                self.wfile.write(b'<html>nginx rate limited</html>' if len(calls)<10 else json.dumps({'usage':{'prompt_tokens':7,'completion_tokens':3}}).encode())
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        try:
            meter=Meter(1)
            with Proxy(f'http://127.0.0.1:{server.server_port}/v1','test-only',meter,allowed_model='allowed') as proxy:
                def request(model):
                    return urllib.request.Request(proxy.url+'/chat/completions',data=json.dumps({'model':model}).encode())
                with self.assertRaises(urllib.error.HTTPError) as raised:
                    urllib.request.urlopen(request('forbidden'))
                self.assertEqual(raised.exception.code,403)
                self.assertEqual(calls,[])
                with patch('genesis.isolation.time.sleep') as sleep:
                    with urllib.request.urlopen(request('allowed')) as response:
                        self.assertIn(b'usage',response.read())
                self.assertGreaterEqual(sum(c.args[0] for c in sleep.call_args_list),150)
                self.assertEqual(len(calls),10)
                self.assertEqual((meter.input_tokens,meter.output_tokens),(7,3))
        finally:
            server.shutdown(); server.server_close(); thread.join()

    def test_proxy_stream_html_retry_and_exhaustion(self):
        calls={}
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                payload=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                scenario=payload['scenario']; calls[scenario]=calls.get(scenario,0)+1
                if scenario=='recover' and calls[scenario]>1:
                    self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
                    self.wfile.write(b'data: {"usage":{"prompt_tokens":7,"completion_tokens":3}}\n\ndata: [DONE]\n\n')
                else:
                    self.send_response(429); self.end_headers(); self.wfile.write(b'<html>nginx 429</html>')
                self.server.user_agent=self.headers.get('User-Agent')
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
        try:
            for scenario in ('recover','exhausted'):
                meter=Meter(1,retry_429_seconds=3)
                expected=contextlib.nullcontext() if scenario=='recover' else self.assertRaises(ProviderUnavailable)
                with expected, patch('genesis.isolation.time.sleep'):
                    with Proxy(f'http://127.0.0.1:{server.server_port}/v1','test-only',meter) as proxy:
                        req=urllib.request.Request(proxy.url+'/chat/completions',data=json.dumps({'stream':True,'scenario':scenario}).encode())
                        with urllib.request.urlopen(req) as response:
                            data=response.read()
                            self.assertEqual(response.status,200)
                        self.assertIn(b': upstream rate limited; retrying',data)
                        self.assertIn(b'[DONE]',data)
                self.assertEqual(meter.upstream_status,None if scenario=='recover' else 429)
                self.assertEqual((meter.input_tokens,meter.output_tokens),(7,3) if scenario=='recover' else (0,0))
            self.assertIn('Mozilla/5.0',server.user_agent)
        finally:
            server.shutdown(); server.server_close(); thread.join()

    def test_shared_proxy_concurrency_limit(self):
        active=peak=0
        lock=threading.Lock();ready=threading.Event()
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                nonlocal active,peak
                self.rfile.read(int(self.headers['Content-Length']))
                with lock:
                    active+=1;peak=max(peak,active)
                    if active==5: ready.set()
                ready.wait(3)
                self.send_response(200);self.end_headers()
                self.wfile.write(b'{"usage":{"prompt_tokens":7,"completion_tokens":3}}')
                with lock: active-=1
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            meter=Meter(1,concurrency=5)
            with Proxy(f'http://127.0.0.1:{server.server_port}/v1','test',meter) as first, Proxy(f'http://127.0.0.1:{server.server_port}/v1','test',meter) as second:
                def request(i):
                    url=(first if i%2 else second).url+'/chat/completions'
                    with urllib.request.urlopen(urllib.request.Request(url,data=b'{}')) as response: return response.read()
                with ThreadPoolExecutor(max_workers=10) as pool: list(pool.map(request,range(10)))
            self.assertEqual(peak,5)
            self.assertEqual(meter.peak_requests,5)
            self.assertEqual((meter.input_tokens,meter.output_tokens),(70,30))
            for invalid in (0,6,True):
                with self.assertRaises(ValueError): Meter(1,concurrency=invalid)
        finally:
            ready.set();server.shutdown();server.server_close();thread.join()

    def test_parallel_coding_tasks_are_isolated(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);config=load();config['agent']['concurrency']=5
            config['agent']['require_bwrap']=False  # This fixture tests the legacy runner command protocol.
            config['benchmarks']['L3']['coding_eval']['task_ids']=['t'+str(i) for i in range(5)]
            folders=[];ready=threading.Barrier(5)
            def run(argv,cwd,timeout,env,check=None):
                path=Path(argv[argv.index('--output')+1]);task=argv[argv.index('--task')+1]
                folders.append(path.parent);ready.wait(5)
                path.write_text(json.dumps({'results':[{'task_id':task,'passed':True}]}))
                return ''
            with patch('genesis.adapters.coding_eval.command',side_effect=run):
                result=coding_eval.evaluate(root,'binary',root/'report.json',config,root/'wrapper.py')
            self.assertEqual(len(result['results']),5)
            self.assertEqual(len(set(folders)),5)
            config['benchmarks']['L3']['coding_eval']['task_ids']=['t','t']
            with self.assertRaises(ValueError): coding_eval.evaluate(root,'binary',root/'report.json',config,root/'wrapper.py')

    def test_command_cancels_on_provider_failure(self):
        count=0
        def check():
            nonlocal count
            count+=1
            if count>1: raise ProviderUnavailable('HTTP 401')
        start=time.monotonic()
        with self.assertRaises(ProviderUnavailable): command([sys.executable,'-c','import time; time.sleep(30)'],Path.cwd(),check=check)
        self.assertLess(time.monotonic()-start,5)

    def test_proxy_disconnected_client_still_charged(self):
        import socket
        released=threading.Event()
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                self.rfile.read(int(self.headers['Content-Length']))
                released.wait(5)
                self.send_response(200)
                self.send_header('Content-Type','text/event-stream')
                self.end_headers()
                self.wfile.write(b'data: {"usage":{"prompt_tokens":7,"completion_tokens":3}}\n\ndata: [DONE]\n\n')
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        try:
            meter=Meter(1,on_update=lambda m: charged.set())
            charged=threading.Event()
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                with Proxy(f'http://127.0.0.1:{server.server_port}/v1','test-only',meter) as proxy:
                    client=socket.create_connection(('127.0.0.1',proxy.server.server_port))
                    client.sendall(b'POST /v1/chat/completions HTTP/1.0\r\nContent-Length: 15\r\n\r\n{"stream":true}')
                    client.close()
                    released.set()
                    self.assertTrue(charged.wait(5))
                self.assertEqual(errors.getvalue(),'')
            self.assertEqual((meter.input_tokens,meter.output_tokens),(7,3))
        finally:
            released.set()
            server.shutdown(); server.server_close(); thread.join()

    def test_proxy_payment_failure_stops_further_upstream_requests(self):
        calls=[]
        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                calls.append(1)
                self.rfile.read(int(self.headers['Content-Length']))
                self.send_error(402,'Payment Required')
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Upstream)
        thread=threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        try:
            meter=Meter(1)
            with self.assertRaises(ProviderUnavailable):
                with Proxy(f'http://127.0.0.1:{server.server_port}/v1','test-only',meter) as proxy:
                    for _ in range(2):
                        request=urllib.request.Request(proxy.url+'/chat/completions',data=b'{}')
                        with self.assertRaises(urllib.error.HTTPError) as raised:
                            urllib.request.urlopen(request)
                        self.assertEqual(raised.exception.code,402)
            self.assertEqual(calls,[1])
            self.assertEqual((meter.input_tokens,meter.output_tokens),(0,0))
        finally:
            server.shutdown(); server.server_close(); thread.join()


class Framework014Case(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = load()
        self.config['_root'] = str(self.root / 'state')
        (self.root / 'state').mkdir()
        self.config['agent']['require_bwrap'] = False
        self.config['benchmarks']['audit_command'] = []

    def score(self, coding, dev, holdout=None):
        report = lambda values: [{'status': 'ok', 'results': [dict(task_id=key, passed=value) for key, value in values.items()]}]
        return {'coding_eval': report(coding), 'eab60_dev': report(dev), 'eab60_holdout': report(holdout or {'H01': True})}

    def test_combined_promotion_and_archive(self):
        parent = self.score({'c1': False, 'c2': False}, {'d1': False, 'd2': False})
        for coding, dev, decision in [({'c1': True, 'c2': True}, {'d1': False, 'd2': False}, 'PROMOTE'),
                                      ({'c1': False, 'c2': False}, {'d1': True, 'd2': True}, 'PROMOTE'),
                                      ({'c1': True, 'c2': False}, {'d1': True, 'd2': False}, 'PROMOTE'),
                                      ({'c1': False, 'c2': False}, {'d1': True, 'd2': False}, 'ARCHIVE'),
                                      ({'c1': False, 'c2': False}, {'d1': False, 'd2': False}, 'INCONCLUSIVE')]:
            with self.subTest(coding=coding, dev=dev):
                candidate = self.score(coding, dev)
                self.assertEqual(gates.decide(parent, candidate, self.config)[0], decision)
        candidate = self.score({'c1': True, 'c2': True}, {'d1': True, 'd2': True})
        self.assertEqual(gates.decide(parent, candidate, self.config, selfcheck=False)[0], 'ARCHIVE')
        self.assertEqual(gates.decide(parent, candidate, self.config, performance_ok=False)[0], 'REJECT')

    def test_safety_holdout_and_non_safety_tradeoff(self):
        parent = self.score({'security_case': True, 'c1': False, 'c2': False, 'c3': False}, {'H02': True})
        candidate = self.score({'security_case': False, 'c1': True, 'c2': True, 'c3': True}, {'H02': True})
        self.assertIn('safety', gates.decide(parent, candidate, self.config)[1])
        candidate['coding_eval'][0]['results'][0]['passed'] = True
        candidate['eab60_dev'][0]['results'][0]['passed'] = False
        self.assertIn('safety', gates.decide(parent, candidate, self.config)[1])
        candidate['eab60_dev'][0]['results'][0]['passed'] = True
        candidate['eab60_holdout'][0]['results'][0]['passed'] = False
        self.assertIn('holdout', gates.decide(parent, candidate, self.config)[1])
        parent = self.score({'old': True, 'c1': False, 'c2': False}, {'d1': False})
        candidate = self.score({'old': False, 'c1': True, 'c2': True}, {'d1': True})
        self.assertEqual(gates.decide(parent, candidate, self.config)[0], 'PROMOTE')
        candidate['eab60_dev'][0]['results'] = []
        self.assertEqual(gates.decide(parent, candidate, self.config)[0], 'INCONCLUSIVE')

    def test_evaluation_hash_ignores_gates_not_provider_or_evaluation(self):
        original = evaluation_hash(self.config)
        self.config['genesis']['budget']['total_usd'] = 99
        self.config['benchmarks']['thresholds']['combined_net_fixed'] = 3
        self.config['benchmarks']['L0'] = []
        self.assertEqual(evaluation_hash(self.config), original)
        self.config['agent']['model'] = 'changed'
        self.assertNotEqual(evaluation_hash(self.config), original)
        self.config['agent']['model'] = load()['agent']['model']
        self.config['benchmarks']['L3']['coding_eval']['runs'] += 1
        self.assertNotEqual(evaluation_hash(self.config), original)

    def test_targeted_format_preserves_untouched_modules(self):
        if not shutil.which('rustfmt'):
            self.skipTest('rustfmt unavailable')
        worktree = self.root / 'work'
        worktree.mkdir()
        (worktree / 'lib.rs').write_text('mod untouched;\npub fn answer( )->u8{42}\n')
        untouched = 'pub fn untouched( )->u8{1}\n'
        (worktree / 'untouched.rs').write_text(untouched)
        with self.assertRaises(RuntimeError):
            cargo.format_changed(worktree, ['lib.rs'], self.config, check=True)
        cargo.format_changed(worktree, ['lib.rs', 'deleted.rs', 'notes.txt'], self.config)
        cargo.format_changed(worktree, ['lib.rs'], self.config, check=True)
        self.assertEqual((worktree / 'untouched.rs').read_text(), untouched)
        self.config['benchmarks']['L0'] = [['cargo', 'fmt', '--check']]
        def checked_command(argv, *args, **kwargs):
            return 'lib.rs\n' if argv[0] == 'git' else command(argv, *args, **kwargs)
        with patch.object(cargo, 'baseline', return_value=[{'passed': False}]), patch.object(cargo, 'command', side_effect=checked_command):
            cargo.evaluate(worktree, self.config, worktree)
            (worktree / 'lib.rs').write_text('fn malformed(\n')
            with self.assertRaises(RuntimeError):
                cargo.evaluate(worktree, self.config, worktree)
        self.assertEqual(cargo.format_changed(worktree, [], self.config), '')
        (self.root / 'outside.rs').write_text('fn hidden() {}')
        (worktree / 'escape.rs').symlink_to(self.root / 'outside.rs')
        with self.assertRaisesRegex(RuntimeError, 'escaped'):
            cargo.format_changed(worktree, ['escape.rs'], self.config)

    def test_parent_relative_clippy_rejects_new_diagnostics(self):
        worktree = self.root / 'work'
        worktree.mkdir()
        self.config['benchmarks']['L0'] = [['cargo', 'clippy', '--', '-D', 'warnings']]
        baseline = [{'passed': False, 'relaxed': True, 'relative': {'passed': True, 'diagnostics': {'existing': 2}}}]
        current = {'passed': True, 'diagnostics': {'existing': 2}, 'tail': ''}
        with patch.object(cargo, 'baseline', return_value=baseline), patch.object(cargo, 'command', return_value=''), patch.object(cargo, 'capture', return_value=current) as captured:
            cargo.evaluate(worktree, self.config, worktree)
            self.assertIn('--cap-lints=warn', captured.call_args.args[0])
            current['diagnostics']['new lint'] = 1
            with self.assertRaisesRegex(RuntimeError, 'new L0 failures'):
                cargo.evaluate(worktree, self.config, worktree)
            current.update(passed=False, diagnostics={}, tail='linker failed')
            with self.assertRaisesRegex(RuntimeError, 'new L0 failures'):
                cargo.evaluate(worktree, self.config, worktree)

    def test_diagnostic_counts_survive_line_moves(self):
        message = {'reason': 'compiler-message', 'message': {'level': 'error', 'code': {'code': 'clippy::lint'}, 'message': 'lint', 'spans': [dict(file_name=str(self.root / 'x.rs'), is_primary=True, text=[dict(text='x()', highlight_start=1, highlight_end=4)])]}}
        first = cargo.diagnostics(json.dumps(message), self.root)
        message['message']['level'] = 'warning'
        message['message']['spans'][0]['line_start'] = 100
        self.assertEqual(cargo.diagnostics(json.dumps(message), self.root), first)
        output = '\n'.join([json.dumps(message)] * 2)
        self.assertEqual(sum(cargo.diagnostics(output, self.root).values()), 2)

    def test_baseline_records_strict_and_complete_lints_then_caches(self):
        self.config['benchmarks']['L0'] = [['cargo', 'clippy', '--', '-D', 'warnings']]
        strict = {'passed': False, 'diagnostics': {'existing': 1}}
        relaxed = {'passed': True, 'diagnostics': {'existing': 1, 'downstream': 1}}
        with patch.object(cargo, 'command', return_value='sha\n'), patch.object(cargo, 'capture', side_effect=[strict, relaxed]) as capture:
            baseline = cargo.baseline(self.root, self.config)
            self.assertFalse(baseline[0]['passed'])
            self.assertTrue(baseline[0]['relative']['passed'])
            self.assertEqual(cargo.baseline(self.root, self.config), baseline)
            self.assertEqual(capture.call_count, 2)

    def test_audit_reads_full_failure_report_not_truncated_exception(self):
        data = {'vulnerabilities': {'list': [{'advisory': {'id': 'OLD', 'description': 'x' * 20000}}]}}
        def failed(argv, worktree, config, env, output_path=None):
            output_path.write_text(json.dumps(data))
            raise RuntimeError('command failed\ntruncated suffix')
        with patch.object(cargo, 'execute', side_effect=failed):
            ids, _ = cargo.audit(self.root, self.config, cargo.environment(self.config))
        self.assertEqual(ids, {'OLD'})

    def test_runtime_manifest_survives_interrupted_patch(self):
        workspace = self.root / 'work'
        workspace.mkdir()
        (workspace / 'logs').mkdir()
        (workspace / 'logs/original').write_text('preserve existing')
        self.config['agent']['command'] = [sys.executable, '-c', 'pass']
        adapter = AgentAdapter(self.config, self.root / 'calls', Meter(1))
        def interrupted(*args, **kwargs):
            (workspace / 'agents').mkdir()
            (workspace / 'agents/generated').write_text('generated')
            (workspace / 'crate/.blockcell').mkdir(parents=True)
            (workspace / 'crate/.blockcell/tasks.json').write_text('{}')
            raise RuntimeError('interrupted')
        with patch('genesis.agent_adapter.command', side_effect=interrupted), patch('genesis.agent_adapter.shutil.move', side_effect=OSError('simulate forced kill before cleanup')):
            with self.assertRaises(OSError):
                adapter.call(sys.executable, '', workspace, 'patch')
        adapter.call(sys.executable, '', workspace, 'patch')
        self.assertFalse((workspace / 'agents').exists())
        self.assertFalse((workspace / 'crate/.blockcell').exists())
        self.assertEqual((workspace / 'logs/original').read_text(), 'preserve existing')

    def test_cargo_runtime_preserves_source_and_archives_only_new_blockcell_dirs(self):
        workspace = self.root / 'work'
        (workspace / 'crate').mkdir(parents=True)
        (workspace / '.blockcell').mkdir()
        (workspace / '.blockcell/source').write_text('original')
        with self.assertRaisesRegex(RuntimeError, 'check failed'):
            with cargo.runtime(workspace, self.root / 'artifacts'):
                (workspace / 'crate/.blockcell').mkdir()
                (workspace / 'crate/.blockcell/tasks.json').write_text('{}')
                (workspace / 'crate/source.rs').write_text('fn answer() {}')
                raise RuntimeError('check failed')
        self.assertTrue((self.root / 'artifacts/crate/.blockcell/tasks.json').exists())
        self.assertTrue((workspace / 'crate/source.rs').exists())
        self.assertTrue((workspace / '.blockcell/source').exists())

    def test_patch_sandbox_offline_cargo_and_no_credentials(self):
        if not shutil.which('bwrap') or not shutil.which('cargo'):
            self.skipTest('bubblewrap/cargo unavailable')
        self.config['agent']['require_bwrap'] = True
        workspace = self.root / 'workspace'
        workspace.mkdir()
        (workspace / 'src').mkdir()
        (workspace / 'Cargo.toml').write_text('[package]\nname="sandbox-check"\nversion="0.1.0"\nedition="2021"\n')
        (workspace / 'src/lib.rs').write_text('#[test] fn works() { assert_eq!(2+2,4); }\n')
        host_cargo = self.root / 'host-cargo'
        host_cargo.mkdir()
        real_cargo = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
        for name in ('bin', 'registry'):
            if (real_cargo / name).exists():
                (host_cargo / name).symlink_to(real_cargo / name, target_is_directory=True)
        dependency = self.root / 'git-dependency'
        dependency.mkdir()
        (dependency / 'src').mkdir()
        (dependency / 'Cargo.toml').write_text('[package]\nname="offline-dependency"\nversion="0.1.0"\nedition="2021"\n')
        (dependency / 'src/lib.rs').write_text('pub fn answer() -> u8 { 42 }\n')
        command(['git', 'init', '--quiet'], dependency)
        command(['git', 'add', '--', 'Cargo.toml', 'src/lib.rs'], dependency)
        command(['git', '-c', 'user.name=Test', '-c', 'user.email=test@localhost', 'commit', '--quiet', '-m', 'dependency'], dependency)
        revision = command(['git', 'rev-parse', 'HEAD'], dependency).strip()
        with (workspace / 'Cargo.toml').open('a') as manifest:
            manifest.write(f'\n[dependencies]\noffline-dependency = {{git="{dependency.as_uri()}", rev="{revision}"}}\n')
        env = clean_env(self.root / 'fetch-home', 'OPENAI_API_KEY')
        env['CARGO_HOME'] = str(host_cargo)
        command(['cargo', 'fetch', '--manifest-path', str(workspace / 'Cargo.toml')], workspace, env=env)
        (host_cargo / 'credentials.toml').write_text('SHOULD_NOT_BE_VISIBLE')
        self.config['agent']['command'] = ['/bin/sh', '-c', 'test "$CARGO_NET_OFFLINE" = true && test -z "$OPENAI_API_KEY" && test ! -e "$CARGO_HOME/credentials.toml" && test ! -e /home/ubuntu/.openai_key && cargo check --offline && cargo test --offline']
        self.config['agent']['command'] = ['bwrap', '--ro-bind', '/', '/', '--dev', '/dev', '--proc', '/proc', '--unshare-net', '--bind', '/tmp', '/tmp', '--bind', str(workspace), str(workspace)] + self.config['agent']['command']
        meter = Meter(1)
        adapter = AgentAdapter(self.config, self.root / 'calls', meter)
        with patch.dict(os.environ, {'CARGO_HOME': str(host_cargo), 'OPENAI_API_KEY': 'fake-supervisor-only'}):
            output = adapter.call('/bin/true', 'MODE: patch', workspace, 'patch')
        self.assertIn('test result: ok.', output)
        self.assertTrue((self.root / 'state/target/debug').exists())
        self.assertFalse((workspace / 'target').exists())
        self.assertFalse((workspace / '.genesis-cargo').exists())


if __name__=='__main__': unittest.main()
