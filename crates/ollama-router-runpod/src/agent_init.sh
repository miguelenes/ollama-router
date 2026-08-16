# ollama-router Verda first-boot installer. Secrets come from
# /run/ollama-router/bootstrap.env (written by the router at create).
# Never xtrace. Never echo secrets. Do not install Tailscale.
# Do not probe public :11434.

set -euo pipefail
set +o xtrace
umask 077

BOOTSTRAP=/run/ollama-router/bootstrap.env
if [[ -f "${BOOTSTRAP}" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "${BOOTSTRAP}"
  set +a
fi

if [[ -n "${OLLAMA_ROUTER_ADMIN_TOKEN:-}" || -n "${ZROK_API_ENDPOINT:-}" ]]; then
  mkdir -p /etc/ollama-node-agent
  : >/etc/ollama-node-agent/env
  if [[ -n "${OLLAMA_ROUTER_ADMIN_TOKEN:-}" ]]; then
    printf 'OLLAMA_ROUTER_ADMIN_TOKEN=%s\n' "${OLLAMA_ROUTER_ADMIN_TOKEN}" >>/etc/ollama-node-agent/env
  fi
  if [[ -n "${ZROK_API_ENDPOINT:-}" ]]; then
    printf 'ZROK_API_ENDPOINT=%s\n' "${ZROK_API_ENDPOINT}" >>/etc/ollama-node-agent/env
  fi
  chmod 600 /etc/ollama-node-agent/env
fi

mkdir -p /etc/ollama-node-agent
{
  printf '%s\n' 'listen: loopback'
  printf '%s\n' 'port: 11436'
  printf '%s\n' 'ollama:'
  printf '%s\n' '  listen: loopback'
  printf '%s\n' 'tunnel:'
  printf '%s\n' '  enable: true'
  printf '%s\n' '  zrok_bin: zrok'
  if [[ -n "${ZROK_API_ENDPOINT:-}" ]]; then
    printf '  api_endpoint: "%s"\n' "${ZROK_API_ENDPOINT}"
  fi
  printf '%s\n' 'register:'
  printf '%s\n' '  origin: runpod'
  printf '%s\n' "  token_env: ${ENROLL_TOKEN_ENV:-OLLAMA_ROUTER_ADMIN_TOKEN}"
  if [[ -n "${ENROLL_URL:-}" ]]; then
    printf '  url: "%s"\n' "${ENROLL_URL}"
  fi
} >/etc/ollama-node-agent/config.yaml
chmod 644 /etc/ollama-node-agent/config.yaml

arch=$(uname -m)
case "${arch}" in
  x86_64 | amd64) deb_arch=amd64 ;;
  aarch64 | arm64) deb_arch=arm64 ;;
  *) deb_arch=amd64 ;;
esac

install_from_file() {
  local pkg=$1
  case "${pkg}" in
    *.deb)
      dpkg -i "${pkg}" || apt-get install -y -f
      ;;
    *.tar.gz | *.tgz)
      local stage
      stage=$(mktemp -d)
      tar -xzf "${pkg}" -C "${stage}"
      if [[ -x "${stage}/ollama-node-agent" ]]; then
        install -m 755 "${stage}/ollama-node-agent" /usr/local/bin/ollama-node-agent
      else
        echo "agent tarball missing binary" >&2
        rm -rf "${stage}"
        return 1
      fi
      rm -rf "${stage}"
      ;;
    *)
      echo "unsupported agent package" >&2
      return 1
      ;;
  esac
}

download_install() {
  if command -v ollama-node-agent >/dev/null 2>&1; then
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    apt-get update -qq
    apt-get install -y -qq curl ca-certificates
  fi
  local tmp
  tmp=$(mktemp)
  local url=""
  if [[ -n "${AGENT_PACKAGE_URL:-}" ]]; then
    url="${AGENT_PACKAGE_URL}"
  elif [[ "${deb_arch}" == "arm64" ]]; then
    url="${AGENT_DEB_ARM64:-}"
  else
    url="${AGENT_DEB_AMD64:-}"
  fi
  if [[ -z "${url}" ]]; then
    echo "agent package url missing" >&2
    return 1
  fi
  if ! curl -fsSL -o "${tmp}" "${url}"; then
    rm -f "${tmp}"
    if [[ -n "${AGENT_PACKAGE_URL:-}" ]]; then
      echo "agent package download failed" >&2
      return 1
    fi
    if [[ "${deb_arch}" == "arm64" ]]; then
      url="${AGENT_TAR_ARM64:-}"
    else
      url="${AGENT_TAR_AMD64:-}"
    fi
    tmp=$(mktemp)
    if [[ -z "${url}" ]] || ! curl -fsSL -o "${tmp}" "${url}"; then
      rm -f "${tmp}"
      echo "agent package download failed" >&2
      return 1
    fi
  fi
  if [[ "${url}" == *.deb ]]; then
    mv "${tmp}" "${tmp}.deb"
    tmp="${tmp}.deb"
  else
    mv "${tmp}" "${tmp}.tar.gz"
    tmp="${tmp}.tar.gz"
  fi
  install_from_file "${tmp}"
  rm -f "${tmp}"
}

download_install

if ! command -v ollama-node-agent >/dev/null 2>&1; then
  echo "ollama-node-agent missing after install" >&2
  exit 1
fi

setup_args=(setup --config /etc/ollama-node-agent/config.yaml)
if [[ -n "${ENROLL_URL:-}" ]]; then
  setup_args+=(--enroll-url "${ENROLL_URL}" --enroll-token-env "${ENROLL_TOKEN_ENV:-OLLAMA_ROUTER_ADMIN_TOKEN}")
fi
# ZROK_ENABLE_TOKEN is taken from the environment (never argv).
ollama-node-agent "${setup_args[@]}"

mkdir -p /var/lib/ollama-node-agent
touch /var/lib/ollama-node-agent/bootstrap.ok
chmod 600 /var/lib/ollama-node-agent/bootstrap.ok
