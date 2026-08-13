#!/usr/bin/env bash
# Idempotent Ubuntu/Debian provisioner for an ollama-router GPU/CPU node.
#
# Intended to run ON the target host (via ollama-router provision / fleet SSH).
# Verda VMs and operator boxes: systemd + kernel TUN only. No Thunder/k8s.
#
# Required / optional environment:
#   TS_AUTHKEY            Tailscale auth key (required if not already logged in)
#   TS_HOSTNAME           Tailscale hostname (default: $(hostname -s))
#   TS_TAGS               Optional comma-separated advertise tags (e.g. tag:gpu)
#   TS_ADVERTISE_ROUTES   Optional CIDR list for subnet router (empty = peer-only)
#   TS_EPHEMERAL=1        Informational only: use an ephemeral auth key in
#                         Tailscale's admin console. Emitted in markers output.
#   TS_ACCEPT_DNS         Override --accept-dns (default: false). Set to "true"
#                         to enable MagicDNS on this host.
#   TS_ACCEPT_ROUTES=1    Accept subnet routes from other Tailscale nodes
#                         (--accept-routes).
#   OLLAMA_MODELS         Space-separated models to pull
#   SKIP_MODELS=1         Skip ollama pull
#   SKIP_OLLAMA=1         Tailscale-only: skip Ollama install/start/bind,
#                         model pulls, and capacity-agent install
#   OS_UPGRADE=1          When set (default), run apt dist-upgrade + reboot once
#                         before Tailscale/Ollama if state marker allows
#   APT_LOCK_TIMEOUT_SECONDS  Wait for dpkg/apt locks (default 900; cloud-init)
#   HARDEN_FIREWALL=1     UFW: allow 22 + tailscale0; default deny (default on)
#   PROVISION_PHASE       bootstrap | full (default: full)
#                         bootstrap = public-SSH handoff only: install Tailscale,
#                         `tailscale up`, emit TAILSCALE_IP= (no Ollama).
#                         full = OS upgrade + Tailscale + Ollama + models
#                         (unless SKIP_OLLAMA=1 → Tailscale only).
#   OLLAMA binds ONLY to the Tailscale IPv4.
#
# State marker: /var/lib/ollama-router-provision/state
#   (empty)       → os-upgrade phase when OS_UPGRADE=1 (full phase only)
#   os_upgraded   → setup phase (post-reboot)
#   complete      → setup phase only (idempotent repair; skip OS upgrade)
#   ts_joined     → bootstrap completed (Tailscale up; ordinary SSH handoff)
#
# Never prints the full TS_AUTHKEY.
#
# Usage (as root or via passwordless sudo):
#   sudo -n env TS_AUTHKEY=... TS_HOSTNAME=nuc bash provision-ollama-gpu.sh
#   sudo -n env PROVISION_PHASE=bootstrap TS_AUTHKEY=... bash provision-ollama-gpu.sh

set -euo pipefail

log() { printf '[provision-ollama-gpu] %s\n' "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }

STATE_DIR=/var/lib/ollama-router-provision
STATE_FILE="${STATE_DIR}/state"

redact_authkey() {
  local key="${1:-}"
  if [[ -z "$key" ]]; then
    printf '%s' '(empty)'
    return
  fi
  if [[ "$key" == tskey-* ]]; then
    printf 'tskey-*** (len=%d)' "${#key}"
  else
    printf '*** (len=%d)' "${#key}"
  fi
}

require_root() {
  if [[ "$(id -u)" -ne 0 ]]; then
    die "must run as root (or via sudo -n)"
  fi
}

read_state() {
  if [[ -f "$STATE_FILE" ]]; then
    tr -d '[:space:]' <"$STATE_FILE"
  else
    printf ''
  fi
}

write_state() {
  mkdir -p "$STATE_DIR"
  printf '%s\n' "$1" >"$STATE_FILE"
}

# Resolve Tailscale IPv4 (CGNAT 100.64.0.0/10). Never fall back to public/hostname -I.
is_tailscale_ipv4() {
  local ip="$1"
  [[ "$ip" =~ ^100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]+\.[0-9]+$ ]]
}

