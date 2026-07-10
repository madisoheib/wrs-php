#!/bin/sh
# Resonance installer — one-liner for any project, Laravel or not:
#   curl -sSL https://raw.githubusercontent.com/madisoheib/wrs-php/main/install.sh | sh
# Detects OS/arch, downloads the matching static binary from GitHub Releases,
# verifies its SHA-256, installs to ./bin/resonance (or $RESONANCE_INSTALL_DIR).
set -eu

REPO="madisoheib/wrs-php"
VERSION="${RESONANCE_VERSION:-latest}"
DIR="${RESONANCE_INSTALL_DIR:-./bin}"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux)  os_part="unknown-linux-musl" ;;
  Darwin) os_part="apple-darwin" ;;
  *) echo "Unsupported OS: $os (Windows: download the .exe from https://github.com/$REPO/releases)"; exit 1 ;;
esac
case "$arch" in
  x86_64|amd64)  arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) echo "Unsupported architecture: $arch"; exit 1 ;;
esac

asset="resonance-${arch_part}-${os_part}"
if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

echo "Downloading $asset ($VERSION)..."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$asset" "$base/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256"

echo "Verifying checksum..."
expected=$(awk '{print $1}' "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
else
  actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
fi
[ "$expected" = "$actual" ] || { echo "Checksum mismatch — aborting."; exit 1; }

mkdir -p "$DIR"
mv "$tmp/$asset" "$DIR/resonance"
chmod +x "$DIR/resonance"

echo "Installed: $DIR/resonance"
"$DIR/resonance" --help >/dev/null 2>&1 && echo "OK — run: $DIR/resonance start --config resonance.toml"
