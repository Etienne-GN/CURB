//! eBPF egress classifier loader (E1).
//!
//! Loads the clang-built classifier (`bpf/curb_cls.bpf.c`) with Aya, attaches
//! it to the interface's egress `clsact` hook, and manages the
//! `cgroup_id -> classid` map. The classifier writes `skb->priority` to the
//! app's HTB class handle so the root HTB qdisc *shapes* (queues) the app's
//! upload smoothly — replacing the drop-based nftables policing for egress.

use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use aya::maps::HashMap as BpfHashMap;
use aya::programs::{tc, SchedClassifier, TcAttachType};
use aya::Ebpf;
use tracing::info;

/// The classifier object, compiled by `build.rs` (empty if clang was absent).
static BPF_OBJ: &[u8] = aya::include_bytes_aligned!(env!("CURB_BPF_OBJ"));

const MAP_NAME: &str = "cgroup_classid";

/// Owns the loaded program (keeping it attached) and its classid map.
pub struct EbpfShaper {
    bpf: Mutex<Ebpf>,
}

impl EbpfShaper {
    /// Load the classifier and attach it to `iface` egress.
    pub fn attach_egress(iface: &str) -> Result<Self> {
        if BPF_OBJ.is_empty() {
            return Err(anyhow!("no eBPF object (clang unavailable at build time)"));
        }
        let mut bpf = Ebpf::load(BPF_OBJ).context("loading eBPF classifier")?;

        // clsact coexists with the root HTB qdisc and provides the egress hook.
        let _ = tc::qdisc_add_clsact(iface);

        let prog: &mut SchedClassifier = bpf
            .program_mut("curb_egress")
            .ok_or_else(|| anyhow!("curb_egress program missing from object"))?
            .try_into()?;
        prog.load().context("loading curb_egress")?;
        prog.attach(iface, TcAttachType::Egress)
            .context("attaching curb_egress to egress")?;

        info!(iface, "eBPF egress classifier attached");
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
