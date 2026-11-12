#!/usr/bin/env sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TMP_ROOT=$(mktemp -d)
trap 'rm -rf "$TMP_ROOT"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_common_fakes() {
  fake_bin="$1"
  mkdir -p "$fake_bin"

  cat >"$fake_bin/uname" <<'EOF'
#!/usr/bin/env sh
case "${1:-}" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) echo Linux ;;
esac
EOF

  cat >"$fake_bin/sudo" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF

  cat >"$fake_bin/apt-get" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF

  chmod +x "$fake_bin/uname" "$fake_bin/sudo" "$fake_bin/apt-get"
}

test_source_install_checks_out_requested_version_and_replaces_atomically() {
  case_dir="$TMP_ROOT/source"
  fake_bin="$case_dir/bin"
  log_dir="$case_dir/log"
  mkdir -p "$log_dir"
  make_common_fakes "$fake_bin"

  cat >"$fake_bin/git" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >"$TEST_LOG_DIR/git.args"
for last_arg do :; done
mkdir -p "$last_arg/target/release"
mkdir -p "$last_arg/webui"
printf '#!/usr/bin/env sh\nexit 0\n' >"$last_arg/target/release/blockcell"
chmod +x "$last_arg/target/release/blockcell"
EOF

  cat >"$fake_bin/cargo" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >"$TEST_LOG_DIR/cargo.args"
exit 0
EOF

  cat >"$fake_bin/npm" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >>"$TEST_LOG_DIR/npm.args"
exit 0
EOF

  cat >"$fake_bin/cp" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >>"$TEST_LOG_DIR/cp.args"
/bin/cp "$@"
EOF

  cat >"$fake_bin/mv" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >>"$TEST_LOG_DIR/mv.args"
/bin/mv "$@"
EOF

  chmod +x "$fake_bin/git" "$fake_bin/cargo" "$fake_bin/npm" "$fake_bin/cp" "$fake_bin/mv"

  PATH="$fake_bin:/usr/bin:/bin" \
  TEST_LOG_DIR="$log_dir" \
  BLOCKCELL_INSTALL_METHOD=source \
  BLOCKCELL_VERSION=v9.8.7 \
  BLOCKCELL_INSTALL_DIR="$case_dir/install" \
    sh "$ROOT/install.sh" >/dev/null

  grep -F -- '--branch v9.8.7' "$log_dir/git.args" >/dev/null \
    || fail "source install did not clone the requested version"
  test -f "$case_dir/install/blockcell" \
    || fail "source install did not produce the installed binary"
  grep -F -- "$case_dir/install/.blockcell.tmp." "$log_dir/cp.args" >/dev/null \
    || fail "source install copied directly over the live executable"
  grep -F -- "$case_dir/install/blockcell" "$log_dir/mv.args" >/dev/null \
    || fail "source install did not atomically rename the verified executable"
  grep -Fx 'ci' "$log_dir/npm.args" >/dev/null \
    || fail "source install did not install locked WebUI dependencies"
  grep -Fx 'test' "$log_dir/npm.args" >/dev/null \
    || fail "source install did not test WebUI sources"
  grep -Fx 'run build' "$log_dir/npm.args" >/dev/null \
    || fail "source install did not rebuild embedded WebUI assets"
  grep -F -- '--locked' "$log_dir/cargo.args" >/dev/null \
    || fail "source install Cargo build is not locked"
}

test_source_install_latest_uses_latest_release_tag() {
  case_dir="$TMP_ROOT/source_latest"
  fake_bin="$case_dir/bin"
  log_dir="$case_dir/log"
  mkdir -p "$log_dir"
  make_common_fakes "$fake_bin"

  cat >"$fake_bin/curl" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' '{"tag_name":"v9.8.7"}'
EOF

  cat >"$fake_bin/git" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' "$*" >"$TEST_LOG_DIR/git.args"
for last_arg do :; done
mkdir -p "$last_arg/target/release" "$last_arg/webui"
printf '#!/usr/bin/env sh\nexit 0\n' >"$last_arg/target/release/blockcell"
chmod +x "$last_arg/target/release/blockcell"
EOF

  cat >"$fake_bin/cargo" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF

  cat >"$fake_bin/npm" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF

  chmod +x "$fake_bin/curl" "$fake_bin/git" "$fake_bin/cargo" "$fake_bin/npm"

  PATH="$fake_bin:/usr/bin:/bin" \
  TEST_LOG_DIR="$log_dir" \
  BLOCKCELL_INSTALL_METHOD=source \
  BLOCKCELL_VERSION=latest \
  BLOCKCELL_INSTALL_DIR="$case_dir/install" \
    sh "$ROOT/install.sh" >/dev/null

  grep -F -- '--branch v9.8.7' "$log_dir/git.args" >/dev/null \
    || fail "latest source install did not check out the latest release tag"
}

make_release_fakes() {
  fake_bin="$1"
  make_common_fakes "$fake_bin"

  cat >"$fake_bin/curl" <<'EOF'
#!/usr/bin/env sh
url=""
output=""
previous=""
for arg do
  if [ "$previous" = "-o" ]; then
    output="$arg"
    previous=""
    continue
  fi
  case "$arg" in
    -o) previous="-o" ;;
    http*) url="$arg" ;;
  esac
done

