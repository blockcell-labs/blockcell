"""Run the locked EAB harness with supervisor model/proxy configuration."""
import importlib.util
import json
import os
import sys
from pathlib import Path


if __name__ == "__main__":
    context = json.loads(Path(sys.argv[1]).read_text())
    runner = Path(context["runner"])
    sys.path.insert(0, str(runner.parent))
    spec = importlib.util.spec_from_file_location("genesis_frozen_eab", runner)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
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
        original(profile)
        cfg = module.load_config()
        cfg['selfImprove'] = {'nudge': {'enabled': False}, 'review': {'enabled': False}}
        defaults = cfg["agents"]["defaults"]
        defaults["model"] = context["model"]
        for entry in defaults.get("modelPool", []):
            entry["model"] = context["model"]
        cfg["providers"]["eab60test"]["apiBase"] = context["proxy_url"]
        module.save_config(cfg)

    module.apply_profile = configured_profile
    sys.argv = [str(runner), *context["ids"]]
    module.main()