wait_for_tailscale_ipv4() {
  local ts_ip=""
  local _i
  for _i in $(seq 1 30); do
    ts_ip="$(tailscale ip -4 2>/dev/null | head -n1 | tr -d '[:space:]' || true)"
    if is_tailscale_ipv4 "$ts_ip"; then
      printf '%s' "$ts_ip"
      return 0
    fi
    sleep 1
  done
  return 1
}

ensure_tailscale_installed() {
  if ! command -v tailscale >/dev/null 2>&1; then
    log "installing Tailscale"
    if command -v apt-get >/dev/null 2>&1; then
      run_apt_get update -qq || true
      run_apt_get install -y -qq curl ca-certificates jq 2>/dev/null || \
        apt-get install -y -qq curl ca-certificates 2>/dev/null || true
    fi
    curl -fsSL https://tailscale.com/install.sh | sh
  else
    log "Tailscale already installed"
  fi
}

# True when PID 1 is systemd.
has_usable_systemd() {
  command -v systemctl >/dev/null 2>&1 || return 1
  local pid1
  pid1="$(ps -p 1 -o comm= 2>/dev/null | tr -d '[:space:]' || true)"
  [[ "$pid1" == "systemd" ]]
}

require_systemd() {
  has_usable_systemd || die "systemd is required (Verda VMs and operator boxes)"
}

ensure_kernel_tun() {
  mkdir -p /dev/net
  if [[ ! -c /dev/net/tun ]]; then
    mknod /dev/net/tun c 10 200 2>/dev/null || die "cannot create /dev/net/tun"
    chmod 666 /dev/net/tun 2>/dev/null || true
  fi
}

tailscaled_socket_ready() {
  [[ -S /var/run/tailscale/tailscaled.sock ]] || [[ -S /run/tailscale/tailscaled.sock ]]
}

ensure_tailscaled_running() {
  local timeout_seconds="${TAILSCALED_READY_TIMEOUT_SECONDS:-60}"
  local start
  start="$(date +%s)"
  require_systemd
  ensure_kernel_tun

  systemctl enable --now tailscaled >/dev/null 2>&1 || \
    systemctl start tailscaled >/dev/null 2>&1 || true

  log "waiting up to ${timeout_seconds}s for local tailscaled"
  while true; do
    if tailscale status --json >/dev/null 2>&1; then
      log "local tailscaled is ready"
      return 0
    fi
    if tailscale version >/dev/null 2>&1 && tailscaled_socket_ready; then
      log "local tailscaled socket is ready"
      return 0
    fi
    if (( "$(date +%s)" - start >= timeout_seconds )); then
      log "tailscaled diagnostics (last log lines):"
      tail -n 40 /var/log/tailscaled.log 2>/dev/null >&2 || true
      systemctl status tailscaled --no-pager -l 2>/dev/null >&2 || true
      die "local tailscaled did not become ready within ${timeout_seconds}s"
    fi
    systemctl start tailscaled >/dev/null 2>&1 || true
    sleep 1
  done
}

