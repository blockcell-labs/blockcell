#!/usr/bin/env sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

test -f "$ROOT/webui/scripts/source-hashes.mjs" \
  || fail "WebUI source hash generator is missing"
test -f "$ROOT/bin/blockcell/build.rs" \
  || fail "Rust build does not validate embedded WebUI freshness"
grep -F 'source-hashes.mjs' "$ROOT/webui/package.json" >/dev/null \
  || fail "npm build does not generate the WebUI source hash manifest"
node "$ROOT/webui/scripts/source-hashes.mjs" --check \
  || fail "committed WebUI dist is stale"

workspace_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
workspace_rust_version=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
workspace_license=$(sed -n 's/^license = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)

test -n "$workspace_version" || fail "could not read workspace version from Cargo.toml"
test -n "$workspace_rust_version" || fail "could not read workspace rust-version from Cargo.toml"
test -n "$workspace_license" || fail "could not read workspace license from Cargo.toml"

metadata=$(cd "$ROOT" && cargo metadata --locked --no-deps --format-version 1)
printf '%s' "$metadata" | jq -e \
  --arg version "$workspace_version" \
  --arg rust_version "$workspace_rust_version" \
  --arg license "$workspace_license" '
  (.workspace_members | length) as $expected_count
  | [.packages[] | select(.name | startswith("blockcell"))]
  | length == $expected_count
    and all(.version == $version)
    and all(.rust_version == $rust_version)
    and all(.license == $license)
    and all(.authors == ["blockcell"])
' >/dev/null || fail "BlockCell packages do not inherit workspace metadata"

if awk '
  /^\[target\.aarch64-unknown-linux-gnu\]/ { in_target = 1; next }
  /^\[/ { in_target = 0 }
  in_target && /--stack/ { found = 1 }
  END { exit !found }
' "$ROOT/.cargo/config.toml"; then
  fail "Linux ARM64 target still uses the PE-only --stack linker option"
fi

test ! -d "$ROOT/vendor/imap-proto" \
  || fail "unused imap-proto vendor copy is still present"

echo "build supply-chain infrastructure tests: ok"
