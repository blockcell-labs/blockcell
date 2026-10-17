# BlockCell Repository Tests & Evaluation Assets

This directory contains two distinct categories of verification assets (note that Rust crate unit and integration tests live alongside each crate under `crates/*/src`, `crates/*/tests`, and `bin/blockcell`):

1. **Infrastructure & Supply-Chain Checks (`*.sh`)**
   - `build_supply_chain_infrastructure.sh`: Verifies embedded WebUI asset hash freshness (`webui/scripts/source-hashes.mjs --check`), workspace metadata inheritance across all `blockcell*` crates, and `.cargo/config.toml` target flags.
   - `install_release_infrastructure.sh`: Tests `install.sh` (source, release, and auto modes) using hermetic fake binaries, covering atomic replacement, SHA-256 verification, tar path-traversal rejection, and downgrade protection.
   - `release_workflow_infrastructure.sh`: Audits `.github/workflows/release.yml` for pinned 40-char Action SHAs, least-privilege `permissions`, version-tag consistency, and CI budget gating.

2. **Agent Coding Evaluation Harness (`coding_eval/`)**
   - A 20-task Python benchmark suite (`bugfix`, `feature`, `refactor`, `greenfield`) for evaluating coding agents in isolated temporary Git repositories.
   - Run the harness self-tests with:
     ```sh
     python3 -m unittest discover -s tests/coding_eval -p "test_*.py"
     ```
   - See [`coding_eval/README.md`](coding_eval/README.md) for benchmark runner usage and metrics format.

## Running Everything Locally

Use the root `Makefile` target to run Rust workspace tests, Python harness self-tests, and all three infrastructure shell scripts:

```sh
make test
```