# Join Tailscale for ordinary OpenSSH over the tailnet. Do not enable
# Tailscale SSH: its port-22 interceptor is controlled by tailnet SSH policy,
# while the router authenticates with the configured OpenSSH key instead.
# TS_EPHEMERAL is informational: current Tailscale derives that lifecycle from
# the auth-key type, not a `tailscale up --ephemeral` flag.
tailscale_up_for_regular_ssh() {
  ensure_tailscaled_running

  local accept_dns="${TS_ACCEPT_DNS:-false}"
  local ts_up_args=(--hostname="$TS_HOSTNAME" --accept-dns="$accept_dns")
  if [[ "${TS_ACCEPT_ROUTES:-0}" == "1" ]]; then
    ts_up_args+=(--accept-routes)
  fi
  if [[ -n "${TS_TAGS:-}" ]]; then
    ts_up_args+=(--advertise-tags="$TS_TAGS")
  fi
  if [[ -n "${TS_ADVERTISE_ROUTES:-}" ]]; then
    ts_up_args+=(--advertise-routes="$TS_ADVERTISE_ROUTES")
  fi

  local backend_state=""
  if command -v jq >/dev/null 2>&1; then
    backend_state="$(tailscale status --json 2>/dev/null | jq -r '.BackendState // empty' || true)"
  fi

  if [[ "$backend_state" == "Running" ]]; then
    log "Tailscale already Running; re-applying up flags (ephemeral=${TS_EPHEMERAL:-0})"
    tailscale up "${ts_up_args[@]}" || true
    return 0
  fi

  if [[ -z "${TS_AUTHKEY:-}" ]]; then
    die "TS_AUTHKEY is required when Tailscale is not already logged in (BackendState=${backend_state:-unknown})"
  fi

  log "tailscale up for regular OpenSSH (authkey=$(redact_authkey "$TS_AUTHKEY") ephemeral=${TS_EPHEMERAL:-0} accept_dns=${accept_dns} accept_routes=${TS_ACCEPT_ROUTES:-0})"

  local ts_out ts_err
  local attempt=1
  local max_attempts=5
  while true; do
    ts_out="$(mktemp /tmp/tailscale-up-out.XXXXXX)"
    ts_err="$(mktemp /tmp/tailscale-up-err.XXXXXX)"
    trap 'rm -f "$ts_out" "$ts_err"' RETURN

    tailscale up --auth-key="$TS_AUTHKEY" "${ts_up_args[@]}" >"$ts_out" 2>"$ts_err" && ec=0 || ec=$?

    if [[ "$ec" -eq 0 ]]; then
      rm -f "$ts_out" "$ts_err"
      trap - RETURN
      return 0
    fi

    log "tailscale up failed (exit=${ec}); stderr:"
    while IFS= read -r line; do
      log "  ts-up: ${line}"
    done <"$ts_err"
    log "tailscale up stdout (last 10 lines):"
    tail -n 10 "$ts_out" >&2 || true

    if (( attempt >= max_attempts )); then
      log "tailscale daemon diagnostics (last 20 slog lines):"
      tail -n 20 /var/log/tailscaled.log 2>/dev/null >&2 || true
      if command -v jq >/dev/null 2>&1; then
        log "tailscale status JSON:"
        tailscale status --json 2>/dev/null >&2 || log "(no status json available)"
      fi
      log "tailscale netcheck:"
      tailscale netcheck 2>/dev/null >&2 || log "(netcheck unavailable)"
      rm -f "$ts_out" "$ts_err"
      trap - RETURN
      return "$ec"
    fi

    rm -f "$ts_out" "$ts_err"
    log "tailscale up failed (exit=${ec}); ensuring daemon and retry ${attempt}/${max_attempts}"
    ensure_tailscaled_running
    attempt=$((attempt + 1))
    sleep 2
  done
}

# Disable Tailscale's port-22 interceptor when a template or prior run enabled
# it. The router deliberately uses the host's regular OpenSSH daemon through
# the Tailscale network, authenticated by its configured SSH key.
disable_tailscale_ssh_interceptor() {
  if tailscale set --ssh=false >/dev/null 2>&1; then
    log "Tailscale SSH interceptor disabled; ordinary OpenSSH owns port 22"
  else
    die "could not disable Tailscale SSH interceptor; refusing an ambiguous ordinary-OpenSSH handoff"
  fi
}

# Verify the node actually appears online in the tailnet (not just has an IP).
verify_tailscale_online() {
  local timeout="${1:-30}"
  local start _i
  start="$(date +%s)"
  log "verifying Tailscale online (timeout=${timeout}s)"
  for _i in $(seq 1 "$timeout"); do
    if command -v jq >/dev/null 2>&1; then
      local online backend
      online="$(tailscale status --json 2>/dev/null | jq -r '.Self.Online // false' || true)"
      backend="$(tailscale status --json 2>/dev/null | jq -r '.BackendState // empty' || true)"
      if [[ "$online" == "true" ]] && [[ "$backend" == "Running" ]]; then
        log "Tailscale node is online (BackendState=Running)"
        return 0
      fi
      if [[ "$backend" != "Running" ]]; then
        log "Tailscale BackendState=${backend} (waiting for Running)"
      fi
    else
      if tailscale status >/dev/null 2>&1 && tailscale ip -4 >/dev/null 2>&1; then
        log "Tailscale node appears online (no jq)"
        return 0
      fi
    fi
    sleep 1
  done
  log "WARN: Tailscale node did not appear online within ${timeout}s"
  if command -v jq >/dev/null 2>&1; then
    log "tailscale status JSON (post-timeout):"
    tailscale status --json 2>/dev/null >&2 || true
  fi
  return 1
}

