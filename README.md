# CURB — Connection Usage & Rate Balancer

Per-application network bandwidth monitoring and control for Linux. Think
[NetLimiter](https://www.netlimiter.com/) — but native, open-source, and built
on eBPF.

![CURB GUI](design/gui-live.png)

---

## Features

- **Live per-app monitoring** — real-time download/upload rates with 60-second
  sparkline history for every running application
- **Per-app rate limits** — independent download and upload caps, shaped smoothly
  via HTB queuing (no packet drops on egress)
- **Data quotas** — daily/weekly/monthly byte budgets; automatically blocks an app
  when its budget is exceeded
- **LAN vs Internet scoping** — apply limits to local-network traffic, internet
  traffic, or both, independently
- **Master toggle** — enable/disable all shaping instantly without losing rules
- **Host-wide limits** — cap total machine-wide bandwidth independent of per-app rules
- **Connections view** — live table of every active TCP/UDP socket with PID and app
- **Desktop GUI** — draggable/resizable widget dashboard, pinned-app quick
  controls, 13 built-in themes + custom JSON themes
- **CLI** — `curb apps`, `curb app firefox limit --down 5mbit`, `curb top`, etc.

---

## Requirements

| Requirement | Notes |
|---|---|
| Linux kernel ≥ 5.10 | BTF required — `/sys/kernel/btf/vmlinux` must exist |
| cgroup v2 | Default on all modern distros |
| `clang` | eBPF compilation at build time |
| `nftables` | Runtime — `nft` in `$PATH` |
| `iproute2` | Runtime — `tc` and `ip` in `$PATH` |
| Root (daemon only) | `curbd` runs via systemd; GUI and CLI are unprivileged |

---

## Install

```sh
# 1. Build release binaries
cargo build --release -p curbd -p curb
cd gui && cargo tauri build && cd ..

# 2. Install system-wide
sudo ./packaging/install.sh
```

The install script creates a `curb` system group, installs all binaries, sets
the setgid bit so the GUI and CLI work immediately from any terminal or desktop
shortcut (no logout/login required), and enables the `curbd` systemd service.

**Build deps (Debian/Ubuntu):**
```sh
sudo apt install clang pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
     librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev nftables iproute2
```

---

## Development

```sh
# Terminal 1 — daemon (root required for eBPF/tc/cgroups)
make daemon

# Terminal 2 — GUI with hot-reload
make gui

# CLI against the running daemon
sg curb -c "./target/debug/curb apps"
sg curb -c "./target/debug/curb app /opt/google/chrome/chrome limit --down 10mbit"
```

---

## Architecture

```
┌─────────────┐  Unix socket  ┌─────────────────────────────────────────┐
│  curb (CLI) │──────────────▶│  curbd  (privileged daemon)             │
│  curb-gui   │──────────────▶│                                         │
└─────────────┘               │  eBPF tc classifier (Aya)               │
                              │    cgroup→classid + flow→classid maps   │
                              │    ingress set-mark program              │
                              │                                         │
                              │  tc HTB        (egress shaping)         │
                              │  nftables      (ingress policing)       │
                              │  cgroup v2     (per-app process groups) │
                              │  proc-connector (exec hook, <1ms)       │
                              └─────────────────────────────────────────┘
```

1. `curbd` creates a cgroup v2 per managed app and places all its PIDs there.
   New processes are detected in under a millisecond via the kernel's
   proc-connector Netlink interface.
2. An Aya eBPF `tc` classifier maps each packet's socket cgroup to its HTB
   classid, writing it to `skb->priority` (egress) or `skb->mark` (ingress).
3. **Egress** — HTB on the real NIC shapes upload smoothly (queuing, not dropping).
4. **Ingress** — nftables polices by `meta mark`, dropping excess per-app packets.
5. The daemon samples eBPF accounting maps every second for live rates and totals.

---

## CLI reference

```sh
curb apps                                          # live per-app rate table
curb top                                           # live view (updates in-place)
curb app <exe> limit --down 5mbit --up 2mbit       # set rate limits
curb app <exe> limit --down 5mbit --scope internet # internet traffic only
curb app <exe> unlimit                             # remove limits
curb quota set <exe> --budget 1GB --period daily   # set data quota
curb quota list                                    # show quota status
curb quota clear <exe>                             # remove quota
curb host limit --down 100mbit --up 20mbit         # host-wide cap
curb host off                                      # remove host cap
curb limiter on|off                                # master toggle
curb ping                                          # check daemon health
```

---

## eBPF ingress shaping (advanced / experimental)

Smooth per-app download shaping via IFB + HTB is implemented but off by default.
Enable with `CURB_EBPF_INGRESS=1 curbd`. Test in an isolated network namespace
first — see the warning in [the engine source](curbd/src/engine/ebpf.rs).

---

## Themes

The GUI ships 13 built-in themes. To create a custom theme see
[CREATING_THEMES.md](CREATING_THEMES.md).

---

## License

MIT
