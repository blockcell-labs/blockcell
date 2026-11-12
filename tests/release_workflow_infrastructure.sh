#!/usr/bin/env sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
WORKFLOW="$ROOT/.github/workflows/release.yml"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

test -f "$ROOT/.github/workflows/ci.yml-" \
  || fail "the intentionally disabled CI workflow was renamed"
test ! -f "$ROOT/.github/workflows/ci.yml" \
  || fail "CI must remain disabled until the project has Actions budget"

grep -F 'workspace_version=' "$WORKFLOW" >/dev/null \
  || fail "release workflow does not read the workspace package version"
grep -F 'Release version $release_version does not match Cargo workspace version $workspace_version' "$WORKFLOW" >/dev/null \
  || fail "release workflow does not reject version mismatches"
grep -F 'points to a different commit' "$WORKFLOW" >/dev/null \
  || fail "manual release can reuse a tag that points at different source code"

grep -F 'npm ci' "$WORKFLOW" >/dev/null \
  || fail "release workflow does not install locked WebUI dependencies"
grep -F 'npm test' "$WORKFLOW" >/dev/null \
  || fail "release workflow does not test WebUI sources"
grep -F 'npm run build' "$WORKFLOW" >/dev/null \
  || fail "release workflow does not rebuild embedded WebUI assets"
grep -F 'rm -rf webui/dist' "$WORKFLOW" >/dev/null \
  || fail "release build does not remove committed WebUI assets before artifact download"

if grep -E '^[[:space:]]*cp README\.md LICENSE([[:space:]]|$)' "$WORKFLOW" >/dev/null; then
  fail "Unix packaging overwrites LICENSE with README.md"
fi

grep -E 'cargo build .*--locked' "$WORKFLOW" >/dev/null \
  || fail "release Cargo build is not locked"

if grep -q '^permissions:' "$WORKFLOW"; then
  fail "release workflow grants permissions globally"
fi
awk '
  /^  release:/ { in_release = 1 }
  in_release && /^    permissions:/ { found_permissions = 1 }
  in_release && /^      contents: write$/ { found_write = 1 }
  END { exit !(found_permissions && found_write) }
' "$WORKFLOW" || fail "only the publishing job should receive contents: write"

grep -F 'basename "$f"' "$WORKFLOW" >/dev/null \
  || fail "checksums are not generated with downloadable asset basenames"

invalid_uses=$(sed -n 's/^[[:space:]]*-[[:space:]]*uses:[[:space:]]*//p' "$WORKFLOW" \
  | awk -F@ 'NF != 2 || $2 !~ /^[0-9a-f]{40}$/ { print }')
test -z "$invalid_uses" \
  || fail "release workflow contains mutable or invalid Action references: $invalid_uses"

python3 -B "$ROOT/tests/test_release_entrypoints.py" ManifestEntrypointTests

echo "release workflow infrastructure tests: ok"
