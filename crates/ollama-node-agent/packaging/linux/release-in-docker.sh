#!/usr/bin/env bash
# Local Linux release inside rust:1.97-slim-bookworm. GHA does not use this file.
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: release-in-docker.sh musl|gnu <arch> <outdir> [version]" >&2
  exit 1
fi

MODE=$1
ARCH=$2
OUTDIR=$3
VERSION=${4:-}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
IMAGE=ollama-node-agent-release:local

case "$ARCH" in
  amd64) MUSL_TARGET=x86_64-unknown-linux-musl ;;
  arm64) MUSL_TARGET=aarch64-unknown-linux-musl ;;
  *) echo "unsupported arch $ARCH" >&2; exit 1 ;;
esac

docker build -f "$SCRIPT_DIR/Dockerfile.release" -t "$IMAGE" "$SCRIPT_DIR"

mkdir -p "$ROOT/.local/cargo-home" "$ROOT/$OUTDIR"

run_in_image() {
  local cargo_target_dir=$1
  shift
  mkdir -p "$ROOT/$cargo_target_dir"
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -e CARGO_HOME=/src/.local/cargo-home \
    -e CARGO_TARGET_DIR="/src/${cargo_target_dir}" \
    -e CC_x86_64_unknown_linux_musl=musl-gcc \
    -e CC_aarch64_unknown_linux_musl=musl-gcc \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    -v "$ROOT":/src \
    -w /src \
    "$IMAGE" \
    "$@"
}

case "$MODE" in
  musl)
    run_in_image .local/target-agent-linux-musl \
      bash -euo pipefail -c "
        cargo build --release --locked -p ollama-node-agent --target ${MUSL_TARGET}
        bash crates/ollama-node-agent/packaging/linux/pack-tarball.sh \
          .local/target-agent-linux-musl/${MUSL_TARGET}/release/ollama-node-agent \
          ${ARCH} ${OUTDIR}
      "
    ;;
  gnu)
    if [[ -z "$VERSION" ]]; then
      echo "gnu mode requires version" >&2
      exit 1
    fi
    run_in_image .local/target-agent-linux-gnu \
      bash -euo pipefail -c "
        cargo build --release --locked -p ollama-node-agent
        bash crates/ollama-node-agent/packaging/linux/pack-deb.sh \
          .local/target-agent-linux-gnu/release/ollama-node-agent \
          ${ARCH} ${VERSION} ${OUTDIR}
      "
    ;;
  *)
    echo "mode must be musl or gnu" >&2
    exit 1
    ;;
esac

bash "$SCRIPT_DIR/write-checksums.sh" "$ROOT/$OUTDIR"