emit_tailscale_markers() {
  local ts_ip="$1"
  printf 'TAILSCALE_IP=%s\n' "$ts_ip"
  local online=0
  if command -v jq >/dev/null 2>&1; then
    local self_online backend
    self_online="$(tailscale status --json 2>/dev/null | jq -r '.Self.Online // false' || true)"
    backend="$(tailscale status --json 2>/dev/null | jq -r '.BackendState // empty' || true)"
    if [[ "$self_online" == "true" ]] && [[ "$backend" == "Running" ]]; then
      online=1
    fi
  else
    if tailscale status >/dev/null 2>&1; then
      online=1
    fi
  fi
  printf 'TAILSCALE_ONLINE=%d\n' "$online"
  printf 'REGULAR_SSH_TAILNET_READY=%d\n' "$online"
  printf 'TAILSCALE_EPHEMERAL=%s\n' "${TS_EPHEMERAL:-0}"
}

TS_HOSTNAME="${TS_HOSTNAME:-$(hostname -s)}"
TS_TAGS="${TS_TAGS:-}"
TS_ADVERTISE_ROUTES="${TS_ADVERTISE_ROUTES:-}"
TS_EPHEMERAL="${TS_EPHEMERAL:-0}"
TS_ACCEPT_DNS="${TS_ACCEPT_DNS:-false}"
TS_ACCEPT_ROUTES="${TS_ACCEPT_ROUTES:-0}"
SKIP_MODELS="${SKIP_MODELS:-0}"
SKIP_OLLAMA="${SKIP_OLLAMA:-0}"
OS_UPGRADE="${OS_UPGRADE:-1}"
PROVISION_PHASE="${PROVISION_PHASE:-full}"
OLLAMA_MODELS="${OLLAMA_MODELS:-}"

case "$PROVISION_PHASE" in
  bootstrap|full) ;;
  *) die "PROVISION_PHASE must be bootstrap or full (got: ${PROVISION_PHASE})" ;;
esac

require_root
require_systemd

STATE="$(read_state)"
log "hostname=${TS_HOSTNAME} phase=${PROVISION_PHASE} authkey=$(redact_authkey "${TS_AUTHKEY:-}") ephemeral=${TS_EPHEMERAL} accept_dns=${TS_ACCEPT_DNS} accept_routes=${TS_ACCEPT_ROUTES} skip_models=${SKIP_MODELS} skip_ollama=${SKIP_OLLAMA} os_upgrade=${OS_UPGRADE} state=${STATE:-empty}"

export DEBIAN_FRONTEND=noninteractive

APT_LOCK_TIMEOUT_SECONDS="${APT_LOCK_TIMEOUT_SECONDS:-900}"

apt_lock_held() {
  if command -v fuser >/dev/null 2>&1; then
    fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1 && return 0
    fuser /var/lib/dpkg/lock >/dev/null 2>&1 && return 0
    fuser /var/lib/apt/lists/lock >/dev/null 2>&1 && return 0
    fuser /var/cache/apt/archives/lock >/dev/null 2>&1 && return 0
  fi
  if command -v flock >/dev/null 2>&1; then
    flock -n /var/lib/dpkg/lock-frontend true 2>/dev/null || return 0
    flock -n /var/lib/dpkg/lock true 2>/dev/null || return 0
    flock -n /var/lib/apt/lists/lock true 2>/dev/null || return 0
  fi
  if systemctl is-active --quiet unattended-upgrades.service 2>/dev/null; then
    return 0
  fi
  if systemctl is-active --quiet apt-daily.service 2>/dev/null; then
    return 0
  fi
  if systemctl is-active --quiet apt-daily-upgrade.service 2>/dev/null; then
    return 0
  fi
  return 1
}

