#!/usr/bin/env bash
# Live cloud autoscale fill-and-idle test (Verda + RunPod).
# Requires: router with cloud overlay deployed, admin bearer, guest-reachable
# enroll_url + zrok controller. Never logs prompts, tokens, or share secrets.
set -euo pipefail

ROUTER_URL="${ROUTER_URL:-http://127.0.0.1:11434}"
MODEL="${MODEL:-llama3.1:8b}"
ADMIN_TOKEN="${OLLAMA_ROUTER_ADMIN_TOKEN:-}"
DRAIN_NUC="${DRAIN_NUC:-true}"
CONCURRENT="${CONCURRENT:-8}"
NUM_PREDICT="${NUM_PREDICT:-512}"
IDLE_WAIT_SECONDS="${IDLE_WAIT_SECONDS:-420}"

if [[ -z "$ADMIN_TOKEN" ]]; then
  echo "Set OLLAMA_ROUTER_ADMIN_TOKEN (admin bearer)." >&2
  exit 1
fi

auth=(-H "Authorization: Bearer ${ADMIN_TOKEN}")

phase0() {
  echo "== Phase 0: gates =="
  curl -fsS "${ROUTER_URL}/healthz" >/dev/null
  echo "healthz ok"

  for provider in verda runpod; do
    if [[ "$provider" == "verda" && "${VERDA_SKIP:-false}" == "true" ]]; then
      echo "verda/status skipped (VERDA_SKIP=true)"
      continue
    fi
    code=$(curl -sS -o /tmp/"${provider}"-status.json -w '%{http_code}' \
      "${auth[@]}" "${ROUTER_URL}/router/v1/${provider}/status" || true)
    echo "${provider}/status HTTP ${code}"
    if [[ "$code" != "200" ]]; then
      cat /tmp/"${provider}"-status.json >&2 || true
      echo "Enable ${provider} in OLLAMA_ROUTER_CONFIG and redeploy." >&2
      exit 1
    fi
  done

  : "${GUEST_ENROLL_URL:?Set GUEST_ENROLL_URL to runpod.enroll_url from the overlay (guest-reachable http(s), not loopback)}"
  : "${GUEST_ZROK_API_ENDPOINT:?Set GUEST_ZROK_API_ENDPOINT to tunnel.api_endpoint from the overlay (guest-reachable http(s), not loopback)}"

  python3 - <<'PY' "$GUEST_ENROLL_URL" "$GUEST_ZROK_API_ENDPOINT"
import sys, urllib.parse
def check(label, raw):
    u = raw.strip()
    if not (u.startswith("http://") or u.startswith("https://")):
        raise SystemExit(f"{label} must be http(s): {u!r}")
    host = (urllib.parse.urlparse(u).hostname or "").lower()
    if host in ("127.0.0.1", "localhost", "::1") or host.startswith("["):
        if host in ("127.0.0.1", "localhost", "::1", "[::1]"):
            raise SystemExit(f"{label} must not be loopback: {u!r}")
    for prefix in ("10.", "192.168.", "172.16.", "172.17.", "172.18.", "172.19.",
                   "172.20.", "172.21.", "172.22.", "172.23.", "172.24.", "172.25.",
                   "172.26.", "172.27.", "172.28.", "172.29.", "172.30.", "172.31."):
        if host.startswith(prefix):
            raise SystemExit(f"{label} must be guest-reachable (not RFC1918): {u!r}")
    print(f"{label} ok (guest-reachable http(s))")
check("enroll_url", sys.argv[1])
check("zrok_api", sys.argv[2])
PY

  if [[ -n "${AGENT_PACKAGE_URL:-}" ]]; then
    code=$(curl -sS -o /dev/null -w '%{http_code}' -I "${AGENT_PACKAGE_URL}" || true)
    echo "agent package HEAD HTTP ${code}"
    if [[ "$code" == "404" ]] || [[ "$code" == "000" ]]; then
      echo "AGENT_PACKAGE_URL must not 404 before minting pods: ${AGENT_PACKAGE_URL}" >&2
      exit 1
    fi
  else
    echo "AGENT_PACKAGE_URL unset — set to a guest-reachable .deb/tarball if GitHub release assets 404" >&2
  fi

  curl -fsS "${auth[@]}" "${ROUTER_URL}/router/v1/nodes" | python3 - <<'PY'
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    print("nodes: empty response")
    sys.exit(0)
d = json.loads(raw)
nodes = d.get("nodes", d if isinstance(d, list) else [])
cloud = [n for n in nodes if n.get("origin") in ("verda", "runpod")]
print(f"fleet nodes: {len(nodes)}, cloud: {len(cloud)}")
PY
}

