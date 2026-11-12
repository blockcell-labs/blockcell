#!/usr/bin/env sh

set -eu

REPO="blockcell-labs/blockcell"
BIN_NAME="blockcell"

INSTALL_DIR_DEFAULT="$HOME/.local/bin"
INSTALL_DIR="${BLOCKCELL_INSTALL_DIR:-$INSTALL_DIR_DEFAULT}"

VERSION="${BLOCKCELL_VERSION:-latest}"
METHOD="${BLOCKCELL_INSTALL_METHOD:-auto}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing required command: $1" 1>&2
    exit 1
  fi
}

install_binary_atomically() {
  source_path="$1"
  mkdir -p "$INSTALL_DIR" || return 1

  temporary_path=$(mktemp "$INSTALL_DIR/.$BIN_NAME.tmp.XXXXXX") || return 1
  if ! cp "$source_path" "$temporary_path"; then
    rm -f "$temporary_path" || true
    return 1
  fi
  if ! chmod +x "$temporary_path"; then
    rm -f "$temporary_path" || true
    return 1
  fi
  if ! mv -f "$temporary_path" "$INSTALL_DIR/$BIN_NAME"; then
    rm -f "$temporary_path" || true
    return 1
  fi
}

detect_os() {
  os_name=$(uname -s) || return 1
  printf '%s\n' "$os_name" | tr '[:upper:]' '[:lower:]'
}

detect_arch() {
  a=$(uname -m) || return 1
  case "$a" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *) echo "$a" ;;
  esac
}

install_source_deps() {
  os="$1"

  if [ "$os" = "linux" ]; then
    if command -v apt-get >/dev/null 2>&1; then
      echo "Installing build dependencies (pkg-config, libssl-dev, libprotobuf-dev, protobuf-compiler, perl, make)..."
      sudo apt-get update -qq
      sudo apt-get install -y pkg-config libssl-dev libprotobuf-dev protobuf-compiler perl make gcc 2>/dev/null || true
    elif command -v yum >/dev/null 2>&1; then
      echo "Installing build dependencies (pkgconfig, openssl-devel, protobuf-devel, protobuf-compiler, perl, make)..."
      sudo yum install -y pkgconfig openssl-devel protobuf-devel protobuf-compiler perl make gcc 2>/dev/null || true
    elif command -v dnf >/dev/null 2>&1; then
      echo "Installing build dependencies (pkgconfig, openssl-devel, protobuf-devel, protobuf-compiler, perl, make)..."
      sudo dnf install -y pkgconfig openssl-devel protobuf-devel protobuf-compiler perl make gcc 2>/dev/null || true
    fi
  elif [ "$os" = "darwin" ]; then
    if command -v brew >/dev/null 2>&1; then
      echo "Installing build dependencies (pkg-config, protobuf)..."
      brew install pkg-config protobuf >/dev/null 2>&1 || true
    fi
  fi
}

github_release_asset_url() {
  require_cmd curl
  require_cmd grep
  require_cmd sed

  os="$1"
  arch="$2"

  suffix="-${os}-${arch}.tar.gz"
  api="https://api.github.com/repos/$REPO/releases"

  if [ "$VERSION" = "latest" ]; then
    json=$(curl -fsSL "$api/latest") || return 1
  else
    json=$(curl -fsSL "$api/tags/$VERSION") || return 1
  fi

  # Keep parser stages separate: POSIX sh has no pipefail, so a final head
  # would otherwise hide grep/sed errors and incorrectly trigger a source build.
  metadata=$(printf '%s\n' "$json" | tr -d '\r\n') || return 1
  if ! printf '%s\n' "$metadata" | grep -Eq '^[[:space:]]*\{.*"assets"[[:space:]]*:[[:space:]]*\[.*\].*\}[[:space:]]*$'; then
    echo "GitHub release metadata does not contain an assets array" 1>&2
    return 1
  fi
  fields_status=0
  url_fields=$(printf '%s\n' "$metadata" \
    | grep -Eo '"browser_download_url"[[:space:]]*:[[:space:]]*"[^"]+"') || fields_status=$?
  if [ "$fields_status" -eq 1 ]; then
    if printf '%s\n' "$metadata" | grep -Eq '"assets"[[:space:]]*:[[:space:]]*\[[[:space:]]*\]'; then
      return 2
    fi
    echo "GitHub release metadata contains no valid asset URLs" 1>&2
    return 1
  elif [ "$fields_status" -ne 0 ]; then
    return 1
  fi
  urls=$(printf '%s\n' "$url_fields" \
    | sed -E 's/^"browser_download_url"[[:space:]]*:[[:space:]]*"(.*)"$/\1/') || return 1
  match_status=0
  matches=$(printf '%s\n' "$urls" | grep -F -- "$suffix") || match_status=$?
  if [ "$match_status" -eq 1 ]; then
    return 2
  elif [ "$match_status" -ne 0 ]; then
    return 1
  fi
  printf '%s\n' "$matches" | head -n 1
}

