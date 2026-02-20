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

detect_os() {
  uname -s | tr '[:upper:]' '[:lower:]'
}

detect_arch() {
  a=$(uname -m)
  case "$a" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *) echo "$a" ;;
  esac
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
    json=$(curl -fsSL "$api/latest")
  else
    json=$(curl -fsSL "$api/tags/$VERSION")
  fi

  echo "$json" \
    | grep -Eo '"browser_download_url"\s*:\s*"[^"]+"' \
    | sed -E 's/^"browser_download_url"\s*:\s*"(.*)"$/\1/' \
    | grep -F "$suffix" \
    | head -n 1
}

install_from_source() {
  require_cmd git

  if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust not found. Installing Rust via rustup..."
    require_cmd curl
    curl https://sh.rustup.rs -sSf | sh -s -- -y
    if [ -f "$HOME/.cargo/env" ]; then
      . "$HOME/.cargo/env"
    fi
  fi

  TMP_DIR=$(mktemp -d)
  cleanup() {
    rm -rf "$TMP_DIR" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  echo "Cloning https://github.com/$REPO ..."
  git clone --depth 1 "https://github.com/$REPO.git" "$TMP_DIR/blockcell"

  echo "Building (release)..."
  os=$(detect_os)
  arch=$(detect_arch)
  target=""
  if [ "$os" = "linux" ]; then
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      aarch64) target="aarch64-unknown-linux-musl" ;;
    esac
  fi

  if [ -n "$target" ]; then
    if command -v rustup >/dev/null 2>&1; then
      rustup target add "$target" >/dev/null 2>&1 || true
    fi
    (cd "$TMP_DIR/blockcell" \
      && export RUSTFLAGS="-C target-feature=+crt-static" \
      && export OPENSSL_VENDORED=1 \
      && export OPENSSL_STATIC=1 \
      && cargo build --release --target "$target")
  else
    (cd "$TMP_DIR/blockcell" && cargo build --release)
  fi

  mkdir -p "$INSTALL_DIR"
  if [ -n "$target" ]; then
    cp "$TMP_DIR/blockcell/target/$target/release/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
  else
    cp "$TMP_DIR/blockcell/target/release/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
  fi
  chmod +x "$INSTALL_DIR/$BIN_NAME" || true
}

install_from_release() {
  require_cmd curl
  require_cmd tar

  os=$(detect_os)
  arch=$(detect_arch)

  case "$os" in
    darwin|linux) ;;
    *)
      echo "Release install not supported on OS: $os" 1>&2
      return 1
      ;;
  esac

  case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64) arch="arm64" ;;
  esac

  url=$(github_release_asset_url "$os" "$arch")
  if [ -z "${url:-}" ]; then
    echo "Failed to find a matching release asset for OS=$os ARCH=$arch VERSION=$VERSION" 1>&2
    return 1
  fi

  asset=$(basename "$url")

  TMP_DIR=$(mktemp -d)
  cleanup() {
    rm -rf "$TMP_DIR" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  echo "Downloading release asset: $url"
  if ! curl -fsSL "$url" -o "$TMP_DIR/$asset"; then
    echo "Failed to download release asset: $asset" 1>&2
    return 1
  fi

  echo "Extracting..."
  tar -xzf "$TMP_DIR/$asset" -C "$TMP_DIR"

  if [ ! -f "$TMP_DIR/$BIN_NAME" ]; then
    if [ -f "$TMP_DIR/blockcell" ]; then
      :
    elif [ -f "$TMP_DIR/bin/$BIN_NAME" ]; then
      cp "$TMP_DIR/bin/$BIN_NAME" "$TMP_DIR/$BIN_NAME"
    elif [ -f "$TMP_DIR/blockcell/bin/$BIN_NAME" ]; then
      cp "$TMP_DIR/blockcell/bin/$BIN_NAME" "$TMP_DIR/$BIN_NAME"
    fi
  fi

  if [ ! -f "$TMP_DIR/$BIN_NAME" ]; then
    echo "Release archive does not contain expected binary: $BIN_NAME" 1>&2
    return 1
  fi

  mkdir -p "$INSTALL_DIR"
  cp "$TMP_DIR/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
  chmod +x "$INSTALL_DIR/$BIN_NAME" || true
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
    if ! install_from_release; then
      echo "Release install not available, falling back to source build..."
      install_from_source
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
