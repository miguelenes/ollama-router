#!/usr/bin/env bash
# Wrap the official zrok-instance compose fetched into .local/zrok.
# Tokens stay in .local/zrok/.env (gitignored). Do not scrape :11436.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
COMPOSE_WRAPPER="${ROOT}/deploy/compose.zrok.yaml"
FETCHED="${ROOT}/.local/zrok/compose.yml"
ENV_FILE="${ROOT}/.local/zrok/.env"

if [[ ! -f "${FETCHED}" ]]; then
  echo "missing ${FETCHED} — run: task zrok:fetch" >&2
  exit 1
fi

env_args=()
if [[ -f "${ENV_FILE}" ]]; then
  env_args=(--env-file "${ENV_FILE}")
fi

cmd=${1:-ps}
shift || true

if docker compose -f "${COMPOSE_WRAPPER}" "${env_args[@]}" config >/dev/null 2>&1; then
  exec docker compose -f "${COMPOSE_WRAPPER}" "${env_args[@]}" "${cmd}" "$@"
fi

echo "compose include wrapper unavailable; using fetched compose.yml" >&2
exec docker compose -f "${FETCHED}" --project-directory "${ROOT}/.local/zrok" \
  "${env_args[@]}" "${cmd}" "$@"