wait_for_apt_lock() {
  local timeout="${APT_LOCK_TIMEOUT_SECONDS}"
  local start
  start="$(date +%s)"
  if ! apt_lock_held; then
    return 0
  fi
  log "apt/dpkg lock held — waiting up to ${timeout}s (unattended-upgrades/cloud-init)"
  systemctl stop unattended-upgrades.service 2>/dev/null || true
  systemctl stop apt-daily.service apt-daily-upgrade.service 2>/dev/null || true
  while apt_lock_held; do
    if (( "$(date +%s)" - start >= timeout )); then
      die "timed out after ${timeout}s waiting for apt/dpkg lock"
    fi
    sleep 5
  done
  log "apt/dpkg lock free"
}

run_apt_get() {
  local attempt=1
  local max_attempts=36
  while true; do
    wait_for_apt_lock
    if apt-get "$@"; then
      return 0
    fi
    local ec=$?
    if (( attempt >= max_attempts )); then
      return "$ec"
    fi
    log "apt-get failed (exit=${ec}); retry ${attempt}/${max_attempts} after brief wait"
    attempt=$((attempt + 1))
    sleep 10
  done
}

ensure_dns_works() {
  local test_host="${1:-login.tailscale.com}"
  local timeout="${DNS_READY_TIMEOUT_SECONDS:-120}"
  local start
  start="$(date +%s)"

  if command -v nslookup >/dev/null 2>&1; then
    if nslookup "$test_host" >/dev/null 2>&1; then
      return 0
    fi
  elif command -v dig >/dev/null 2>&1; then
    if dig +short "$test_host" >/dev/null 2>&1; then
      return 0
    fi
  elif command -v getent >/dev/null 2>&1; then
    if getent ahosts "$test_host" >/dev/null 2>&1; then
      return 0
    fi
  fi
  log "DNS not resolving ${test_host} — checking /etc/resolv.conf"

  local stub="/run/systemd/resolve/stub-resolv.conf"
  local current_target
  current_target="$(readlink -f /etc/resolv.conf 2>/dev/null || true)"

  if [[ -f "$stub" ]] && [[ "$current_target" != "$stub" ]]; then
    log "fixing /etc/resolv.conf → ${stub} (was: ${current_target:-none})"
    ln -sf "$stub" /etc/resolv.conf
    systemctl restart systemd-resolved 2>/dev/null || true
  fi

  while true; do
    if command -v nslookup >/dev/null 2>&1; then
      if nslookup "$test_host" >/dev/null 2>&1; then
        log "DNS resolution restored to ${test_host}"
        return 0
      fi
    elif command -v dig >/dev/null 2>&1; then
      if dig +short "$test_host" >/dev/null 2>&1; then
        log "DNS resolution restored to ${test_host}"
        return 0
      fi
    elif command -v getent >/dev/null 2>&1; then
      if getent ahosts "$test_host" >/dev/null 2>&1; then
        log "DNS resolution restored to ${test_host}"
        return 0
      fi
    fi
    if (( "$(date +%s)" - start >= timeout )); then
      log "WARN: DNS still not resolving ${test_host} after ${timeout}s (continuing anyway)"
      return 1
    fi
    sleep 2
  done
}

join_tailscale() {
  ensure_dns_works || log "WARN: DNS pre-check failed; Tailscale join may fail"
  ensure_tailscale_installed
  tailscale_up_for_regular_ssh
  disable_tailscale_ssh_interceptor
}

