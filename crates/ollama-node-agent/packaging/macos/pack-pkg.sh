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
CANONICAL_PLIST="$SCRIPT_DIR/com.ollama.node-agent.plist"
CONFIG="$SCRIPT_DIR/config.yaml"

if [[ ! -f "$BINARY" ]]; then
  echo "missing binary: $BINARY" >&2
  exit 1
fi
if [[ ! -f "$CANONICAL_PLIST" ]]; then
  echo "missing plist: $CANONICAL_PLIST" >&2
  exit 1
fi
if [[ ! -f "$CONFIG" ]]; then
  echo "missing config: $CONFIG" >&2
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
strip "$root/usr/local/bin/ollama-node-agent"
chmod 755 "$root/usr/local/bin/ollama-node-agent"

cp "$CANONICAL_PLIST" "$root/Library/LaunchDaemons/com.ollama.node-agent.plist"
cp "$SCRIPT_DIR/com.ollama.node-agent.tunnel.plist" \
  "$root/Library/LaunchDaemons/com.ollama.node-agent.tunnel.plist"
if ! cmp -s "$CANONICAL_PLIST" "$root/Library/LaunchDaemons/com.ollama.node-agent.plist"; then
  echo "staged plist drifted from $CANONICAL_PLIST" >&2
  exit 1
fi
cp "$CONFIG" "$root/Library/Application Support/ollama-node-agent/config.yaml"

zipdir="$(mktemp -d)"
cp "$root/usr/local/bin/ollama-node-agent" "$zipdir/ollama-node-agent"
chmod 755 "$zipdir/ollama-node-agent"
(cd "$zipdir" && zip -q "$OUTDIR/ollama-node-agent-darwin-${ARCH}.zip" ollama-node-agent)
rm -rf "$zipdir"
echo "wrote $OUTDIR/ollama-node-agent-darwin-${ARCH}.zip"

if ! command -v pkgbuild >/dev/null 2>&1; then
  echo "skipping Darwin pkg: pkgbuild not on PATH (GHA macos-14 is canonical)" >&2
  exit 0
fi

pkgbuild \
  --root "$root" \
  --identifier com.ollama.node-agent \
  --version "$VERSION" \
  --install-location / \
  --scripts "$SCRIPT_DIR/scripts" \
  "$OUTDIR/ollama-node-agent-${VERSION}-darwin-${ARCH}.pkg"

echo "wrote $OUTDIR/ollama-node-agent-${VERSION}-darwin-${ARCH}.pkg"
