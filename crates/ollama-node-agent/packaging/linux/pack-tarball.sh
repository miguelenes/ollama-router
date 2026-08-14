#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: pack-tarball.sh <binary> <arch> <outdir>" >&2
  exit 1
fi

BINARY=$1
ARCH=$2
OUTDIR=$3
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CANONICAL_UNIT="$SCRIPT_DIR/ollama-node-agent.service"

if [[ ! -f "$BINARY" ]]; then
  echo "missing binary: $BINARY" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
OUTDIR="$(cd "$OUTDIR" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

cp "$BINARY" "$stage/ollama-node-agent"
chmod 755 "$stage/ollama-node-agent"
strip --strip-unneeded "$stage/ollama-node-agent"
chmod 755 "$stage/ollama-node-agent"

"$stage/ollama-node-agent" setup --print-unit > "$stage/ollama-node-agent.service"
if ! cmp -s "$CANONICAL_UNIT" "$stage/ollama-node-agent.service"; then
  echo "unit text from setup --print-unit drifted from $CANONICAL_UNIT" >&2
  exit 1
fi

cp "$SCRIPT_DIR/README.portable.md" "$stage/README.md"
cp "$SCRIPT_DIR/ollama-node-agent-tunnel.service" "$stage/ollama-node-agent-tunnel.service"
mkdir -p "$stage/contrib/openrc"
cp "$SCRIPT_DIR/openrc/ollama-node-agent" "$stage/contrib/openrc/ollama-node-agent"
chmod 755 "$stage/contrib/openrc/ollama-node-agent"

tar -C "$stage" -czf "$OUTDIR/ollama-node-agent-linux-${ARCH}.tar.gz" \
  ollama-node-agent ollama-node-agent.service ollama-node-agent-tunnel.service README.md contrib
echo "wrote $OUTDIR/ollama-node-agent-linux-${ARCH}.tar.gz"
