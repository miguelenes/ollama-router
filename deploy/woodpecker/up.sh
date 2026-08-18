#!/usr/bin/env bash
# Start Woodpecker compose and ensure Tailscale Funnel proxies host :8000.
# Funnel is host-level (tailscale CLI), not a Compose service. Never commit .env.
set -euo pipefail

DIR=$(cd "$(dirname "$0")" && pwd)
ENV_FILE="${DIR}/.env"
EXAMPLE="${DIR}/.env.example"
COMPOSE=(docker compose -f "${DIR}/compose.yaml" --env-file "${ENV_FILE}")

if [[ ! -f "${ENV_FILE}" ]]; then
  if [[ -f "${EXAMPLE}" ]]; then
    cp "${EXAMPLE}" "${ENV_FILE}"
    echo "wrote ${ENV_FILE} from .env.example — fill WOODPECKER_* (never commit) and re-run" >&2
  else
    echo "missing ${ENV_FILE}" >&2
  fi
  exit 1
fi

funnel_url() {
  local dns
  dns=$(tailscale status --json | python3 -c 'import json, sys
d = json.load(sys.stdin)
print(((d.get("Self") or {}).get("DNSName") or "").rstrip("."))
')
  if [[ -z "${dns}" ]]; then
    echo "could not resolve Tailscale MagicDNS name from tailscale status --json" >&2
    exit 1
  fi
  printf 'https://%s\n' "${dns}"
}

env_webhook_host() {
  local line
  line=$(grep -E '^WOODPECKER_EXPERT_WEBHOOK_HOST=' "${ENV_FILE}" | tail -n1 || true)
  line=${line#WOODPECKER_EXPERT_WEBHOOK_HOST=}
  line=${line%$'\r'}
  line=${line#\"}
  line=${line%\"}
  line=${line#\'}
  line=${line%\'}
  printf '%s\n' "${line}"
}

running_webhook_host() {
  local cid
  cid=$("${COMPOSE[@]}" ps -q woodpecker-server 2>/dev/null || true)
  if [[ -z "${cid}" ]]; then
    return 0
  fi
  docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "${cid}" |
    sed -n 's/^WOODPECKER_EXPERT_WEBHOOK_HOST=//p' | tail -n1
}

FUNNEL_URL=$(funnel_url)
PINNED=$(env_webhook_host)
WEBHOOK_HOST=${PINNED:-${FUNNEL_URL}}

if [[ -z "${PINNED}" ]]; then
  echo "WOODPECKER_EXPERT_WEBHOOK_HOST unset; using ${WEBHOOK_HOST} for this invocation (pin it in .env)" >&2
fi

export WOODPECKER_EXPERT_WEBHOOK_HOST="${WEBHOOK_HOST}"

CURRENT=$(running_webhook_host || true)
if [[ -n "${CURRENT}" && "${CURRENT}" != "${WEBHOOK_HOST}" ]]; then
  echo "webhook host changed (${CURRENT} -> ${WEBHOOK_HOST}); recreating woodpecker-server" >&2
  "${COMPOSE[@]}" up -d --force-recreate --no-deps woodpecker-server
fi

"${COMPOSE[@]}" up -d

# --set-path=/ updates only Woodpecker. Do not omit it (that can reset /router).
if ! funnel_out=$(tailscale funnel --bg --yes --set-path=/ 8000 2>&1); then
  printf '%s\n' "${funnel_out}" >&2
  echo "Tailscale Funnel failed. Enable it on the tailnet (login.tailscale.com/f/funnel) and retry. Do not use Cloudflare or trycloudflare." >&2
  exit 1
fi
printf '%s\n' "${funnel_out}"

echo "Woodpecker UI (loopback OAuth): http://127.0.0.1:8000/"
echo "Public Funnel (webhooks + UI): ${FUNNEL_URL}/"
echo "Compose still binds :8000 on loopback only."
