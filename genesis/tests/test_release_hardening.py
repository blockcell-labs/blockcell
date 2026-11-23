import json
import os
import random
import runpy
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from genesis.archive import Archive
import time

from genesis.config import load
from genesis.selector import Selector
from genesis.supervisor import Supervisor


class IsolationTests(unittest.TestCase):
    def test_gateway_launcher_executes_in_place_for_signal_delivery(self):
        from genesis import eab_isolation
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); (root/'runtime/workspace').mkdir(parents=True)
            context=root/'settings.json'
            context.write_text(json.dumps({'root':str(root),'binary':'/binary','proxy_url':'http://127.0.0.1:12345/v1'}))
            class Executed(Exception): pass
            with patch.object(sys,'argv',['gateway',str(context),'gateway']), \
                    patch('genesis.native_sandbox.macos_command',return_value=['/sandbox','/binary','gateway']), \
                    patch('sys.platform','darwin'), \
                    patch('os.chdir'), \
                    patch('os.execvpe',side_effect=Executed) as execute, \
                    patch('subprocess.call',return_value=0):
                with self.assertRaises((Executed,SystemExit)):
                    runpy.run_path(str(Path(eab_isolation.__file__)),run_name='__main__')
            self.assertTrue(execute.called)
            self.assertEqual(execute.call_args.args[:2],('/sandbox',['/sandbox','/binary','gateway']))

    def test_patch_retains_pinned_toolchain_path(self):
        from genesis.agent_adapter import AgentAdapter
        from genesis.isolation import Meter
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); work = root/'work'; work.mkdir()
            config = load(); config['_root'] = str(root)
            adapter = AgentAdapter(config, root/'calls', Meter(1, 0, 0))
            with patch('genesis.preflight.toolchain_bin', return_value='/pinned/bin'), \
                    patch('genesis.agent_adapter.sandbox', side_effect=lambda argv, *a, **kw: argv), \
                    patch('genesis.agent_adapter.command', return_value='done') as call:
                adapter.call('/binary', 'patch', work, 'patch')
            self.assertIn('/pinned/bin', call.call_args.args[3]['PATH'].split(os.pathsep))

    def test_isolated_home_authorizes_only_test_executables(self):
        from genesis.isolation import prepare_home
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); config = load()
            prepare_home(root/'home', root/'work', config, 'http://127.0.0.1:12345/v1')
            policy = json.loads((root/'home/.blockcell/path_access.json5').read_text())
            self.assertTrue(policy['builtin_protected_paths'])
            self.assertEqual(policy['default_policy'], 'confirm')
            self.assertTrue(all(rule['ops'] == ['exec'] for rule in policy['rules']))
            env = prepare_home(root/'home', root/'work', config, 'http://127.0.0.1:12345/v1')
            self.assertTrue(Path(env['TMPDIR']).is_relative_to((root/'home').resolve()))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_macos_system_linker_can_load_xcode_frameworks(self):
        from genesis.native_sandbox import macos_command
        result=subprocess.run(macos_command(['/usr/bin/cc','--version'],[]),capture_output=True,text=True,timeout=10)
        self.assertEqual(result.returncode,0,result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_cargo_ancestor_configuration_outside_workspace_is_hidden(self):
        from genesis.native_sandbox import macos_command
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);work=root/'work';work.mkdir();(root/'.cargo').mkdir()
            config=root/'.cargo/config.toml';config.write_text('[build]\n')
            script=('import pathlib\ntry: pathlib.Path('+repr(str(config.resolve()))+').stat(); raise AssertionError("ancestor visible")'
                    '\nexcept PermissionError: pass')
            result=subprocess.run(macos_command([sys.executable,'-I','-c',script],[work]),capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_sandbox_parent_can_terminate_its_child(self):
        from genesis.native_sandbox import macos_command
        script = ('import subprocess\np=subprocess.Popen(["/bin/sleep","3"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n'
                  'p.terminate()\np.wait(timeout=2)\n')
        result = subprocess.run(macos_command([sys.executable, '-I', '-c', script], []),
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_host_cargo_configuration_is_not_discoverable(self):
        from genesis.native_sandbox import macos_command
        host_config = Path.home()/'.cargo/config.toml'
        if not host_config.is_file():
            self.skipTest('host Cargo config absent')
        script = ('import pathlib\ntry: pathlib.Path('+repr(str(host_config))+').stat(); raise AssertionError("host config visible")'
                  '\nexcept PermissionError: pass')
        result = subprocess.run(macos_command([sys.executable, '-I', '-c', script], []),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_system_crypto_configuration_is_readable(self):
        from genesis.native_sandbox import macos_command
        result = subprocess.run(macos_command(['/usr/bin/openssl', 'version'], []),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_shell_can_read_its_script_file_descriptor(self):
        from genesis.native_sandbox import macos_command
        p = subprocess.run(macos_command(['/bin/bash', '-c', 'source /dev/fd/0'], []),
                           input='echo shell-ok\n', capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn('shell-ok', p.stdout)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_proxy_rule_allows_only_configured_loopback_port(self):
        from genesis.native_sandbox import macos_command
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0)); server.listen()
            port = server.getsockname()[1]
            script = 'import socket; s=socket.create_connection(("127.0.0.1",'+str(port)+')); s.close()'
            argv = macos_command([sys.executable, '-I', '-S', '-c', script], [], proxy_url=f'http://127.0.0.1:{port}/v1')
            p = subprocess.run(argv, capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)

    def test_eab_gateway_command_hides_grader_and_reports(self):
        from genesis.eab_isolation import gateway_command
        if sys.platform != 'darwin':
            self.skipTest('macOS profile')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            argv = gateway_command(['/usr/bin/true'], root, '/usr/bin/true', 'http://127.0.0.1:12345/v1')
            profile = argv[2]
            for hidden in ('reports', 'evidence', 'kit', 'graders.py'):
                self.assertNotIn(str(root / hidden), profile)
            self.assertIn(str((root / 'runtime/workspace').resolve()), profile)
            self.assertIn('localhost:18790', profile)

    def test_coding_verification_cannot_read_host_or_write_result(self):
        from genesis.adapters.coding_eval import verify_workspace
        from genesis.isolation import clean_env
        if sys.platform != 'darwin':
            self.skipTest('macOS integration')
        parent = Path(__file__).resolve().parents[2] / 'tmp'
        parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as tmp:
            root = Path(tmp); work = root / 'work'; work.mkdir()
            secret = root / 'secret'; secret.write_text('canary')
            grader = root / 'grader'; grader.mkdir()
            verify = grader / 'verify.py'
            verify.write_text('from pathlib import Path\n'
                              'try: Path('+repr(str(secret))+').read_text(); raise AssertionError("host visible")\n'
                              'except PermissionError: pass\n'
                              'try: Path('+repr(str(root/'report.json'))+').write_text("forged"); raise AssertionError("report writable")\n'
                              'except PermissionError: pass\n'
                              'print("Ran 1 test in 0.01s")\n')
            result = verify_workspace(verify, work, grader, root / 'verify-home', 10, clean_env(root/'verify-home','OPENAI_API_KEY'))
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertFalse((root / 'report.json').exists())

    def test_symlink_fixture_test_is_rejected_without_touching_host(self):
        from genesis.adapters.coding_eval import restore_tests
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); fixture = root / 'fixture'; fixture.mkdir()
            work = root / 'work'; work.mkdir()
            secret = root / 'secret'; secret.write_text('untouched')
            (fixture / 'test_a.py').write_text('trusted')
            (work / 'test_a.py').symlink_to(secret)
            with self.assertRaisesRegex(RuntimeError, 'symlink'):
                restore_tests(fixture, work)
            self.assertEqual(secret.read_text(), 'untouched')

    def test_macos_profile_hides_secrets_and_restricts_network(self):
        from genesis.native_sandbox import macos_command
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            argv = macos_command(['/usr/bin/true'], [root / 'work'], [root / 'home'], proxy_url='http://127.0.0.1:12345/v1')
            profile = argv[2]
            self.assertIn('(deny default)', profile)
            self.assertIn('localhost:12345', profile)
            self.assertNotIn('(allow network*)', profile)
            self.assertNotIn('(allow file-read*)\n', profile)
            self.assertNotIn('(allow file-read* (subpath '+json.dumps(str(Path.home()/'.cargo'))+'))', profile)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS native isolation')
    def test_macos_enforces_file_and_network_boundaries(self):
        from genesis.native_sandbox import macos_command
        root_parent = Path(__file__).resolve().parents[2] / 'tmp'
        root_parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root_parent) as tmp:
            root = Path(tmp)
            work = root / 'work'; work.mkdir()
            secret = root / 'host-secret'; secret.write_text('canary')
            script = ('import pathlib,socket,sys; w=pathlib.Path(sys.argv[1]); '
                      'w.joinpath("allowed").write_text("ok"); '
                      '\ntry: pathlib.Path(sys.argv[2]).read_text(); raise AssertionError("secret visible")'
                      '\nexcept PermissionError: pass'
                      '\ntry: socket.create_connection(("1.1.1.1",443),timeout=1); raise AssertionError("network allowed")'
                      '\nexcept OSError: pass\nprint("isolated")')
            argv = macos_command([sys.executable, '-I', '-B', '-c', script, str(work), str(secret)], [work], [work])
            result = subprocess.run(argv, cwd=work, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((work / 'allowed').read_text(), 'ok')
            self.assertIn('isolated', result.stdout)

    def test_build_uses_independent_cargo_home(self):
        from genesis.isolation import build_sandbox
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            host = root / 'host-cargo'; host.mkdir()
            (host / 'credentials.toml').write_text('secret')
            (host / 'registry').mkdir()
            home = root / 'home'
            env = {'HOME': str(home), 'CARGO_HOME': str(host), 'RUSTUP_HOME': str(root / 'rustup')}
            with patch('genesis.isolation.shutil.which', return_value='/bin/bwrap'):
                argv = build_sandbox(['cargo', 'check'], root / 'work', root / 'target', home, env)
            self.assertNotEqual(env['CARGO_HOME'], str(host))
            self.assertNotIn(str(host), argv)
            self.assertNotIn(str(host / 'credentials.toml'), ' '.join(argv))


class RecoveryTests(unittest.TestCase):
    def test_cli_reports_locked_workspace_without_traceback(self):
        from genesis.__main__ import main
        with patch.object(sys,'argv',['genesis','status']), \
                patch('genesis.__main__.Supervisor',side_effect=RuntimeError('already running')), \
                patch('sys.stderr') as stderr:
            self.assertEqual(main(),1)
        self.assertIn('already running',''.join(c.args[0] for c in stderr.write.call_args_list))

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.config = load()
        self.config['_root'] = str(self.root)
        self.config['agent']['require_bwrap'] = False
        self.config['genesis']['budget'] = {'total_usd': 30, 'per_cycle_usd': 2}
        self.s = Supervisor(self.config)
        self.addCleanup(self.s.archive.db.close)

    def test_unsafe_archive_never_selected_even_when_forced(self):
        self.s.archive.add('safe', 'a', status='PARENT')
        self.s.archive.add('unsafe', 'b', status='ARCHIVE')
        self.s.archive.update('unsafe', parent_eligible=0)
        for forced in (False, True):
            selected = Selector(self.config, random.Random(1)).select(self.s.archive.agents(), forced)
            self.assertEqual(selected['agent_id'], 'safe')

    def test_resume_restores_cycle_cap_before_next_request(self):
        self.s.archive.add('genesis-0', 'a', status='PARENT')
        self.s.archive.transition('SELECT_PARENT', {'cycle': 1, 'target_cycle': 1,
                                 'cycle_cost': 1.0, 'cycle_started': time.time()})
        seen = []
        def select(*args):
            seen.append(self.s.meter.limit)
            raise KeyboardInterrupt()
        with patch.object(self.s.selector, 'select', side_effect=select):
            with self.assertRaises(KeyboardInterrupt):
                self.s.run(resume=True)
        self.assertEqual(seen, [3.0])

    def test_resume_initial_build_before_genesis_record_exists(self):
        self.s.archive.transition('BUILD_GENESIS0', {'cycle': 0})
        def initialize():
            self.s.archive.add('genesis-0', 'a', status='PARENT')
            self.s.archive.transition('IDLE', {'cycle': 0, 'target_cycle': 0})
        with patch.object(self.s, 'init', side_effect=initialize) as init:
            self.s.run(resume=True)
        init.assert_called_once()

    def test_second_process_cannot_mutate_locked_workspace(self):
        from genesis.locking import WorkspaceLock
        with WorkspaceLock(self.root):
            code = ('from pathlib import Path; from genesis.locking import WorkspaceLock; '
                    'lock=WorkspaceLock(Path(__import__("sys").argv[1])); lock.__enter__()')
            result = subprocess.run([sys.executable, '-c', code, str(self.root)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('already running', result.stderr)

    def test_initialization_cannot_migrate_archive_while_workspace_locked(self):
        from genesis.locking import WorkspaceLock
        with WorkspaceLock(self.root), patch('genesis.supervisor.Archive') as archive:
            with self.assertRaisesRegex(RuntimeError, 'already running'):
                Supervisor(self.config)
        archive.assert_not_called()

    def test_promotion_does_not_create_branches(self):
        from genesis.source import Source
        source = Source(self.config)
        with patch.object(source, 'validate'), patch.object(source, 'snapshot', return_value='sha') as snap, \
                patch('genesis.source.command', side_effect=AssertionError('promotion must use snapshot only')):
            self.assertEqual(source.promote('agent-1', self.root), 'sha')
        snap.assert_called_once()


class TransportTests(unittest.TestCase):
    def test_proxy_transport_failure_blocks_unknown_usage(self):
        import urllib.request
        from genesis.isolation import Meter, Proxy, ProviderUnavailable
        meter = Meter(30)
        with patch('genesis.isolation.open_upstream', side_effect=OSError('connection reset')):
            with self.assertRaises(ProviderUnavailable), Proxy('https://compatible.example/v1', 'test-only', meter) as proxy:
                request = urllib.request.Request(proxy.url + '/chat/completions', data=b'{}')
                with self.assertRaises(urllib.error.HTTPError):
                    urllib.request.urlopen(request)
        self.assertEqual(meter.missing_usage_requests, 1)
        with self.assertRaises(ProviderUnavailable):
            meter.check()

    def test_missing_usage_blocks_without_fabricating_tokens_or_cost(self):
        from genesis.isolation import Meter,ProviderUnavailable
        meter=Meter(30,.15,.6,token_limit=250000000)
        meter.charge({'prompt_tokens':10,'completion_tokens':2})
        before=(meter.input_tokens,meter.output_tokens,meter.cost)
        meter.charge(None)
        self.assertEqual((meter.input_tokens,meter.output_tokens,meter.cost),before)
        self.assertEqual(meter.missing_usage_requests,1)
        with self.assertRaisesRegex(ProviderUnavailable,'usage'):
            meter.check()

    def test_unknown_usage_block_survives_restart(self):
        with tempfile.TemporaryDirectory() as tmp:
            config=load();config['_root']=tmp
            s=Supervisor(config)
            s.meter.charge(None)
            s.archive.db.close()
            restored=Supervisor(config)
            self.addCleanup(restored.archive.db.close)
            self.assertEqual(restored.meter.missing_usage_requests,1)
            self.assertEqual(restored.meter.last_invalid_usage['type'], 'NoneType')

    def test_eab_launcher_constructs_verified_http_client(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runner = root/'runner.py'
            runner.write_text('import urllib.request\nfrom types import SimpleNamespace\n'
                              'eab60=SimpleNamespace(_env=lambda: {},Gateway=type("Gateway",(),{}))\n'
                              'def apply_profile(profile): pass\n'
                              'def main(): urllib.request.build_opener(); print("client-ready")\n')
            context=root/'context.json'
            context.write_text(json.dumps({'runner':str(runner),'ids':[]}))
            launcher=Path(__file__).resolve().parents[1]/'eab_launcher.py'
            result=subprocess.run([sys.executable,str(launcher),str(context)],capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertIn('client-ready',result.stdout)

    def test_dns_fallback_parses_only_addresses_without_changing_hostname(self):
        from genesis.http_transport import resolve_addresses
        with patch('genesis.http_transport.socket.getaddrinfo', side_effect=socket.gaierror('unavailable')), \
                patch('genesis.http_transport.subprocess.check_output', return_value='alias.example.\n192.0.2.10\n') as query:
            self.assertEqual(resolve_addresses('compatible.example', 443), ['192.0.2.10:443'])
        self.assertIn('compatible.example', query.call_args.args[0])

    def test_doctor_reports_missing_dependencies_without_initializing(self):
        from genesis.preflight import inspect_environment
        config = load()
        with patch('genesis.preflight.shutil.which', return_value=None), patch.dict(os.environ, {}, clear=True):
            result = inspect_environment(config, require_key=True)
        self.assertFalse(result['ready'])
        self.assertIn('OPENAI_API_KEY', ' '.join(result['errors']))

    def test_native_transport_keeps_credential_out_of_arguments(self):
        from genesis.http_transport import open_upstream
        with tempfile.TemporaryDirectory() as tmp:
            helper = Path(tmp) / 'transport.py'
            helper.write_text('import json,sys,os\ns=json.load(sys.stdin)\n'
                              'assert os.getsid(0) == os.getpid(), "transport must survive supervisor SIGINT"\n'
                              'assert "private-test-key" not in " ".join(sys.argv)\n'
                              'assert s["key"] == "private-test-key"\n'
                              'print(json.dumps({"status":200,"headers":{"Content-Type":"application/json"}}))\n'
                              'print(json.dumps({"usage":{"prompt_tokens":2}}))\n')
            with patch('genesis.http_transport.resolve_addresses', return_value=['192.0.2.10:443']), \
                    open_upstream('https://compatible.example/v1/chat/completions', b'{}', 'private-test-key',
                                  'Genesis', [sys.executable, str(helper)]) as response:
                self.assertEqual(response.status, 200)
                self.assertEqual(json.loads(response.read())['usage']['prompt_tokens'], 2)


if __name__ == '__main__':
    unittest.main()
