---
title: Ollama node agent (capacity discovery)
tags: [ollama, router, capacity, ram-pressure, rust, node-agent]
sourceRefs:
  - crates/ollama-node-agent
  - crates/ollama-capacity-types
  - crates/ollama-router-core/src/capacity
  - crates/ollama-router-core/src/fleet
lastReviewed: 2026-08-14
---

# Ollama node agent

Every Ollama host in the mixed CPU/GPU fleet runs **`ollama-node-agent`**
(`crates/ollama-node-agent`). The router process needs **no GPU** and does not
install Ollama. They meet over HTTP on port `11436`.

Privilege split:

- `ollama-node-agent setup` — elevated, idempotent converge (install + OS service)
- `ollama-node-agent serve` — unprivileged HTTP on `:11436`
- `ollama-node-agent doctor` — no side effects
- `ollama-node-agent uninstall` — best-effort unit/plist/task removal

The agent never talks to Verda and never owns cloud idle teardown. On tunneled
hosts, `setup` starts a zrok sidecar by spawning the zrok binary (no Go SDK,
no TUN); Ollama and the agent bind **loopback**. `setup` and `doctor` print a
**find this node** block (share token **id** + enroll status); the router
learns the share via enroll (FleetState only, never `fleet.yaml`). `serve` must not hold `VERDA_*` or control-plane admin secrets.
See [[concepts/ollama-router-node-tunnel]]. `fleet.yaml` LAN URLs stay direct
HTTP on `:11434` / `:11436`.

Shared JSON types live in `crates/ollama-capacity-types`. The router owns only
the HTTP client and merge policy in `crates/ollama-router-core/src/capacity/`.

Wire: `GET /healthz`, `GET /metrics` (open); `GET /v1/capacity`, `/v1/pressure`,
`/v1/status` (bearer if `token` / `OLLAMA_NODE_AGENT_TOKEN` is set). JSON field
names stay compatible with the historical Illumination capacity agent;
additive keys only (`gpu_backend`, `vram_free_known`, RAM/GPU util, disk, loaded counts).

**GiB = bytes / 1024³.** sysinfo reports RAM in bytes. Never divide by `1024²`.
nvidia-smi memory is MiB (`/ 1024` → GiB).

**Unknown vs measured.** Never encode unknown as `0` for VRAM free/used, GPU
util, RAM available, or CPU%. Additive `vram_free_known` / `vram_used_known`
booleans mark a real sample. A full GPU is `vram_free_gb=0` **and**
`vram_free_known=true`. CPU / Metal-without-discrete is `gpus=0`, `vram_gb=0`,
`vram_free_known=false`. Old agents without the flag: infer known iff
`gpus > 0 && vram_gb > 0`.

**Grafana display path.** The health probe already GETs `/v1/capacity` and
`/v1/pressure`. The router persists fields on `NodeState` and re-exports
`ollama_router_node_*` on `GET /metrics`. Prometheus scrapes the **router**
only for fleet dashboards. Do not add production scrape jobs for agent
`:11436` (agents do not know the fleet.yaml node id; Verda spots churn).
`task compose:mock` may still scrape mock `:11436` for `ollama_up`. Grafana gates
VRAM panels on `vram_free_known == 1`, not `vram_free_gb > 0`.

**Collect.** `/v1/*` and `/metrics` share a 2s TTL cache so router probes do
not stack `nvidia-smi`. GPU subprocesses stay at 2s timeout; sysinfo runs in
`spawn_blocking`. A 1s sampler fills CPU%; omit CPU% until the second sample
exists. Windows does not use load average (sysinfo reports 0). `ram_available_source`
is `MemAvailable` on Linux and `sysinfo` elsewhere. Linux PSI `some avg10`
amplifies elevated/critical. Auto GPU policy: NVIDIA inventory → CUDA (including
macOS eGPU); else macOS Metal; else ROCm inventory (`gpus > 0`); else CPU.
Probe `rocm-smi --showmeminfo vram --csv` (PATH, `/opt/rocm/bin/rocm-smi`,
`/usr/bin/rocm-smi`) then `amd-smi metric --mem-usage --csv|--json` (PATH,
`/opt/rocm/bin/amd-smi`). Metal never copies unified RAM into `vram_gb`.

Agent-down is **soft-fail**: node health still follows Ollama `/api/tags`, the
last discovered capacity is retained, `capacity_error` is populated, and routing
degrades to static / `ps_lower_bound` values.

Default probe URL: `http://{ollama-url-host}:11436/...`. Override per node with
fleet.yaml `capacity_url`. On tunneled hosts the router reaches `:11434` and
`:11436` through the private share, not a public or LAN bind.

`GET /v1/status` is how a later router slice can learn `gpu_backend`
(`cpu|cuda|rocm|metal|unknown`) without guessing labels. Metal reports
`gpu_backend=metal` and optional `metal_recommended_gb`; it does **not** fake
CUDA VRAM (`vram_gb=0` on Apple unless a real discrete inventory exists).

Remote / Verda hosts: a **startup script** installs `ollama-node-agent` and
runs `setup`. The router must not SSH.
LAN hosts: operator-installed agent, direct HTTP in `fleet.yaml`. Do not keep
embedding Ubuntu Ollama install logic in the router; Verda bootstrap is a
startup script on the instance.

