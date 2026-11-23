"""Run the locked EAB harness with supervisor model/proxy configuration."""
import importlib.util
import json
import os
import ssl
import signal
import sys
from pathlib import Path


if __name__ == "__main__":
    # Raise normally so our cleanup can stop the harness's detached gateway.
    def interrupted(signum, frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    # The locked harness builds urllib's default handlers even for loopback
    # HTTP. Use standard certificate verification, independent of site hooks.
    ssl._create_default_https_context = ssl.create_default_context
    context = json.loads(Path(sys.argv[1]).read_text())
    runner = Path(context["runner"])
    sys.path.insert(0, str(runner.parent))
    spec = importlib.util.spec_from_file_location("genesis_frozen_eab", runner)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    gateways = []
    original_gateway = module.eab60.Gateway

    class TrackedGateway(original_gateway):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            gateways.append(self)

    module.eab60.Gateway = TrackedGateway
    if context.get('isolated'):
        # The trusted controller sees the kit and reports; the gateway never does.
        wrapper = Path(context['root']) / 'gateway_wrapper.py'
        settings = Path(context['root']) / 'gateway_settings.json'
        settings.write_text(json.dumps(context))
        wrapper.write_text('#!' + sys.executable + '\nimport runpy,sys\n'
                           'sys.path.insert(0,' + repr(str(Path(__file__).resolve().parent.parent)) + ')\n'
                           'sys.argv.insert(1,' + repr(str(settings)) + ')\n'
                           'runpy.run_module("genesis.eab_isolation",run_name="__main__")\n')
        wrapper.chmod(0o700)
        module.eab60.BINARY = wrapper
    original = module.apply_profile
    original_env = module.eab60._env

    def configured_env():
        env = original_env()
        for name in ("CARGO_HOME", "RUSTUP_HOME"):
            if name in os.environ:
                env[name] = os.environ[name]
        return env

    module.eab60._env = configured_env

    def configured_profile(profile):
        if hasattr(module, 'BLOCKCELL_DIR'):
            module.BLOCKCELL_DIR.mkdir(parents=True, exist_ok=True)
        original(profile)
        cfg = module.load_config()
        cfg['selfImprove'] = {'nudge': {'enabled': False}, 'review': {'enabled': False}}
        cfg.setdefault('memory', {})['learningSystem'] = {'enabled': False}
        defaults = cfg["agents"]["defaults"]
        defaults["model"] = context["model"]
        for entry in defaults.get("modelPool", []):
            entry["model"] = context["model"]
        cfg["providers"]["eab60test"]["apiBase"] = context["proxy_url"]
        cfg['gateway']['webuiPort'] = 18791
        if context.get('isolated') and sys.platform == 'darwin':
            cfg["tools"]["exec"]["sandboxPolicy"] = 'full-access'  # inherited outer Seatbelt remains enforced
        module.save_config(cfg)

    module.apply_profile = configured_profile
    sys.argv = [str(runner), *context["ids"]]
    try:
        module.main()
    finally:
        for gateway in gateways:
            gateway.stop(timeout=2)
