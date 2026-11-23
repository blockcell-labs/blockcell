"""Release regressions using real local HTTP clients and deterministic upstreams."""
import io
import http.client
import hashlib
import ssl
import json
import tempfile
import subprocess
import socket
import sys
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest.mock import patch

from genesis.config import evaluation_hash, load
from genesis.isolation import Meter, Proxy, ProviderUnavailable
from genesis.supervisor import Supervisor


class Response(io.BytesIO):
    status = 200
    headers = {'Content-Type': 'application/json'}


class CargoArtifactTests(unittest.TestCase):
    def test_l0_generated_target_is_archived_without_hiding_source_changes(self):
        from genesis.adapters import cargo
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); work = root / 'work'; work.mkdir()
            source = work / 'src.rs'; source.write_text('original')
            with self.assertRaisesRegex(RuntimeError, 'test failure'):
                with cargo.runtime(work, root / 'evidence'):
                    output = work / 'target/test-runtime/audit.json'
                    output.parent.mkdir(parents=True); output.write_text('evidence')
                    source.write_text('candidate change')
                    raise RuntimeError('test failure')
            self.assertFalse((work / 'target').exists())
            self.assertEqual((root / 'evidence/target/test-runtime/audit.json').read_text(), 'evidence')
            self.assertEqual(source.read_text(), 'candidate change')

    def test_l0_preserves_preexisting_target_and_external_symlink(self):
        from genesis.adapters import cargo
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); work = root / 'work'; work.mkdir()
            target = work / 'target'; target.mkdir(); (target / 'original').write_text('keep')
            with cargo.runtime(work, root / 'evidence'): pass
            self.assertEqual((target / 'original').read_text(), 'keep')
            target.rename(root / 'external'); target.symlink_to(root / 'external', target_is_directory=True)
            with cargo.runtime(work, root / 'evidence'): pass
            self.assertTrue(target.is_symlink())
            self.assertEqual((root / 'external/original').read_text(), 'keep')


