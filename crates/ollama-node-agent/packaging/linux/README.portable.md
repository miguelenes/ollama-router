# Portable ollama-node-agent (Linux)

Musl-static binary for any Linux (no dpkg required). Extract and install with
the binary's own `setup` (elevated, idempotent):

```bash
tar -xzf ollama-node-agent-linux-<arch>.tar.gz
sudo ./ollama-node-agent setup
```

`setup` writes the systemd unit (same text as `ollama-node-agent.service` in this
archive, from `setup --print-unit`) when `/run/systemd/system` exists. Without
systemd it copies the binary and prints how to run `serve` under another
supervisor. `contrib/openrc/ollama-node-agent` is optional; `setup` does not
install it.

The `.deb` is the Debian/Ubuntu package path (gnu, glibc ≥ bookworm). Packages
install the agent only; `setup` still converges Ollama.
