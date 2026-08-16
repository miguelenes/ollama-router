# Before → After (apply notes)

| Surface | Before | After (stable) |
|--------|--------|----------------|
| rustc / rust-version | 1.97 (local 1.97.1) | 1.97 (keep; 1.98 beta-only) |
| Docker rust | `rust:1.97-slim-bookworm` | `rust:1.97.1-slim-bookworm` |
| Debian runtime | `debian:bookworm-slim` | `debian:bookworm-slim` (trixie available; keep glibc floor) |
| tower-http | 0.6 | 0.7 |
| reqwest | 0.12 | 0.13 |
| rusqlite | 0.37 | 0.40 |
| md-5 / sha2 / base64 / sysinfo | 0.10 / 0.10 / 0.22 / 0.35 | 0.11 / 0.11 / 0.23 / 0.39 |
| Prometheus | v3.2.1 | v3.13.2 |
| Alertmanager | v0.28.1 | v0.34.0 |
| Loki | 3.4.2 | 3.7.6 |
| Alloy | v1.9.2 | v1.18.1 |
| Grafana | 11.6.3 | 13.1.3 |
| NFPM | 2.47.0 | 2.47.0 (already latest) |
| download-artifact | @v7 | @v8 |
| softprops/action-gh-release | @v2 | @v3 |
| UI packages | `"latest"` floats | concrete npm stables (React 19.2.x, Vite 8.x, TS 7.x) |

Deferred: rustc 1.98 (beta). Edition remains 2021.