class BufferedProxyTests(unittest.TestCase):
    def test_upstream_connection_reset_keeps_unknown_billing_blocked(self):
        for error in (ConnectionResetError('reset'), BrokenPipeError('pipe closed')):
            with self.subTest(error=type(error).__name__):
                meter = Meter(30)
                with patch('genesis.isolation.open_upstream', side_effect=error):
                    with self.assertRaises(ProviderUnavailable):
                        with Proxy('https://compatible.example/v1', 'test-only', meter) as proxy:
                            request = urllib.request.Request(proxy.url + '/chat/completions', data=b'{}')
                            try:
                                urllib.request.urlopen(request, context=ssl.create_default_context())
                            except (OSError, http.client.RemoteDisconnected):
                                pass
                self.assertEqual(meter.missing_usage_requests, 1)

    def test_upstream_server_error_blocks_evaluation_without_replaying(self):
        for status in (500, 502, 503, 504):
            with self.subTest(status=status):
                meter = Meter(30)
                error = urllib.error.HTTPError('https://compatible.example', status, 'unavailable', {}, None)
                with patch('genesis.isolation.open_upstream', side_effect=error) as upstream:
                    with self.assertRaises(ProviderUnavailable):
                        with Proxy('https://compatible.example/v1', 'test-only', meter) as proxy:
                            request = urllib.request.Request(proxy.url + '/chat/completions', data=b'{}')
                            with self.assertRaises(urllib.error.HTTPError):
                                urllib.request.urlopen(request, context=ssl.create_default_context())
                    self.assertEqual(upstream.call_count, 1)
                with self.assertRaises(ProviderUnavailable): meter.check()

    def test_billed_malformed_completion_is_infrastructure_failure(self):
        meter = Meter(30); meter.upstream_stream = False
        with self.assertRaises(ProviderUnavailable):
            self.request({'usage': {'prompt_tokens': 7, 'completion_tokens': 3}, 'choices': []}, meter)
        self.assertEqual((meter.input_tokens, meter.output_tokens), (7, 3))
        self.assertEqual(meter.missing_usage_requests, 0)

    def request(self, data, meter):
        with patch('genesis.isolation.open_upstream', return_value=Response(json.dumps(data).encode())) as upstream:
            with Proxy('https://compatible.example/v1', 'test-only', meter, allowed_model='test') as proxy:
                request = urllib.request.Request(proxy.url + '/chat/completions',
                    data=json.dumps({'model': 'test', 'stream': True, 'messages': []}).encode())
                with urllib.request.urlopen(request, context=ssl.create_default_context()) as response:
                    content_type = response.headers['Content-Type']
                    body = response.read().decode()
        return body, content_type, json.loads(upstream.call_args.args[1]), upstream.call_count

    def test_nonstream_upstream_preserves_text_tools_finish_and_usage_as_sse(self):
        meter = Meter(30, .15, .6); meter.upstream_stream = False
        data = {'id': 'response-1', 'model': 'test', 'choices': [
            {'index': 0, 'message': {'role': 'assistant', 'content': '你好', 'reasoning_content': 'reason',
             'tool_calls': [{'id': 'call-1', 'type': 'function', 'function': {'name': 'exec', 'arguments': '{"x":1}'}}]},
             'finish_reason': 'tool_calls'}], 'usage': {'prompt_tokens': 7, 'completion_tokens': 3}}
        body, content_type, request, calls = self.request(data, meter)
        self.assertIs(request['stream'], False)
        self.assertNotIn('stream_options', request)
        self.assertEqual(calls, 1)
        self.assertIn('text/event-stream', content_type)
        events = [json.loads(line[5:]) for line in body.splitlines() if line.startswith('data:') and '[DONE]' not in line]
        choice = next(e['choices'][0] for e in events if e.get('choices'))
        self.assertEqual(choice['delta']['content'], '你好')
        self.assertEqual(choice['delta']['reasoning_content'], 'reason')
        self.assertEqual(choice['delta']['tool_calls'][0]['index'], 0)
        self.assertEqual(choice['delta']['tool_calls'][0]['function']['arguments'], '{"x":1}')
        self.assertEqual(choice['finish_reason'], 'tool_calls')
        self.assertEqual(events[-1]['usage'], data['usage'])
        self.assertIn('data: [DONE]', body)
        self.assertEqual((meter.input_tokens, meter.output_tokens), (7, 3))

    def test_buffered_missing_usage_fails_before_delivering_success(self):
        meter = Meter(30); meter.upstream_stream = False
        with self.assertRaises(urllib.error.HTTPError):
            self.request({'choices': [{'message': {'content': 'unmetered'}, 'finish_reason': 'stop'}]}, meter)
        self.assertEqual(meter.missing_usage_requests, 1)
        self.assertEqual(meter.last_proxy_error['type'], 'ProviderUnavailable')
        with self.assertRaises(ProviderUnavailable): meter.check()

    def test_stream_setting_invalidates_evaluation_cache(self):
        config = load(); first = evaluation_hash(config)
        config['agent']['upstream_stream'] = not config['agent'].get('upstream_stream', True)
        self.assertNotEqual(first, evaluation_hash(config))

    def test_invalid_token_usage_cannot_credit_budget(self):
        for usage in ({'prompt_tokens': -10, 'completion_tokens': 2}, {'total_tokens': 5},
                      {'prompt_tokens': 'bad', 'completion_tokens': 2}, {'prompt_tokens': True, 'completion_tokens': 2}):
            with self.subTest(usage=usage):
                meter = Meter(30, .15, .6)
                meter.charge(usage)
                self.assertEqual(meter.cost, 0)
                self.assertEqual(meter.missing_usage_requests, 1)
                self.assertEqual(meter.last_invalid_usage['type'], 'dict')