latest_release_tag() {
  require_cmd curl
  require_cmd grep
  require_cmd sed

  json=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest") || return 1
  tag_fields=$(printf '%s\n' "$json" \
    | grep -Eo '"tag_name"[[:space:]]*:[[:space:]]*"[^"]+"') || return 1
  tags=$(printf '%s\n' "$tag_fields" \
    | sed -E 's/^"tag_name"[[:space:]]*:[[:space:]]*"(.*)"$/\1/') || return 1
  tag=$(printf '%s\n' "$tags" | head -n 1) || return 1
  if [ -z "$tag" ]; then
    echo "Latest GitHub release does not contain a valid tag name" 1>&2
    return 1
  fi
  printf '%s\n' "$tag"
}

install_from_source() {
  require_cmd git
  require_cmd npm

  if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust not found. Installing Rust via rustup..."
    require_cmd curl
    curl https://sh.rustup.rs -sSf | sh -s -- -y
    if [ -f "$HOME/.cargo/env" ]; then
      . "$HOME/.cargo/env"
    fi
  fi

  TMP_DIR=$(mktemp -d) || return 1
  cleanup() {
    rm -rf "$TMP_DIR" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  echo "Cloning https://github.com/$REPO ..."
  if [ "$VERSION" = "latest" ]; then
    source_ref=$(latest_release_tag) || return 1
  else
    source_ref="$VERSION"
  fi
  git clone --depth 1 --branch "$source_ref" "https://github.com/$REPO.git" "$TMP_DIR/blockcell" || return 1

  echo "Testing and building WebUI..."
  (cd "$TMP_DIR/blockcell/webui" && npm ci && npm test && npm run build) || return 1

  echo "Building Rust binary (release)..."
  os=$(detect_os) || return 1
  install_source_deps "$os" || return 1

  # Limit parallel jobs to avoid OOM on low-memory servers (each rustc job can use 1-2 GB)
  total_mem_kb=$(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 0)
  if [ "$total_mem_kb" -gt 0 ] 2>/dev/null; then
    total_mem_gb=$(( total_mem_kb / 1024 / 1024 ))
    if [ "$total_mem_gb" -le 2 ]; then
      build_jobs=1
    elif [ "$total_mem_gb" -le 4 ]; then
      build_jobs=2
    else
      build_jobs=$(nproc 2>/dev/null || echo 4)
    fi
  else
    build_jobs=$(nproc 2>/dev/null || echo 4)
  fi
  echo "Building with $build_jobs parallel job(s)..."
  (cd "$TMP_DIR/blockcell" && cargo build --release --locked -j "$build_jobs") || return 1

  install_binary_atomically "$TMP_DIR/blockcell/target/release/$BIN_NAME" || return 1
}

sha256_file() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha_output=$(sha256sum "$file") || return 1
  elif command -v shasum >/dev/null 2>&1; then
    sha_output=$(shasum -a 256 "$file") || return 1
  else
    echo "Error: sha256sum or shasum is required to verify release assets" 1>&2
    return 1
  fi
  printf '%s\n' "$sha_output" | awk '{print $1}'
}

verify_release_checksum() {
  archive="$1"
  asset="$2"
  checksums="$3"

  expected=$(awk -v asset="$asset" '
    {
      name = $2
      sub(/^\*/, "", name)
      count = split(name, parts, "/")
      if (parts[count] == asset) {
        print $1
        exit
      }
    }
  ' "$checksums") || return 1

  if ! printf '%s\n' "$expected" | grep -Eq '^[0-9a-fA-F]{64}$'; then
    echo "No valid SHA-256 checksum found for release asset: $asset" 1>&2
    return 1
  fi

  actual=$(sha256_file "$archive") || return 1
  expected=$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]') || return 1
  actual=$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]') || return 1
  if [ "$actual" != "$expected" ]; then
    echo "SHA-256 checksum mismatch for release asset: $asset" 1>&2
    return 1
  fi
}

