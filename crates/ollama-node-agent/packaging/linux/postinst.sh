#!/bin/sh
set -e
if ! id ollama-node-agent >/dev/null 2>&1; then
  useradd -r -s /usr/sbin/nologin -M ollama-node-agent || true
fi
if [ -d /var/lib/ollama-node-agent ]; then
  chown ollama-node-agent:ollama-node-agent /var/lib/ollama-node-agent || true
fi
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
  systemctl enable --now ollama-node-agent || true
else
  echo "systemd not detected; start with: /usr/local/bin/ollama-node-agent serve --config /etc/ollama-node-agent/config.yaml"
fi
