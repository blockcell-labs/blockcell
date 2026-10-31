.PHONY: release reload test test-rust test-python test-infra test-webui webui-dist hooks

release:
	cd webui && npm ci && npm test && npm run build
	cargo build --release --locked
	@set -eu; \
	install_dir="$(HOME)/.local/bin"; \
	mkdir -p "$$install_dir"; \
	temporary_path="$$install_dir/.blockcell.tmp.$$$$"; \
	trap 'rm -f "$$temporary_path"' EXIT; \
	cp target/release/blockcell "$$temporary_path"; \
	chmod +x "$$temporary_path"; \
	mv -f "$$temporary_path" "$$install_dir/blockcell"; \
	trap - EXIT; \
	"$$install_dir/blockcell" --version


reload:
	cp -r skills/* ~/.blockcell/workspace/skills/ || true
	cargo run -p blockcell -- skills reload && \
	blockcell --version

# ---------------------------------------------------------------------------
# Local quality gate.
#
# CI is intentionally disabled until the project has GitHub Actions budget
# (see tests/release_workflow_infrastructure.sh), so `make test` is the
# supported way to run the whole suite. Wire it to git with `make hooks`.
# ---------------------------------------------------------------------------
test: test-rust test-python test-infra test-eab60

# Rust tests embed webui/dist and bin/blockcell/build.rs asserts its hash
# matches webui/src, so rebuild the bundle first when it has gone stale.
webui-dist:
	@node webui/scripts/source-hashes.mjs --check >/dev/null 2>&1 \
		|| (echo "webui dist is stale — rebuilding" && cd webui && npm run build)

test-rust: webui-dist
	cargo fmt --all --check
	cargo test --workspace

# Harness self-tests only: unittest discovery does not descend into the
# non-package tasks/*/repo fixture directories.
test-python:
	python3 -B -m unittest discover -s tests/coding_eval -p "test_*.py" -v

test-infra:
	sh tests/build_supply_chain_infrastructure.sh
	sh tests/install_release_infrastructure.sh
	sh tests/release_workflow_infrastructure.sh

# Isolated gateway survival probe: starts a real gateway with the *default*
# channel configuration and asserts it stays up. This is the only automated
# guard for the "disabled channel kills the gateway" regression, so it runs on
# every `make test` (debug binary keeps it cheap).
test-eab60:
	cargo build -p blockcell
	EAB60_BIN=$(CURDIR)/target/debug/blockcell python3 tests/eab60/startup.py

test-webui:
	cd webui && npm ci && npm test

# Install the tracked git hooks (pre-push runs `make test`).
hooks:
	git config core.hooksPath .githooks
	@echo "git hooks installed: core.hooksPath=.githooks (pre-push runs 'make test')"