validate_release_archive() {
  archive="$1"

  if ! entries=$(tar -tzf "$archive"); then
    echo "Failed to inspect release archive" 1>&2
    return 1
  fi

  while IFS= read -r entry; do
    normalized=${entry#./}
    case "$normalized" in
      /*|..|../*|*/..|*/../*)
        echo "Unsafe path in release archive: $entry" 1>&2
        return 1
        ;;
    esac
  done <<EOF
$entries
EOF

  if ! verbose_entries=$(tar -tvzf "$archive"); then
    echo "Failed to inspect release archive" 1>&2
    return 1
  fi
  link_status=0
  printf '%s\n' "$verbose_entries" | grep -Eq '^[[:space:]]*[lh]' || link_status=$?
  if [ "$link_status" -eq 0 ]; then
    echo "Release archive contains unsupported links" 1>&2
    return 1
  elif [ "$link_status" -ne 1 ]; then
    return 1
  fi
  return 0
}

install_from_release() {
  require_cmd curl
  require_cmd tar

  os=$(detect_os) || return 1
  arch=$(detect_arch) || return 1

  case "$os" in
    darwin|linux) ;;
    *)
      echo "Release install not supported on OS: $os" 1>&2
      return 2
      ;;
  esac

  case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64) arch="arm64" ;;
  esac

  url_status=0
  url=$(github_release_asset_url "$os" "$arch") || url_status=$?
  if [ "$url_status" -eq 2 ]; then
    echo "Failed to find a matching release asset for OS=$os ARCH=$arch VERSION=$VERSION" 1>&2
    return 2
  elif [ "$url_status" -ne 0 ]; then
    echo "Failed to query GitHub release metadata" 1>&2
    return 1
  fi

  asset=$(basename "$url") || return 1

  TMP_DIR=$(mktemp -d) || return 1
  cleanup() {
    rm -rf "$TMP_DIR" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  echo "Downloading release asset: $url"
  if ! curl -fsSL "$url" -o "$TMP_DIR/$asset"; then
    echo "Failed to download release asset: $asset" 1>&2
    return 1
  fi

  checksums_url="${url%/*}/checksums.txt"
  echo "Downloading release checksums: $checksums_url"
  if ! curl -fsSL "$checksums_url" -o "$TMP_DIR/checksums.txt"; then
    echo "Failed to download release checksums" 1>&2
    return 1
  fi
  verify_release_checksum "$TMP_DIR/$asset" "$asset" "$TMP_DIR/checksums.txt" || return 1
  validate_release_archive "$TMP_DIR/$asset" || return 1

  echo "Extracting..."
  if ! tar -xzf "$TMP_DIR/$asset" -C "$TMP_DIR"; then
    echo "Failed to extract release archive" 1>&2
    return 1
  fi

  if [ ! -f "$TMP_DIR/$BIN_NAME" ]; then
    for candidate in \
      "$TMP_DIR"/*/"$BIN_NAME" \
      "$TMP_DIR"/*/bin/"$BIN_NAME" \
      "$TMP_DIR"/bin/"$BIN_NAME" \
      "$TMP_DIR"/"$BIN_NAME"; do
      if [ -f "$candidate" ] && [ ! -L "$candidate" ]; then
        cp "$candidate" "$TMP_DIR/$BIN_NAME" || return 1
        break
      fi
    done
  fi

  if [ ! -f "$TMP_DIR/$BIN_NAME" ]; then
    echo "Release archive does not contain expected binary: $BIN_NAME" 1>&2
    return 1
  fi

  if [ -L "$TMP_DIR/$BIN_NAME" ]; then
    echo "Release binary must not be a symbolic link" 1>&2
    return 1
  fi

  install_binary_atomically "$TMP_DIR/$BIN_NAME" || return 1
}

echo "Installing $BIN_NAME..."
echo "Repo:    $REPO"
echo "Version: $VERSION"
echo "Method:  $METHOD"
echo "Target:  $INSTALL_DIR/$BIN_NAME"
echo

case "$METHOD" in
  release)
    if ! install_from_release; then
      echo "Release install failed." 1>&2
      exit 1
    fi
    ;;
  source)
    install_from_source
    ;;
  auto)
    release_status=0
    install_from_release || release_status=$?
    if [ "$release_status" -eq 2 ]; then
      echo "Release install not available, falling back to source build..."
      install_from_source
    elif [ "$release_status" -ne 0 ]; then
      echo "Release install failed integrity or download checks; refusing source fallback." 1>&2
      exit 1
    fi
    ;;
  *)
    echo "Invalid BLOCKCELL_INSTALL_METHOD: $METHOD (expected auto|release|source)" 1>&2
    exit 1
    ;;
esac

echo
echo "Installation complete."
echo
echo "Next steps:"
echo "  1) Ensure PATH contains: $INSTALL_DIR"
echo "     e.g.  export PATH=\"$INSTALL_DIR:\$PATH\""
echo "  2) Initialize workspace: blockcell onboard"
echo "  3) Check status:         blockcell status"
echo "  4) Start CLI chat:       blockcell agent"
echo "  5) Start Gateway+WebUI:  blockcell gateway"
echo