class UsageReservationTests(unittest.TestCase):
    def test_reservation_survives_restart_without_fabricated_token_usage(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = load(); config['_root'] = tmp
            s = Supervisor(config); s.meter.charge({'prompt_tokens': 10, 'completion_tokens': 2}); s.meter.charge(None)
            actual = s.meter.cost
            s.reserve_unknown_usage(1, 'Conservative operator reservation for interrupted request; actual bill unknown')
            s.archive.db.close()
            s = Supervisor(config); self.addCleanup(s.archive.db.close)
            s.meter.check()
            self.assertEqual(s.meter.cost, actual)
            self.assertEqual((s.meter.input_tokens, s.meter.output_tokens), (10, 2))
            self.assertEqual(s.meter.reserved_cost, 1)
            self.assertEqual(s.meter.reservations[0]['unknown_requests'], 1)
            s.meter.limit = 1
            from genesis.isolation import BudgetExceeded
            with self.assertRaises(BudgetExceeded): s.meter.check()
            with self.assertRaises(ValueError): s.reserve_unknown_usage(1, 'no outstanding unknown requests')

    def test_invalid_reservations_leave_block_intact(self):
        for amount, reason in ((0, 'zero'), (-1, 'negative'), (float('nan'), 'nan'), (float('inf'), 'inf'), (True, 'bool'), (1, '')):
            with tempfile.TemporaryDirectory() as tmp:
                config = load(); config['_root'] = tmp
                s = Supervisor(config); self.addCleanup(s.archive.db.close); s.meter.charge(None)
                with self.assertRaises(ValueError): s.reserve_unknown_usage(amount, reason)
                self.assertEqual(s.meter.missing_usage_requests, 1)


class TrustedSelfcheckTests(unittest.TestCase):
    def test_eab_profile_has_config_directory_before_writing_policy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); runner = root / 'runner.py'; settings = root / 'runtime/home/.blockcell'
            runner.write_text('from pathlib import Path\nfrom types import SimpleNamespace\n'
                'BLOCKCELL_DIR=Path(' + repr(str(settings)) + ')\n'
                'eab60=SimpleNamespace(_env=lambda:{},Gateway=type("Gateway",(),{}))\n'
                'def apply_profile(profile): (BLOCKCELL_DIR/"tool_policy.yaml").write_text("deny")\n'
                'def load_config(): return {"agents":{"defaults":{}},"providers":{"eab60test":{}},"gateway":{}}\n'
                'def save_config(config): pass\n'
                'def main(): apply_profile("deny_exec")\n')
            context=root/'context.json'; context.write_text(json.dumps({'runner':str(runner),'ids':[], 'model':'test','proxy_url':'http://127.0.0.1:12345/v1'}))
            launcher=Path(__file__).resolve().parents[1]/'eab_launcher.py'
            result=subprocess.run([sys.executable,'-I','-S',str(launcher),str(context)],capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual((settings/'tool_policy.yaml').read_text(),'deny')

    def test_eab_controller_interrupt_stops_detached_gateway(self):
        from genesis.source import command
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); ready = root / 'ready'; cleaned = root / 'cleaned'
            runner = root / 'runner.py'
            runner.write_text('import types,time\nfrom pathlib import Path\n'
                'class Gateway:\n def stop(self,timeout=15): Path(' + repr(str(cleaned)) + ').touch()\n'
                'eab60=types.SimpleNamespace(Gateway=Gateway,_env=lambda:{})\n'
                'def apply_profile(profile): pass\n'
                'def main():\n gateway=eab60.Gateway()\n Path(' + repr(str(ready)) + ').touch()\n time.sleep(30)\n')
            context = root / 'context.json'; context.write_text(json.dumps({'runner': str(runner), 'ids': []}))
            def check():
                if ready.exists(): raise InterruptedError('test cancellation')
            launcher = Path(__file__).resolve().parents[1] / 'eab_launcher.py'
            with self.assertRaises(InterruptedError):
                command([sys.executable, '-I', '-S', str(launcher), str(context)], root, timeout=10, check=check)
            self.assertTrue(cleaned.exists(), 'detached gateway cleanup did not run')

    def test_command_interrupt_allows_controller_cleanup(self):
        from genesis.source import command
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); ready = root / 'ready'; cleaned = root / 'cleaned'
            script = ('import signal,time\nfrom pathlib import Path\n'
                      'def stop(*args):\n Path(' + repr(str(cleaned)) + ').write_text("cleaned"); raise SystemExit(0)\n'
                      'signal.signal(signal.SIGTERM,stop)\nPath(' + repr(str(ready)) + ').touch()\ntime.sleep(30)\n')
            def check():
                if ready.exists(): raise InterruptedError('test cancellation')
            with self.assertRaises(InterruptedError):
                command([sys.executable, '-I', '-S', '-c', script], root, timeout=10, check=check)
            self.assertEqual(cleaned.read_text() if cleaned.exists() else '', 'cleaned')

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_local_fault_service_does_not_grant_other_network_access(self):
        from genesis.native_sandbox import macos_command
        with socket.socket() as permitted, socket.socket() as forbidden:
            permitted.bind(('127.0.0.1', 0)); permitted.listen()
            forbidden.bind(('127.0.0.1', 0)); forbidden.listen()
            allowed = permitted.getsockname()[1]; blocked = forbidden.getsockname()[1]
            script = ('import socket\nsocket.create_connection(("127.0.0.1",' + str(allowed) + '),timeout=2).close()\n'
                      'try: socket.create_connection(("127.0.0.1",' + str(blocked) + '),timeout=2); raise AssertionError("other port allowed")\n'
                      'except PermissionError: pass\n')
            argv = macos_command([sys.executable, '-I', '-S', '-c', script], [], local_service_ports=[allowed])
            result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_eab_hook_can_record_without_access_to_grader_or_scores(self):
        from genesis.eab_isolation import gateway_command
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); scripts = root / 'scripts'; scripts.mkdir()
            reports = root / 'reports'; reports.mkdir()
            hook = scripts / 'hook_collect.py'
            hook.write_text('from pathlib import Path\nimport sys\nPath(sys.argv[1]).write_text("hook-event")\n')
            grader = scripts / 'graders.py'; grader.write_text('hidden')
            score = reports / 'results.json'; score.write_text('trusted')
            log = reports / 'hook_events.jsonl'
            script = ('import runpy,sys\nfrom pathlib import Path\n'
                      'sys.argv=[' + repr(str(hook)) + ',' + repr(str(log)) + ']\n'
                      'runpy.run_path(sys.argv[0],run_name="__main__")\n'
                      'try: Path(' + repr(str(grader)) + ').read_text(); raise AssertionError("grader visible")\n'
                      'except PermissionError: pass\n'
                      'try: Path(' + repr(str(score)) + ').write_text("forged"); raise AssertionError("score writable")\n'
                      'except PermissionError: pass\n')
            argv = gateway_command([sys.executable, '-I', '-S', '-c', script], root, sys.executable,
                                   'http://127.0.0.1:12345/v1', hook_script=hook)
            result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(log.read_text(), 'hook-event')
            self.assertEqual(score.read_text(), 'trusted')

    def test_selfcheck_cannot_restore_through_candidate_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); config = load(); config['_root'] = tmp
            config['agent']['require_bwrap'] = False
            s = Supervisor(config); self.addCleanup(s.archive.db.close)
            secret = root / 'host-file'; secret.write_text('untouched')
            def candidate(binary, prompt, work, mode, **kwargs):
                (work / 'check.py').unlink()
                (work / 'check.py').symlink_to(secret)
            with patch.object(s.adapter, 'call', side_effect=candidate):
                self.assertFalse(s.evaluator.selfcheck('/binary'))
            self.assertEqual(secret.read_text(), 'untouched')


