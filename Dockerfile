# Multi-stage image for the Ollama fleet proxy, the compose mock, and the node
# agent. Default target is `router` (last stage):
#   docker build -t ollama-router:local .
# Mock:
#   docker build --target mock -t ollama-mock:local .
# Node agent (was Dockerfile.agent; now a target here):
#   docker build --target agent -t ollama-node-agent:local .
# Bake targets are declared in docker-bake.hcl (router / mock / agent).
#
# Listen :11434 inside the router container; :11436 inside the agent container.
# Compose publishes host 11435 → 11434. rust-toolchain.toml is dockerignored;
# this image tag is the rustc pin.

FROM rust:1.97.1-slim-bookworm AS chef
WORKDIR /app
RUN cargo install cargo-chef --locked --version 0.1.78

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin ollama-router --bin ollama-mock --bin ollama-node-agent

FROM debian:bookworm-slim AS mock
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 mock
COPY --from=builder /app/target/release/ollama-mock /usr/local/bin/
USER mock
EXPOSE 11434
ENV OLLAMA_MOCK_PORT=11434
CMD ["ollama-mock"]

FROM debian:bookworm-slim AS agent
RUN apt-get update -qq \
    && apt-get install -y -qq --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 agent \
    && mkdir -p /etc/ollama-node-agent \
    && chown agent:agent /etc/ollama-node-agent
COPY --from=builder /app/target/release/ollama-node-agent /usr/local/bin/
COPY crates/ollama-node-agent/config.docker.yaml /etc/ollama-node-agent/config.yaml
USER agent
EXPOSE 11436
ENV OLLAMA_NODE_AGENT_HOST=127.0.0.1 \
    OLLAMA_NODE_AGENT_PORT=11436
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:11436/healthz || exit 1
CMD ["ollama-node-agent", "serve", "--config", "/etc/ollama-node-agent/config.yaml"]

FROM debian:bookworm-slim AS router
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