## Packages

Release workflow [`.github/workflows/release-agent.yml`](../../../.github/workflows/release-agent.yml)
(not `ci.yml`) builds OS-native daemon artifacts. Local: `task agent:release`
(plus `agent:release:linux` / `:deb` / `:macos` / `:windows`). From Linux,
`task agent:release:github` dispatches that workflow and downloads artifacts
into `dist/agent/` (`gh auth login` with repo + workflow scopes, or `GH_TOKEN`;
push the branch/tag first; Darwin/Windows packages). Linux local
recipes run in Docker `rust:1.97-slim-bookworm` (musl static-pie tarball via
`RUSTFLAGS=-C target-feature=+crt-static -C link-self-contained=yes`; gnu `.deb`
is bookworm glibc). GHA compiles on native
`ubuntu-latest` / `ubuntu-24.04-arm` and packages in the same job; the `.deb`
job uses a `rust:1.97-slim-bookworm` container. GHA never installs Task.
`SHA256SUMS.txt` is an Actions artifact on every run, including
`workflow_dispatch`; GitHub Release assets are `v*` tags only.

| Artifact | Role |
| --- | --- |
| `ollama-node-agent-linux-<arch>.tar.gz` | musl static-pie binary + unit + README + optional OpenRC contrib; installer is `sudo ./ollama-node-agent setup` |
| `ollama-node-agent_<ver>_<arch>.deb` | gnu (glibc ≥ bookworm); unit in `/usr/lib/systemd/system`; postinst enables the service when `/run/systemd/system` exists |
| `ollama-node-agent-<ver>-darwin-<arch>.pkg` | component pkg: binary + LaunchDaemon `com.ollama.node-agent` + default config; postinstall `bootout` then `bootstrap`; does **not** install Ollama.app |
| `ollama-node-agent-darwin-<arch>.zip` | unsigned binary for source-style `setup` (not the install path) |
| `ollama-node-agent-windows-amd64.exe` | portable; elevated `setup` registers a LocalSystem SCM service (Ollama + netsh firewall) |
| `ollama-node-agent-<ver>-windows-amd64.msi` | WiX `ServiceInstall` LocalSystem; starts on install / stops+deletes on uninstall; default `config.yaml` with `NeverOverwrite`; does **not** install Ollama or firewall |
| `SHA256SUMS.txt` | checksums of the files above |

Unit, plist, and Windows service name/args live under
`crates/ollama-node-agent/packaging/` and are `include_str!` into `setup`
(`setup --print-unit` prints the systemd unit). Packages do **not** run full
`setup` (that still downloads Ollama). Without `/run/systemd/system`, Linux
`setup` and `.deb` postinst do not bail: they install the binary and print a
`serve` command. OpenRC is contrib-in-tarball only. `uninstall` ignores
`systemctl`/`sc`/`launchctl` failures. The first SCM release deletes leftover
scheduled task `ollama-node-agent`. After MSI, run elevated `setup` for Ollama
and firewall; do not also run the Ollama tray on `:11434` (LocalSystem GPU vs
user tray). Unsigned Windows artifacts may hit SmartScreen. Local
`task agent:release:windows` is a no-op unless the rustc host is
`x86_64-pc-windows-msvc` (do not ship mingw labeled as the GHA exe).
macOS `.pkg` is agent+LaunchDaemon only (`setup` brew-or-fails Ollama). After
`uninstall`, `sudo pkgutil --forget com.ollama.node-agent`. Unsigned pkgs are
OK on a private LAN or private share; Gatekeeper blocks Safari quarantine. Local
`task agent:release:macos` skips unless Darwin (GHA `macos-14` builds both
`aarch64-apple-darwin` and `x86_64-apple-darwin`). pkg upgrades replace the
packaged `config.yaml`.

## Effective capacity

The registry retains configured, discovered, and effective capacities. Effective
fills omitted static fields from discovery. Explicit static VRAM/RAM **cap**
discovered values; explicit GPU/core values **override**. When both are absent,
effective stays unknown. Positive loaded VRAM from `/api/ps` can only provide a
lower bound (`ps_lower_bound`). Admission uses effective capacity; live
loaded/reserved VRAM tightens request admission but does not make placement
transiently ineligible.

## RAM pressure

Each node tracks `pressure_level` (`ok | elevated | critical | unknown`).
**The agent classifies** (worst-signal-wins: available-RAM ratio/GB, swap
amplifier, load-per-CPU on Unix, CPU% after the sampler, Linux PSI
amplifiers). The router **trusts** the wire token via
`PressureLevel::from_wire`. Do not port `classify_pressure` knobs into router
`PolicyConfig`.

No live available-RAM, no usable load, and no CPU% → `unknown` (permissive).
Windows never treats load=0 as a signal.

Critical nodes hard-reject when `reject_on_ram_critical`. Elevated nodes reject
classes in `reject_on_ram_elevated_for_classes` (default medium/large). See
[[concepts/ollama-router-load-share]] for scoring penalties.

Optional `register` heartbeat to the router is **off by default**. Production
membership is fleet.yaml. Heartbeat must be authenticated and must not let a
random laptop join a production router.
