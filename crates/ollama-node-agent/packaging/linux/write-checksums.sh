#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: write-checksums.sh <outdir>" >&2
  exit 1
fi

OUTDIR=$1
if [[ ! -d "$OUTDIR" ]]; then
  echo "missing outdir: $OUTDIR" >&2
  exit 1
fi
OUTDIR="$(cd "$OUTDIR" && pwd)"
cd "$OUTDIR"

shopt -s nullglob
files=(ollama-node-agent-linux-*.tar.gz ollama-node-agent_*.deb)
if [[ ${#files[@]} -eq 0 ]]; then
  echo "no linux artifacts in $OUTDIR" >&2
  exit 1
fi
sha256sum -- "${files[@]}" > SHA256SUMS.txt
echo "wrote $OUTDIR/SHA256SUMS.txt"