class EvaluationRecoveryTests(unittest.TestCase):
    def test_l0_ignores_legacy_environment_failure_cache(self):
        from genesis.adapters import cargo
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); config = load(); config['_root'] = str(root)
            config['benchmarks']['L0'] = [['cargo', 'test', '--workspace']]
            key = hashlib.sha256(json.dumps(config['benchmarks']['L0'], sort_keys=True).encode()).hexdigest()[:16]
            cache = root / 'L0-baselines' / ('sha-' + key); cache.mkdir(parents=True)
            (cache / 'checks.json').write_text(json.dumps([{'passed': False, 'tail': 'old Cargo configuration permission error'}]))
            valid = {'passed': True, 'diagnostics': {}, 'failures': {}, 'tail': '', 'argv': ['cargo', 'test']}
            with patch('genesis.adapters.cargo.command', return_value='sha\n'), \
                 patch('genesis.adapters.cargo.capture', return_value=valid) as capture:
                result = cargo.baseline(root, config)
            self.assertTrue(result[0]['passed'])
            self.assertEqual(capture.call_count, 1)

    def test_eab_completed_case_survives_provider_failure(self):
        from genesis.adapters.eab60 import evaluate
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); binary = root / 'binary'; binary.write_text('version one')
            config = load(); config['_root'] = str(root); config['agent']['require_bwrap'] = False
            kit = root / 'kit'
            kit.mkdir()
            config['benchmarks']['L3']['eab60'].update(kit=str(kit), dev=['A01', 'A02'])
            calls = []; fail = True
            def harness(argv, *args, **kwargs):
                context = json.loads(Path(argv[-1]).read_text()); ids = context['ids']; calls.extend(ids)
                if fail and 'A02' in ids: raise ProviderUnavailable('test outage')
                folder = Path(context['root']) / 'reports'; folder.mkdir()
                (folder / 'results.json').write_text(json.dumps([
                    {'id': case, 'grade': {'status': 'PASS', 'checks': [], 'notes': []}} for case in ids]))
            with patch('genesis.adapters.eab60.command', side_effect=harness):
                with self.assertRaises(ProviderUnavailable):
                    evaluate(root, str(binary), root / 'eval', config, 'dev', Meter(30))
                fail = False; calls.clear()
                result = evaluate(root, str(binary), root / 'eval', config, 'dev', Meter(30))
                self.assertEqual(calls, ['A02'])
                self.assertEqual(len(result['results']), 2)

    def test_finished_task_survives_provider_failure_but_not_binary_change(self):
        from types import SimpleNamespace
        from genesis.adapters.coding_eval import evaluate
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); suite = root / 'tests/coding_eval'; suite.mkdir(parents=True)
            (suite / 'fixture').mkdir(); (suite / 'accept.py').write_text('')
            tasks = [dict(id=x, fixture='fixture', acceptance='accept.py', prompt=x) for x in ('t1', 't2')]
            (suite / 'manifest.json').write_text(json.dumps({'tasks': tasks}))
            binary = root / 'binary'; binary.write_text('version one')
            config = load(); config['agent']['concurrency'] = 1
            output = root / 'reports/report.json'; calls = []
            def interrupted(argv, *args, **kwargs):
                calls.append(argv[-1])
                if argv[-1] == 't2': raise ProviderUnavailable('test provider failure')
                return ''
            with patch('genesis.adapters.coding_eval.command', side_effect=interrupted), \
                 patch('genesis.adapters.coding_eval.verify_workspace', return_value=SimpleNamespace(stdout='Ran 1 test in 0.01s', returncode=0)):
                with self.assertRaises(ProviderUnavailable): evaluate(root, str(binary), output, config, root / 'wrapper')
            self.assertEqual(calls, ['t1', 't2'])
            with patch('genesis.adapters.coding_eval.command', return_value='') as command, \
                 patch('genesis.adapters.coding_eval.verify_workspace', return_value=SimpleNamespace(stdout='Ran 1 test in 0.01s', returncode=0)):
                result = evaluate(root, str(binary), output, config, root / 'wrapper')
                self.assertEqual(command.call_count, 1)
                self.assertTrue(all(row['passed'] for row in result['results']))
                binary.write_text('version two')
                evaluate(root, str(binary), output, config, root / 'wrapper')
                self.assertEqual(command.call_count, 3)
            binary.write_text('version three')
            with patch('genesis.adapters.coding_eval.command', side_effect=RuntimeError('wrapper exited')), \
                 patch('genesis.adapters.coding_eval.verify_workspace', return_value=SimpleNamespace(stdout='Ran 1 test in 0.01s', returncode=0)):
                def unavailable(): raise ProviderUnavailable('upstream unavailable')
                with self.assertRaises(ProviderUnavailable):
                    evaluate(root, str(binary), output, config, root / 'wrapper', check=unavailable)
