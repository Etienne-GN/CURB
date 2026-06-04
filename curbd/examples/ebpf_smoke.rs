//! Smoke test: load the clang-built classifier with Aya, attach it to a
//! clsact egress hook, and write one map entry. Proves the eBPF pipeline works
//! end to end before integrating into the engine.
//!
//! Run as root: `sudo ./target/debug/examples/ebpf_smoke eno1`

use aya::maps::HashMap as BpfHashMap;
use aya::programs::{tc, SchedClassifier, TcAttachType};
use aya::Ebpf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iface = std::env::args().nth(1).unwrap_or_else(|| "eno1".to_string());

    let obj = aya::include_bytes_aligned!(env!("CURB_BPF_OBJ"));
    println!("loading {} bytes of BPF...", obj.len());
    let mut bpf = Ebpf::load(obj)?;
    println!("loaded.");

    // Ensure a clsact qdisc exists (ignore "exists").
    let _ = tc::qdisc_add_clsact(&iface);

    let prog: &mut SchedClassifier = bpf.program_mut("curb_egress").unwrap().try_into()?;
    prog.load()?;
    let link = prog.attach(&iface, TcAttachType::Egress)?;
    println!("attached curb_egress to {iface} egress.");

    // Write a sample cgroup_id -> classid (1:16) entry.
    let mut map: BpfHashMap<_, u64, u32> =
        BpfHashMap::try_from(bpf.map_mut("cgroup_classid").unwrap())?;
    map.insert(0xdead_beefu64, 0x0001_0010u32, 0)?;
    println!("map write ok; entries verified: {}", map.keys().count());

    println!("SUCCESS — eBPF classifier loads, attaches, and the map is writable.");
    // Detach immediately by dropping the link (don't disturb the interface).
    drop(link);
    Ok(())
}