drain_nuc() {
  if [[ "$DRAIN_NUC" != "true" ]]; then
    return 0
  fi
  echo "== Phase 2: drain nuc =="
  curl -fsS -X POST "${auth[@]}" \
    "${ROUTER_URL}/router/v1/nodes/nuc/drain" >/dev/null
  echo "nuc cordoned"
}

undrain_nuc() {
  if [[ "$DRAIN_NUC" != "true" ]]; then
    return 0
  fi
  echo "== Restore: undrain nuc =="
  curl -fsS -X POST "${auth[@]}" \
    "${ROUTER_URL}/router/v1/nodes/nuc/undrain" >/dev/null || true
}

miss_probe() {
  echo "== Phase 3a: capacity miss =="
  code=$(curl -sS -o /tmp/miss.json -w '%{http_code}' \
    -X POST "${ROUTER_URL}/api/generate" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"${MODEL}\",\"prompt\":\"hi\",\"stream\":false,\"num_predict\":1}" || true)
  ra=$(curl -sSI -X POST "${ROUTER_URL}/api/generate" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"${MODEL}\",\"prompt\":\"hi\",\"stream\":false,\"num_predict\":1}" \
    2>/dev/null | awk -F': ' 'tolower($1)=="retry-after"{print $2}' | tr -d '\r' || true)
  echo "generate HTTP ${code} Retry-After=${ra:-none}"
  python3 -c 'import json; print(json.load(open("/tmp/miss.json")))' 2>/dev/null || cat /tmp/miss.json
}

metrics_cloud() {
  curl -fsS "${ROUTER_URL}/metrics" | rg 'ollama_router_cloud_(instances|events_total|price_per_hour)|ollama_router_inflight' || true
}

wait_cloud_healthy() {
  echo "== Phase 3b: wait for cloud node healthy (max 20m) =="
  local i=0
  while (( i < 120 )); do
    if curl -fsS "${ROUTER_URL}/metrics" | rg -q 'ollama_router_cloud_instances\{provider="(verda|runpod)"\} [1-9]'; then
      metrics_cloud
      return 0
    fi
    sleep 10
    (( i++ )) || true
  done
  echo "No cloud instance registered within timeout." >&2
  metrics_cloud
  return 1
}

load_fill() {
  echo "== Phase 3c: concurrent streaming load (${CONCURRENT} x ${MODEL}) =="
  local pids=()
  for ((i=0; i<CONCURRENT; i++)); do
    curl -sS -N -X POST "${ROUTER_URL}/api/generate" \
      -H 'Content-Type: application/json' \
      -d "{\"model\":\"${MODEL}\",\"prompt\":\"load\",\"stream\":true,\"num_predict\":${NUM_PREDICT}}" \
      >/dev/null &
    pids+=($!)
  done
  sleep 3
  metrics_cloud
  curl -fsS "${ROUTER_URL}/metrics" | rg 'ollama_router_inflight' || true

  echo "== Phase 3d: saturation probe =="
  miss_probe || true

  for pid in "${pids[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
}

idle_wait() {
  echo "== Phase 5: idle teardown wait (${IDLE_WAIT_SECONDS}s) =="
  metrics_cloud
  sleep "$IDLE_WAIT_SECONDS"
  metrics_cloud
  curl -fsS "${auth[@]}" "${ROUTER_URL}/router/v1/verda/status" || true
  echo
  curl -fsS "${auth[@]}" "${ROUTER_URL}/router/v1/runpod/status" || true
  echo
}

abort_destroy() {
  echo "== Abort destroy (if needed) =="
  curl -fsS -X POST "${auth[@]}" "${ROUTER_URL}/router/v1/verda/destroy" || true
  curl -fsS -X POST "${auth[@]}" "${ROUTER_URL}/router/v1/runpod/destroy" || true
}

trap 'undrain_nuc' EXIT

phase0
drain_nuc
miss_probe
wait_cloud_healthy || { abort_destroy; exit 1; }
load_fill
idle_wait
undrain_nuc
trap - EXIT

echo "Done. Confirm ollama_router_cloud_instances == 0 and provider consoles show terminate/delete."
