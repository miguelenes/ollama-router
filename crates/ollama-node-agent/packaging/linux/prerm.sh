#!/bin/sh
if [ -d /run/systemd/system ]; then
  systemctl disable --now ollama-node-agent-tunnel || true
  systemctl disable --now ollama-node-agent || true
fi
