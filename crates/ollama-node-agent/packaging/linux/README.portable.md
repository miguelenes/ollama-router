# Portable ollama-node-agent (Linux)

Musl **static-pie** binary for any Linux (no dpkg/rpm, no glibc version pin). Extract
and install with the binary's own `setup` (elevated, idempotent):

```bash
tar -xzf ollama-node-agent-linux-<arch>.tar.gz
sudo ./ollama-node-agent setup
```

`setup` writes the systemd unit (same text as `ollama-node-agent.service` in this
archive, from `setup --print-unit`) when `/run/systemd/system` exists, plus
`ollama-node-agent-tunnel.service` for the zrok private-share sidecar. Without
systemd it copies the binary and prints how to run `serve` (and `tunnel`) under
another supervisor. `contrib/openrc/ollama-node-agent` is optional; `setup` does not
install it.

The `.deb` is the Debian/Ubuntu package path (gnu, glibc ≥ bookworm). Packages
install the agent only; `setup` still converges Ollama.

Enable zrok against a **self-hosted** controller (not a VPN, not Tailscale):

```bash
export ZROK_API_ENDPOINT=http://127.0.0.1:18080   # or https://zrok.example
export ZROK_ENABLE_TOKEN='…'                      # enable token from the controller
sudo --preserve-env=ZROK_API_ENDPOINT,ZROK_ENABLE_TOKEN ./ollama-node-agent setup
```

`setup`/`doctor` print a **find this node** block (share token **id** + enroll
status). The router enrolls that share; same-LAN `fleet.yaml` hosts keep
direct URLs. Never write enable or share tokens into `config.yaml`.