case "$url" in
  */releases/latest)
    printf '%s\n' '{"assets":[{"browser_download_url":"https://github.com/blockcell-labs/blockcell/releases/download/v9.8.7/blockcell-v9.8.7-linux-amd64.tar.gz"}]}'
    ;;
  */blockcell-v9.8.7-linux-amd64.tar.gz)
    printf '%s' 'archive bytes' >"$output"
    ;;
  */checksums.txt)
    printf '%s  %s\n' "$TEST_RELEASE_SHA" 'blockcell-v9.8.7-linux-amd64.tar.gz' >"$output"
    ;;
  *)
    echo "unexpected curl URL: $url" >&2
    exit 1
    ;;
esac
EOF

  cat >"$fake_bin/tar" <<'EOF'
#!/usr/bin/env sh
case " $* " in
  *' -tzf '*) printf '%s\n' "$TEST_TAR_ENTRY" ;;
  *' -tvzf '*) printf '%s\n' "${TEST_TAR_VERBOSE:--rwxr-xr-x user/group 1 2026-01-01 00:00 blockcell}" ;;
  *)
    destination=""
    previous=""
    for arg do
      if [ "$previous" = "-C" ]; then destination="$arg"; previous=""; continue; fi
      [ "$arg" = "-C" ] && previous="-C"
    done
    mkdir -p "$destination"
    printf '#!/usr/bin/env sh\nexit 0\n' >"$destination/blockcell"
    chmod +x "$destination/blockcell"
    ;;
esac
EOF

  chmod +x "$fake_bin/curl" "$fake_bin/tar"
}

run_release_case() {
  name="$1"
  release_sha="$2"
  tar_entry="$3"
  case_dir="$TMP_ROOT/$name"
  fake_bin="$case_dir/bin"
  mkdir -p "$case_dir"
  make_release_fakes "$fake_bin"

  PATH="$fake_bin:/usr/bin:/bin" \
  TEST_RELEASE_SHA="$release_sha" \
  TEST_TAR_ENTRY="$tar_entry" \
  BLOCKCELL_INSTALL_METHOD=release \
  BLOCKCELL_INSTALL_DIR="$case_dir/install" \
    sh "$ROOT/install.sh" >"$case_dir/install.log" 2>&1
}

test_release_rejects_checksum_mismatch() {
  bad_sha=0000000000000000000000000000000000000000000000000000000000000000
  if run_release_case checksum_mismatch "$bad_sha" blockcell; then
    fail "release install accepted an asset whose checksum did not match"
  fi
  grep -F 'SHA-256 checksum mismatch' "$TMP_ROOT/checksum_mismatch/install.log" >/dev/null \
    || fail "checksum rejection did not reach the checksum verification"
}

test_release_rejects_unsafe_archive_paths() {
  if command -v sha256sum >/dev/null 2>&1; then
    archive_sha=$(printf '%s' 'archive bytes' | sha256sum | awk '{print $1}')
  else
    archive_sha=$(printf '%s' 'archive bytes' | shasum -a 256 | awk '{print $1}')
  fi
  if run_release_case unsafe_archive "$archive_sha" '../escaped'; then
    fail "release install accepted a traversing archive entry"
  fi
  grep -F 'Unsafe path in release archive' "$TMP_ROOT/unsafe_archive/install.log" >/dev/null \
    || fail "unsafe archive rejection did not reach archive validation"
}

test_release_installs_verified_asset() {
  if command -v sha256sum >/dev/null 2>&1; then
    archive_sha=$(printf '%s' 'archive bytes' | sha256sum | awk '{print $1}')
  else
    archive_sha=$(printf '%s' 'archive bytes' | shasum -a 256 | awk '{print $1}')
  fi
  run_release_case verified_release "$archive_sha" blockcell \
    || fail "release install rejected a verified asset"
  test -x "$TMP_ROOT/verified_release/install/blockcell" \
    || fail "release install did not commit the verified executable"
}

test_auto_does_not_fallback_after_integrity_failure() {
  case_dir="$TMP_ROOT/auto_integrity"
  fake_bin="$case_dir/bin"
  mkdir -p "$case_dir"
  make_release_fakes "$fake_bin"

  cat >"$fake_bin/git" <<'EOF'
#!/usr/bin/env sh
touch "$TEST_GIT_MARKER"
exit 1
EOF
  chmod +x "$fake_bin/git"

  bad_sha=0000000000000000000000000000000000000000000000000000000000000000
  if PATH="$fake_bin:/usr/bin:/bin" \
    TEST_RELEASE_SHA="$bad_sha" \
    TEST_TAR_ENTRY=blockcell \
    TEST_GIT_MARKER="$case_dir/git-called" \
    BLOCKCELL_INSTALL_METHOD=auto \
    BLOCKCELL_INSTALL_DIR="$case_dir/install" \
      sh "$ROOT/install.sh" >"$case_dir/install.log" 2>&1; then
    fail "auto install accepted or bypassed a release integrity failure"
  fi

  test ! -e "$case_dir/git-called" \
    || fail "auto install fell back to source after a release integrity failure"
  grep -F 'SHA-256 checksum mismatch' "$case_dir/install.log" >/dev/null \
    || fail "auto integrity rejection did not reach checksum verification"
}

test_source_install_checks_out_requested_version_and_replaces_atomically
test_source_install_latest_uses_latest_release_tag
test_release_installs_verified_asset
test_release_rejects_checksum_mismatch
test_release_rejects_unsafe_archive_paths
test_auto_does_not_fallback_after_integrity_failure

python3 -B "$ROOT/tests/test_release_entrypoints.py" InstallerEntrypointTests

echo "install release infrastructure tests: ok"
