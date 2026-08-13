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
  mkdir -p "$ROOT/$cargo_target_dir" "$ROOT/.local/rustup"
  local -a env_args=(
    -e HOME=/tmp
    -e CARGO_HOME=/src/.local/cargo-home
    -e RUSTUP_HOME=/src/.local/rustup
    -e CARGO_TARGET_DIR="/src/${cargo_target_dir}"
  )
  # Musl portable builds: fully static-pie via rustc self-contained CRT.
  # Do not set musl-gcc as linker — that + -static produces a broken binary.
  if [[ -n "${RUSTFLAGS:-}" ]]; then
    env_args+=(-e "RUSTFLAGS=${RUSTFLAGS}")
  fi
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    "${env_args[@]}" \
    -v "$ROOT":/src \
    -w /src \
    "$IMAGE" \
    "$@"
}

case "$MODE" in
  musl)
    # rust-toolchain.toml remounts a fresh toolchain into RUSTUP_HOME; re-add
    # the musl target. crt-static + link-self-contained so the tarball runs on
    # glibc hosts (pack scripts invoke setup --print-unit).
    RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
    run_in_image .local/target-agent-linux-musl \
      bash -euo pipefail -c "
        rustup target add ${MUSL_TARGET}
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
