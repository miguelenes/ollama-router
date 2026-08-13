#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 4 ]]; then
  echo "usage: pack-pkg.sh <binary> <arch> <version> <outdir>" >&2
  exit 1
fi

BINARY=$1
ARCH=$2
VERSION=$3
OUTDIR=$4
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ ! -f "$BINARY" ]]; then
  echo "missing binary: $BINARY" >&2
  exit 1
fi
if ! command -v pkgbuild >/dev/null 2>&1; then
  echo "pkgbuild not on PATH (macOS only)" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
OUTDIR="$(cd "$OUTDIR" && pwd)"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/usr/local/bin"
mkdir -p "$root/Library/LaunchDaemons"
mkdir -p "$root/Library/Application Support/ollama-node-agent"
cp "$BINARY" "$root/usr/local/bin/ollama-node-agent"
chmod 755 "$root/usr/local/bin/ollama-node-agent"
cp "$SCRIPT_DIR/com.ollama.node-agent.plist" "$root/Library/LaunchDaemons/com.ollama.node-agent.plist"

pkgbuild \
  --root "$root" \
  --identifier com.ollama.node-agent \
  --version "$VERSION" \
  --install-location / \
  --scripts "$SCRIPT_DIR/scripts" \
  "$OUTDIR/ollama-node-agent-${VERSION}-darwin-${ARCH}.pkg"

zipdir="$(mktemp -d)"
cp "$BINARY" "$zipdir/ollama-node-agent"
chmod 755 "$zipdir/ollama-node-agent"
(cd "$zipdir" && zip -q "$OUTDIR/ollama-node-agent-darwin-${ARCH}.zip" ollama-node-agent)
rm -rf "$zipdir"

echo "wrote $OUTDIR/ollama-node-agent-${VERSION}-darwin-${ARCH}.pkg"
echo "wrote $OUTDIR/ollama-node-agent-darwin-${ARCH}.zip"
