#!/usr/bin/env python3
"""Run review probes against current crate APIs using only temporary artificial data.

The output records current behavior. Exit zero is NOT a regression/security pass.
No project source, toolchain selection or root lockfile is modified.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def main():
    toolchain = "1.96.0"
    rustc = subprocess.check_output(
        ["rustup", "which", "--toolchain", toolchain, "rustc"], text=True
    ).strip()
    rustdoc = str(Path(rustc).with_name("rustdoc"))
    env = os.environ.copy()
    env.update(RUSTC=rustc, RUSTDOC=rustdoc, CARGO_TARGET_DIR=str(ROOT / "target"))
    with tempfile.TemporaryDirectory(prefix="blockcell-review-api-", dir="/private/tmp") as tmp:
        tmp = Path(tmp)
        (tmp / "src").mkdir()
        shutil.copyfile(HERE / "probe_api.rs", tmp / "src/main.rs")
        dependencies = "\n".join(
            f'blockcell-{name} = {{ path = {json.dumps(str(ROOT / "crates" / name))} }}'
            for name in ("core", "storage", "tools", "agent")
        )
        (tmp / "Cargo.toml").write_text(
            '[package]\nname = "blockcell-review-api-probe"\nversion = "0.0.0"\nedition = "2021"\n'
            '[workspace]\n[dependencies]\n' + dependencies + '\n'
            'serde_json = "1.0"\n'
            'tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }\n'
            'zip = { version = "2.2", default-features = false, features = ["deflate"] }\n',
            encoding="utf-8",
        )
        # Seed dependency versions from the audited checkout; only the temporary
        # lockfile may be updated to add this probe package.
        shutil.copyfile(ROOT / "Cargo.lock", tmp / "Cargo.lock")
        subprocess.run(
            ["rustup", "run", toolchain, "cargo", "run", "--offline", "--manifest-path",
             str(tmp / "Cargo.toml"), "--", str(tmp / "data")],
            cwd=ROOT, env=env, check=True,
        )


if __name__ == "__main__":
    main()
