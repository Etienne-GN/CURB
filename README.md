# CURB

**C**onnection **U**sage & **R**ate **B**alancer — NetLimiter-style, per-application
bandwidth monitoring and control for Linux.

CURB lets you watch and limit network bandwidth **per application**, live, with
separate inbound/outbound controls, host-wide caps, LAN-vs-internet scoping, and
data quotas. It ships as a CLI (`curb`) and a dark-themed desktop GUI.

> Status: early development. See [the build plan](#roadmap) for what works today.

## Architecture

| Component | Crate / dir | Role |
|-----------|-------------|------|
| Contract  | `proto/` (`curb-proto`) | The frozen IPC wire protocol + client library shared by every component. |
| Engine    | `curbd/` | Privileged daemon. Owns the control socket and (from P2) the eBPF/tc/cgroup traffic engine. |
| CLI       | `cli/` (`curb`) | Command-line client. |
| GUI       | `gui/` *(later)* | Tauri app: Rust backend = thin client of `curbd`; web frontend = the UI. |

The daemon runs privileged (systemd service) and listens on a Unix socket
(`/run/curbd.sock`, mode `0660`, group `curb`). The CLI and GUI are unprivileged
clients of that socket.

## Build

```sh
cargo build            # builds curb-proto, curbd, curb
```

Requirements for the engine (later phases): Linux kernel ≥ 5.10 with BTF
(`/sys/kernel/btf/vmlinux`), cgroup v2, and `CAP_NET_ADMIN`/`CAP_BPF`.

## Try it (P0)

Run the daemon and talk to it over a dev socket (no root needed):

```sh
CURB_SOCK=/tmp/curbd.sock cargo run -p curbd            # terminal 1
CURB_SOCK=/tmp/curbd.sock cargo run -p curb -- ping     # terminal 2
CURB_SOCK=/tmp/curbd.sock cargo run -p curb -- status
```

## Roadmap

- **P0 — Scaffold** ✅ workspace, IPC contract, daemon + CLI skeletons, `ping`/`status`.
- **P1 — Live monitoring** eBPF accounting + per-app attribution, `curb top`, first GUI pass.
- **P2 — Host-wide limits** tc HTB + IFB; live on/off and rate changes; in/out split.
- **P3 — Per-app limits** cgroup + eBPF classifier → HTB; the core feature.
- **P4 — LAN vs Internet** per-rule direction × scope.
- **P5 — Quotas** cumulative per-app accounting + enforcement.
- **P6 — GUI polish** dark theme, graphs, rule editor.

## License

MIT
