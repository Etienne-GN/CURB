# CLAUDE.md

Developer guide for Claude Code working in this repo.

## Project overview

CURB (Connection Usage & Rate Balancer) is a Linux desktop app for per-application
bandwidth monitoring and control — think NetLimiter for Linux. It consists of:

| Component | Path | Role |
|---|---|---|
| `curbd` | `curbd/` | Privileged daemon. Owns eBPF programs, tc/HTB, cgroups, nftables rules, and the Unix control socket. |
| `curb` | `cli/` | Unprivileged CLI client of `curbd`. |
| `curb-gui` | `gui/` | Tauri v2 desktop app. Rust backend = thin `curbd` client; frontend = vanilla JS/CSS/HTML (no build step). |
| `curb-proto` | `proto/` | Shared IPC types + framed-codec client library. |
| eBPF | `curbd/bpf/` | C source compiled by `build.rs` via clang at build time; embedded in `curbd` binary. |

The daemon runs as root via a systemd service. The socket (`/run/curbd.sock`)
is group-owned by `curb`, mode 0660. CLI and GUI binaries have the setgid bit
(`g+s`, group `curb`) so they work without the user being in the group in their
current session.

## Build commands

```sh
# Daemon + CLI (debug)
cargo build -p curbd -p curb

# Daemon + CLI (release)
cargo build --release -p curbd -p curb

# GUI (debug, hot-reload)
make gui                 # wraps: sg curb -c "cd gui && tauri dev"

# GUI (release bundle)
cd gui && cargo tauri build

# Run daemon (needs root for eBPF/tc/cgroups)
make daemon              # wraps: cargo build -p curbd && sudo ./target/debug/curbd

# Install system-wide
sudo ./packaging/install.sh

# Quick health check
make check               # curb ping against running daemon
```

## Dev workflow notes

- The GUI frontend is plain `src/index.html` + `src/styles.css` + `src/main.js` —
  no npm build step. Edit these files and Tauri's dev server hot-reloads instantly.
- GridStack v10 (`src/gridstack-all.js`, `src/gridstack.min.css`) is bundled
  locally — do not load it from CDN.
- The eBPF object (`curbd/bpf/curb_cls.bpf.o`) is compiled by `build.rs` if
  clang is available. If clang is absent the daemon starts without eBPF shaping.
- To run the GUI without `make gui`, use: `sg curb -c "CURB_SOCK=/run/curbd.sock
  ./node_modules/.bin/tauri dev"` from inside `gui/`.

## Architecture details

### Process → cgroup attribution
`curbd` creates a cgroup v2 directory under `/sys/fs/cgroup/curb/<exe-hash>/`
per managed application. PIDs are written to `cgroup.procs`. New processes are
detected via proc-connector (Netlink `CN_IDX_PROC`, `PROC_EVENT_EXEC`) in under
a millisecond, with a 5-second full `/proc` scan as fallback.

### eBPF classification
- **Egress**: `curb_egress` (sched_cls, clsact) reads `cgroup_classid` map,
  sets `skb->priority` to the HTB class handle.
- **Ingress mark**: `curb_ingress_setmark` (sched_cls, clsact ingress) reads
  `flow_classid` map, sets `skb->mark` to classid. nftables polices by mark.
- **Ingress priority** (off by default, `CURB_EBPF_INGRESS=1`): sets
  `skb->priority` for IFB-based smooth shaping.

### Aya 0.13 link lifetime bug
`SchedClassifier::attach()` returns a `SchedClassifierLink` that **detaches when
dropped**. The `EbpfShaper` struct stores the link as
`Box<dyn Any + Send + Sync>` to keep it alive. Do NOT discard the return value
of `attach()` — the filter will silently disappear and enforcement will break.

### nftables rules
Rules live in table `inet curb`, chain `input`. Per-app policing:
```
add rule inet curb input meta mark {classid} limit rate over {rate} bytes/second burst {burst} bytes drop
```

## CRITICAL safety rules

These rules exist because of past incidents. Do not bypass them.

1. **NEVER use `bpf_redirect()` from the clsact ingress hook on a live NIC.**
   It blackholed all inbound traffic and required a hard reboot. Always use
   `tc mirred` redirect for ingress redirection.

2. **Test ingress/IFB eBPF paths in an isolated network namespace first.**
   Use `nsenter --net=...` (NOT `ip netns exec` — it shadows cgroup namespaces).
   Run a connectivity check (`curl -s -o /dev/null -w "HTTP %{http_code}"`) at
   every step before and after touching the live NIC.

3. **NEVER `pkill -f target/debug/curbd`.** It matches the shell's own argv
   string. Use `pkill -x curbd` to kill only the exact binary.

4. **`bpf_sk_cgroup_id` / `bpf_sk_lookup` are NOT available in tc/sched_cls
   programs.** The verifier rejects them with "unknown func". Use the
   `cgroup_classid` BPF map keyed by cgroup ID instead.

5. **The `CURB_EBPF_INGRESS=1` flag is off by default for safety.** Do not
   enable it on a real interface without prior namespace testing.

## Key files

| File | Notes |
|---|---|
| `curbd/src/engine/mod.rs` | Main engine: starts/stops eBPF, HTB, nftables; reconciler loop |
| `curbd/src/engine/ebpf.rs` | Aya loader; stores `_ingress_link` to prevent detach |
| `curbd/src/engine/tc.rs` | HTB/IFB setup via shell-out to `tc` |
| `curbd/src/engine/nft.rs` | nftables rule management |
| `curbd/src/engine/cgroup.rs` | cgroup v2 creation and PID placement |
| `curbd/src/engine/proc_connector.rs` | Netlink proc-connector listener |
| `curbd/src/quota.rs` | Quota tracking, enforcement, and reset logic |
| `curbd/bpf/curb_cls.bpf.c` | eBPF C source (egress + ingress programs) |
| `proto/src/lib.rs` | IPC wire types and framed client |
| `gui/src/main.js` | Entire GUI frontend (polling, rendering, widget system) |
| `gui/src/styles.css` | All GUI styles including theme CSS variables |
| `packaging/install.sh` | System installer (group, binaries, setgid, systemd) |
| `packaging/curbd.service` | systemd unit for the daemon |
