#!/usr/bin/env bash
# Re-apply guest Funnel paths on desktop-pc without resetting Woodpecker `/`.
#
# HTTPS 443:
#   /        → Woodpecker :8000  (owned by deploy/woodpecker/up.sh)
#   /router  → router :11434     (enroll + admin; Funnel strips /router)
#   /agent   → dist/agent files  (optional; needs task agent:release:deb)
# HTTPS 8443 → zrok controller :18080 (only if that port is listening)
# TCP 10000  → OpenZiti router :3022 (only if that port is listening)
#
# Never `tailscale funnel reset` (wipes Woodpecker). Never Funnel `/api` or
# the whole Ollama listen — those routes are unauthenticated.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
FUNNEL_HOST="${FUNNEL_HOST:-desktop.bicorn-beta.ts.net}"
AGENT_DIR="${AGENT_DIR:-${ROOT}/dist/agent}"
TIMEOUT_SEC="${FUNNEL_TIMEOUT_SEC:-20}"

funnel() {
  # ACL misses hang the CLI on a login.tailscale.com/f/funnel URL.
  timeout "${TIMEOUT_SEC}" tailscale funnel --bg --yes "$@"
}

echo "Keeping Woodpecker / → :8000 (do not funnel reset)" >&2
if ! funnel --set-path=/router http://127.0.0.1:11434; then
  echo "Funnel /router failed. tag:compute (or tag:ops) needs attr funnel in the tailnet ACL." >&2
  exit 1
fi

if [[ -d "${AGENT_DIR}" ]] && ls "${AGENT_DIR}"/*.deb >/dev/null 2>&1; then
  if ! funnel --set-path=/agent "${AGENT_DIR}"; then
    echo "Funnel /agent failed (deb still in ${AGENT_DIR}; serve later)" >&2
  fi
else
  echo "No ${AGENT_DIR}/*.deb — run: task agent:release:deb" >&2
fi

if ss -ltn | grep -q ':18080 '; then
  funnel --https=8443 http://127.0.0.1:18080 || echo "Funnel 8443 (zrok API) skipped" >&2
else
  echo "zrok :18080 not listening — skip Funnel 8443 (task zrok:up after advertised addresses)" >&2
fi

if ss -ltn | grep -q ':3022 '; then
  funnel --tcp=10000 tcp://127.0.0.1:3022 || echo "Funnel TCP 10000 (ziti 3022) skipped" >&2
else
  echo "ziti :3022 not listening — skip Funnel TCP 10000" >&2
fi

tailscale funnel status

ENROLL="https://${FUNNEL_HOST}/router/router/v1/nodes/enroll"
echo "Probing ${ENROLL} (Funnel strips --set-path=/router)" >&2
code=$(curl -sS -o /tmp/funnel-guest-enroll.body -w '%{http_code}' \
  -X POST -H 'Content-Type: application/json' -d '{}' "${ENROLL}" || true)
# 401 missing/invalid bearer, 403 admin disabled, 422 missing enroll fields — all router.
case "${code}" in
  401|403|422)
    echo "enroll Funnel ok (HTTP ${code})" >&2
    ;;
  *)
    echo "enroll Funnel unexpected HTTP ${code} (Woodpecker is 200 HTML):" >&2
    head -c 240 /tmp/funnel-guest-enroll.body; echo >&2
    exit 1
    ;;
esac
echo "Do not enable RunPod until zrok API + ziti 3022 also succeed from a guest network." >&2
