# Multi-stage image for the Ollama fleet proxy.
# Build:  docker build -t ollama-router:local .
# Run:    docker run --rm -p 11434:11434 ollama-router:local
#
# Listen :11434 inside the container. Illumination Sail publishes host 11435
# (`FORWARD_OLLAMA_ROUTER_PORT`, default 11435) — do not collide with host Ollama.

FROM rust:1.97-slim-bookworm AS builder
WORKDIR /app
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin ollama-router

FROM debian:bookworm-slim
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 router \
    && mkdir -p /var/lib/ollama-router \
    && chown router:router /var/lib/ollama-router
COPY --from=builder /app/target/release/ollama-router /usr/local/bin/
USER router
EXPOSE 11434
ENV OLLAMA_ROUTER_HOST=0.0.0.0 \
    OLLAMA_ROUTER_PORT=11434
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:11434/healthz || exit 1
CMD ["ollama-router", "serve"]
