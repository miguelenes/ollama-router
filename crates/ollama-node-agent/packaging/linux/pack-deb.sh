#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 4 ]]; then
  echo "usage: pack-deb.sh <binary> <arch> <version> <outdir>" >&2
  exit 1
fi

BINARY=$1
ARCH=$2
VERSION=$3
OUTDIR=$4
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CANONICAL_UNIT="$SCRIPT_DIR/ollama-node-agent.service"

if [[ ! -f "$BINARY" ]]; then
  echo "missing binary: $BINARY" >&2
  exit 1
fi
if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm not on PATH; install from https://github.com/goreleaser/nfpm/releases" >&2
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

cp "$SCRIPT_DIR/nfpm.yaml" "$stage/nfpm.yaml"
cp "$SCRIPT_DIR/config.yaml" "$stage/config.yaml"
cp "$SCRIPT_DIR/ollama-node-agent-tunnel.service" "$stage/ollama-node-agent-tunnel.service"
cp "$SCRIPT_DIR/postinst.sh" "$stage/postinst.sh"
cp "$SCRIPT_DIR/prerm.sh" "$stage/prerm.sh"
chmod 755 "$stage/postinst.sh" "$stage/prerm.sh"

export AGENT_VERSION="$VERSION"
export AGENT_ARCH="$ARCH"
(cd "$stage" && nfpm package -f nfpm.yaml -p deb -t "$OUTDIR/ollama-node-agent_${VERSION}_${ARCH}.deb")
echo "wrote $OUTDIR/ollama-node-agent_${VERSION}_${ARCH}.deb"