# ---------------------------------------------------------------------------
# Bootstrap phase (public SSH): Tailscale join only — keep short.
# Orchestrator then uses ordinary OpenSSH over the Tailscale IP for full phase.
# ---------------------------------------------------------------------------
if [[ "$PROVISION_PHASE" == "bootstrap" ]]; then
  log "phase=bootstrap: Tailscale install + ordinary SSH handoff (no OS upgrade / Ollama)"
  need_pkgs=()
  for pkg in curl ca-certificates jq iproute2; do
    if ! dpkg -s "$pkg" >/dev/null 2>&1; then
      need_pkgs+=("$pkg")
    fi
  done
  if ((${#need_pkgs[@]})); then
    log "installing bootstrap packages: ${need_pkgs[*]}"
    run_apt_get update -qq
    run_apt_get install -y -qq "${need_pkgs[@]}"
  fi
  join_tailscale
  TS_IP="$(wait_for_tailscale_ipv4)" || die "no Tailscale IPv4 after bootstrap join (refusing public IP fallback)"
  log "Tailscale IPv4=${TS_IP}"
  verify_tailscale_online 30 || log "WARN: Tailscale node not online after bootstrap (check auth key / network)"
  if [[ "$STATE" != "complete" && "$STATE" != "os_upgraded" ]]; then
    write_state "ts_joined"
  fi
  log "bootstrap done"
  emit_tailscale_markers "$TS_IP"
  exit 0
fi

# ---------------------------------------------------------------------------
# Phase: OS upgrade + reboot (once per machine until marker advances)
# Full phase only. Bootstrap never reboots (public SSH session must survive).
# ---------------------------------------------------------------------------
if [[ "$OS_UPGRADE" == "1" && "$STATE" != "os_upgraded" && "$STATE" != "complete" ]]; then
  log "phase=os-upgrade: apt-get update && dist-upgrade"
  run_apt_get update -qq
  run_apt_get -y -o Dpkg::Options::="--force-confdef" -o Dpkg::Options::="--force-confold" dist-upgrade
  write_state "os_upgraded"
  log "OS upgraded; scheduling reboot"
  printf 'REBOOT_REQUIRED=1\n'
  sync
  nohup bash -c 'sleep 3; systemctl reboot || reboot' >/dev/null 2>&1 &
  exit 0
fi

# ---------------------------------------------------------------------------
# Phase: setup (sysctl, ethtool, Tailscale, Ollama, models)
# ---------------------------------------------------------------------------
log "phase=setup"

if [[ "$SKIP_OLLAMA" == "1" ]]; then
  log "SKIP_OLLAMA=1 — Tailscale-only setup (no packages/Ollama/capacity)"
  join_tailscale
  TS_IP="$(wait_for_tailscale_ipv4)" || die "no Tailscale IPv4 after join (refusing public IP fallback)"
  log "Tailscale IPv4=${TS_IP}"
  verify_tailscale_online 30 || log "WARN: Tailscale node not online (check auth key / network)"
  write_state "complete"
  log "done (tailscale-only)"
  emit_tailscale_markers "$TS_IP"
  printf 'curl check: curl -fsS http://%s:11434/api/tags\n' "$TS_IP"
  exit 0
fi

need_pkgs=()
for pkg in ethtool iproute2 curl jq ca-certificates; do
  if ! dpkg -s "$pkg" >/dev/null 2>&1; then
    need_pkgs+=("$pkg")
  fi
done
if ((${#need_pkgs[@]})); then
  log "installing packages: ${need_pkgs[*]}"
  run_apt_get update -qq
  run_apt_get install -y -qq "${need_pkgs[@]}"
else
  log "packages already present"
fi

SYSCTL_FILE=/etc/sysctl.d/99-tailscale.conf
log "writing ${SYSCTL_FILE}"
cat >"$SYSCTL_FILE" <<'EOF'
net.ipv4.ip_forward = 1
net.ipv6.conf.all.forwarding = 1
EOF
sysctl -p "$SYSCTL_FILE" >/dev/null 2>&1 || \
  log "WARN: sysctl apply failed"

NETDEV="$(ip -o route get 8.8.8.8 2>/dev/null | cut -f 5 -d ' ' || true)"
if [[ -z "${NETDEV}" ]]; then
  log "WARN: could not detect default NETDEV; skipping ethtool"
else
  log "ethtool UDP GRO on ${NETDEV}"
  ethtool -K "$NETDEV" rx-udp-gro-forwarding on rx-gro-list off || \
    log "WARN: ethtool -K failed on ${NETDEV} (may be unsupported)"

  ETHTOOL_SCRIPT_BODY=$(cat <<EOF
#!/bin/sh
# Persist Tailscale UDP GRO settings across reboot.
NETDEV=\$(ip -o route get 8.8.8.8 | cut -f 5 -d " ")
[ -n "\$NETDEV" ] || exit 0
ethtool -K "\$NETDEV" rx-udp-gro-forwarding on rx-gro-list off
EOF
)

  if systemctl is-enabled networkd-dispatcher >/dev/null 2>&1; then
    DEST=/etc/networkd-dispatcher/routable.d/50-tailscale
    mkdir -p "$(dirname "$DEST")"
    printf '%s\n' "$ETHTOOL_SCRIPT_BODY" >"$DEST"
    chmod 755 "$DEST"
    log "persisted ethtool via ${DEST}"
  else
    DEST=/usr/local/sbin/tailscale-udp-gro.sh
    printf '%s\n' "$ETHTOOL_SCRIPT_BODY" >"$DEST"
    chmod 755 "$DEST"
    UNIT=/etc/systemd/system/tailscale-udp-gro.service
    cat >"$UNIT" <<EOF
[Unit]
Description=Tailscale UDP GRO ethtool settings
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=${DEST}
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now tailscale-udp-gro.service >/dev/null
    log "persisted ethtool via ${UNIT}"
  fi
fi

join_tailscale

TS_IP="$(wait_for_tailscale_ipv4)" || die "no Tailscale IPv4 after join (refusing public IP fallback)"
log "Tailscale IPv4=${TS_IP}"
verify_tailscale_online 30 || log "WARN: Tailscale node not online (check auth key / network)"

HARDEN_FIREWALL="${HARDEN_FIREWALL:-1}"
if [[ "$HARDEN_FIREWALL" == "1" ]]; then
  if ! command -v ufw >/dev/null 2>&1; then
    log "installing ufw"
    run_apt_get update -qq
    run_apt_get install -y -qq ufw
  fi
  log "hardening UFW: allow 22/tcp (public SSH until tailnet OpenSSH is proven) + tailscale scoped ports; default deny incoming"
  ufw --force reset >/dev/null 2>&1 || true
  ufw default deny incoming >/dev/null
  ufw default allow outgoing >/dev/null
  ufw allow 22/tcp >/dev/null
  ufw allow in on tailscale0 to any port 11434 proto tcp >/dev/null
  ufw allow in on tailscale0 to any port 11436 proto tcp >/dev/null
  ufw allow in on tailscale0 to any port 22 proto tcp >/dev/null
  ufw --force enable >/dev/null
  ufw status numbered >&2 || true
  log "keeping public SSH port 22 open; router closes it only after an independent ordinary-OpenSSH tailnet probe"
else
  log "HARDEN_FIREWALL=0 — skipping UFW"
fi

OLLAMA_BIND="$TS_IP"
log "kernel Tailscale — Ollama will bind ${OLLAMA_BIND}:11434 only"

if ! command -v ollama >/dev/null 2>&1; then
  log "installing Ollama"
  curl -fsSL https://ollama.com/install.sh | sh
else
  log "Ollama already installed"
fi

DROPIN_DIR=/etc/systemd/system/ollama.service.d
DROPIN_FILE="${DROPIN_DIR}/override.conf"
mkdir -p "$DROPIN_DIR"
desired_dropin=$(printf '[Service]\nEnvironment="OLLAMA_HOST=%s:11434"\n' "$OLLAMA_BIND")
needs_restart=0
if [[ -f "$DROPIN_FILE" ]] && [[ "$(cat "$DROPIN_FILE")" == "$desired_dropin" ]]; then
  log "Ollama systemd drop-in already correct (bind ${OLLAMA_BIND}:11434)"
else
  log "writing ${DROPIN_FILE} (OLLAMA_HOST=${OLLAMA_BIND}:11434)"
  printf '%s' "$desired_dropin" >"$DROPIN_FILE"
  needs_restart=1
fi

systemctl daemon-reload
systemctl enable ollama >/dev/null 2>&1 || true
if [[ "$needs_restart" -eq 1 ]] || ! systemctl is-active --quiet ollama; then
  log "restarting ollama"
  systemctl restart ollama
else
  systemctl start ollama
fi

for _ in $(seq 1 60); do
  if ss -ltn 2>/dev/null | awk '/:11434/ {print $4}' | grep -F "${OLLAMA_BIND}:11434" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

verify_ollama_bind() {
  local listeners
  listeners="$(ss -ltn 2>/dev/null | awk '/:11434/ {print $4}' || true)"
  if [[ -z "$listeners" ]]; then
    die "Ollama is not listening on :11434"
  fi
  local ok=0
  local line
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    if [[ "$line" == "${OLLAMA_BIND}:11434" ]]; then
      ok=1
      continue
    fi
    if [[ "$line" == "127.0.0.1:11434" || "$line" == "[::1]:11434" || "$line" == "::1:11434" ]]; then
      continue
    fi
    die "Ollama must not listen on public/wildcard (${line}); expected only ${OLLAMA_BIND}:11434 (+ loopback)"
  done <<<"$listeners"

  if [[ "$ok" -ne 1 ]]; then
    die "Ollama is not bound to ${OLLAMA_BIND}:11434 (listeners: ${listeners//$'\n'/ })"
  fi
  log "Ollama Tailscale-only bind ok: ${listeners//$'\n'/ }"
}

verify_ollama_bind

CAPACITY_TOKEN="${OLLAMA_CAPACITY_TOKEN:-}"
if command -v docker >/dev/null 2>&1; then
  log "installing ollama-capacity-agent (Docker)"
  if [[ -n "$CAPACITY_TOKEN" ]]; then
    mkdir -p /etc/ollama-capacity
    printf 'OLLAMA_CAPACITY_TOKEN=%s\n' "$CAPACITY_TOKEN" > /etc/ollama-capacity/env
    chmod 600 /etc/ollama-capacity/env
    log "capacity agent token written to /etc/ollama-capacity/env (0600)"
  fi

  DOCKER_RUN=(docker run -d --name ollama-capacity --restart=unless-stopped --network host)
  if [[ -n "$CAPACITY_TOKEN" ]]; then
    DOCKER_RUN+=(-e "OLLAMA_CAPACITY_TOKEN=${CAPACITY_TOKEN}")
  fi
  DOCKER_RUN+=(-e "OLLAMA_CAPACITY_HOST=${TS_IP}" -e "OLLAMA_CAPACITY_PORT=11436")
  if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
    DOCKER_RUN+=(--gpus all)
  fi
  DOCKER_RUN+=(ghcr.io/illumination/ollama-capacity-agent:latest)

  docker rm -f ollama-capacity >/dev/null 2>&1 || true
  if "${DOCKER_RUN[@]}"; then
    log "capacity agent started (Docker, bound to ${TS_IP}:11436)"
    printf '  curl check: curl -fsS http://%s:11436/healthz\n' "$TS_IP"
  else
    log "WARN: capacity agent Docker run failed; provision continues"
  fi
else
  log "WARN: Docker not found — skip capacity agent install. Install manually:"
  log "  docker run -d --name ollama-capacity --restart=unless-stopped --network host \\"
  log "    -e OLLAMA_CAPACITY_HOST=${TS_IP} -e OLLAMA_CAPACITY_PORT=11436 \\"
  if [[ -n "$CAPACITY_TOKEN" ]]; then
    log "    -e OLLAMA_CAPACITY_TOKEN=... \\"
  fi
  log "    ghcr.io/illumination/ollama-capacity-agent:latest"
fi

if [[ "$SKIP_MODELS" == "1" ]]; then
  log "SKIP_MODELS=1 — skipping pulls"
else
  export OLLAMA_HOST="${OLLAMA_BIND}:11434"
  for model in $OLLAMA_MODELS; do
    log "ollama pull ${model}"
    ollama pull "$model"
  done
fi

write_state "complete"
log "done"
emit_tailscale_markers "$TS_IP"
printf 'curl check: curl -fsS http://%s:11434/api/tags\n' "$TS_IP"
