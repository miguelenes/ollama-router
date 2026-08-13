#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: pack-msi.sh <exe> <version> <outdir>" >&2
  exit 1
fi

EXE=$1
VERSION=$2
OUTDIR=$3
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ ! -f "$EXE" ]]; then
  echo "missing exe: $EXE" >&2
  exit 1
fi
if ! command -v wix >/dev/null 2>&1; then
  echo "wix not on PATH; install with: dotnet tool install --global wix --version 5.0.2" >&2
  exit 1
fi

CONFIG="$SCRIPT_DIR/config.yaml"
if [[ ! -f "$CONFIG" ]]; then
  echo "missing config: $CONFIG" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
OUTDIR="$(cd "$OUTDIR" && pwd)"
# WiX Version must be 1-4 numeric components (no leading v).
wix build \
  -d "ExePath=$EXE" \
  -d "ConfigPath=$CONFIG" \
  -d "ProductVersion=$VERSION" \
  "$SCRIPT_DIR/ollama-node-agent.wxs" \
  -o "$OUTDIR/ollama-node-agent-${VERSION}-windows-amd64.msi"
echo "wrote $OUTDIR/ollama-node-agent-${VERSION}-windows-amd64.msi"
