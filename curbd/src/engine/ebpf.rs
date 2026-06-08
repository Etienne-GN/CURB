//! eBPF egress classifier loader (E1).
//!
//! Loads the clang-built classifier (`bpf/curb_cls.bpf.c`) with Aya, attaches
//! it to the interface's egress `clsact` hook, and manages the
//! `cgroup_id -> classid` map. The classifier writes `skb->priority` to the
//! app's HTB class handle so the root HTB qdisc *shapes* (queues) the app's
//! upload smoothly — replacing the drop-based nftables policing for egress.

use std::net::Ipv4Addr;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use aya::maps::HashMap as BpfHashMap;
use aya::programs::tc::{NlOptions, TcAttachOptions};
use aya::programs::{tc, SchedClassifier, TcAttachType};
use aya::Ebpf;
use tracing::info;

/// Netlink priority for the ingress set-priority filter; the `mirred` redirect
/// filter is added by `tc` at a higher number so it runs after this one.
pub const INGRESS_BPF_PRIORITY: u16 = 1;

/// The classifier object, compiled by `build.rs` (empty if clang was absent).
static BPF_OBJ: &[u8] = aya::include_bytes_aligned!(env!("CURB_BPF_OBJ"));

const MAP_NAME: &str = "cgroup_classid";
const FLOW_MAP: &str = "flow_classid";

/// Connection key, byte-for-byte identical to `struct flow_key` in the BPF
/// program. Address/port fields hold raw network-order bytes (as the classifier
/// reads them from the packet), so we store octets/`to_be_bytes` directly.
#[repr(C)]
#[derive(Clone, Copy)]
struct FlowKey {
    saddr: [u8; 4],
    daddr: [u8; 4],
    sport: [u8; 2],
    dport: [u8; 2],
    proto: u8,
    pad: [u8; 3],
}

// Safe: plain old data with no padding-sensitive interpretation (compared
// byte-wise by the kernel as a map key).
unsafe impl aya::Pod for FlowKey {}

/// Owns the loaded program (keeping it attached) and its classid map.
pub struct EbpfShaper {
    bpf: Mutex<Ebpf>,
}

impl EbpfShaper {
    /// Load the classifier and attach the egress hook (always) plus, when
    /// `ingress` is set, the **set-priority-only** ingress filter.
    ///
    /// Egress is self-contained (set `skb->priority`, never drop/redirect) and
    /// safe. The ingress filter (`curb_ingress_setprio`) only sets priority and
    /// returns `TC_ACT_UNSPEC`; it does NOT redirect — a separate `tc mirred`
    /// filter (added by the engine when download shaping is active) performs the
    /// reinjection-correct redirect. The dangerous `bpf_redirect` program is
    /// never attached. `ingress` is gated by an off-by-default flag and must be
    /// validated in a network namespace before use on a real NIC.
    pub fn attach(iface: &str, ingress: bool) -> Result<Self> {
        if BPF_OBJ.is_empty() {
            return Err(anyhow!("no eBPF object (clang unavailable at build time)"));
        }
        let mut bpf = Ebpf::load(BPF_OBJ).context("loading eBPF classifier")?;

        // clsact provides the egress + ingress hooks and coexists with HTB.
        let _ = tc::qdisc_add_clsact(iface);

        let egress: &mut SchedClassifier = bpf
            .program_mut("curb_egress")
            .ok_or_else(|| anyhow!("curb_egress program missing"))?
            .try_into()?;
        egress.load().context("loading curb_egress")?;
        egress
            .attach(iface, TcAttachType::Egress)
            .context("attaching curb_egress")?;

        if ingress {
            let setprio: &mut SchedClassifier = bpf
                .program_mut("curb_ingress_setprio")
                .ok_or_else(|| anyhow!("curb_ingress_setprio program missing"))?
                .try_into()?;
            setprio.load().context("loading curb_ingress_setprio")?;
            setprio
                .attach_with_options(
                    iface,
                    TcAttachType::Ingress,
                    TcAttachOptions::Netlink(NlOptions {
                        priority: INGRESS_BPF_PRIORITY,
                        handle: 0,
                    }),
                )
                .context("attaching curb_ingress_setprio")?;
            info!(iface, "eBPF egress + ingress(set-priority) classifiers attached");
        } else {
            info!(iface, "eBPF egress classifier attached");
        }

        Ok(Self {
            bpf: Mutex::new(bpf),
        })
    }

    /// Map an app's cgroup id to its HTB class handle.
    pub fn set_class(&self, cgroup_id: u64, classid: u32) -> Result<()> {
        let mut bpf = self.bpf.lock().unwrap();
        let mut map: BpfHashMap<_, u64, u32> = BpfHashMap::try_from(
            bpf.map_mut(MAP_NAME).ok_or_else(|| anyhow!("map {MAP_NAME} missing"))?,
        )?;
        map.insert(cgroup_id, classid, 0)?;
        Ok(())
    }

    /// Seed both directions of an existing IPv4 connection into the flow map so
    /// it is classified immediately, without waiting for the cgroup path (which
    /// only catches sockets created after a process joins its cgroup).
    pub fn seed_flow(
        &self,
        local_ip: Ipv4Addr,
        local_port: u16,
        remote_ip: Ipv4Addr,
        remote_port: u16,
        proto: u8,
        classid: u32,
    ) -> Result<()> {
        let mut bpf = self.bpf.lock().unwrap();
        let mut map: BpfHashMap<_, FlowKey, u32> = BpfHashMap::try_from(
            bpf.map_mut(FLOW_MAP).ok_or_else(|| anyhow!("map {FLOW_MAP} missing"))?,
        )?;
        let (l, r) = (local_ip.octets(), remote_ip.octets());
        let (lp, rp) = (local_port.to_be_bytes(), remote_port.to_be_bytes());
        // Outbound key (egress lookup): local -> remote.
        let outbound = FlowKey { saddr: l, daddr: r, sport: lp, dport: rp, proto, pad: [0; 3] };
        // Inbound key (ingress lookup): remote -> local.
        let inbound = FlowKey { saddr: r, daddr: l, sport: rp, dport: lp, proto, pad: [0; 3] };
        map.insert(outbound, classid, 0)?;
        map.insert(inbound, classid, 0)?;
        Ok(())
    }

    /// Remove every entry (called before rebuilding the class set).
    pub fn clear_all(&self) -> Result<()> {
        let mut bpf = self.bpf.lock().unwrap();
        let mut map: BpfHashMap<_, u64, u32> = BpfHashMap::try_from(
            bpf.map_mut(MAP_NAME).ok_or_else(|| anyhow!("map {MAP_NAME} missing"))?,
        )?;
        let keys: Vec<u64> = map.keys().filter_map(|k| k.ok()).collect();
        for k in keys {
            let _ = map.remove(&k);
        }
        Ok(())
    }
}
