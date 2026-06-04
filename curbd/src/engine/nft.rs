//! nftables policing for per-application limits (P3).
//!
//! Builds one `inet curb` table with `input`/`output` chains. For each app rule
//! we add `socket cgroupv2 ... limit rate over <cap> bytes/second ... drop`,
//! matched in the hook where the owning socket is known (input for download,
//! output for upload). This *polices* (drops excess) rather than *shapes*
//! (queues), so throughput sawtooths around the cap — acceptable for v1; a
//! future eBPF classifier can provide smooth shaping.
//!
//! The whole table is regenerated atomically on every change via `nft -f -`.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Result};
use tracing::debug;

use super::cgroup;

/// One app's policing parameters for the ruleset generator.
pub struct AppRule {
    /// App cgroup path relative to the cgroup root (e.g. `curb/firefox-3f9a`).
    pub cgroup_rel: String,
    pub down_bps: Option<u64>,
    pub up_bps: Option<u64>,
}

fn nft_bin() -> String {
    for p in ["/usr/sbin/nft", "/sbin/nft", "/usr/bin/nft"] {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    "nft".to_string()
}

/// Token-bucket burst sized to smooth short spikes without blowing the cap:
/// a quarter-second of traffic, floored at 64 KiB.
fn burst(bps: u64) -> u64 {
    (bps / 4).max(64 * 1024)
}

/// Regenerate the entire `inet curb` table from the given app rules.
pub fn apply(rules: &[AppRule]) -> Result<()> {
    let mut s = String::new();
    // Idempotent: create the table, then flush it before re-adding chains/rules.
    s.push_str("add table inet curb\n");
    s.push_str("flush table inet curb\n");
    s.push_str("add chain inet curb input { type filter hook input priority -150; }\n");
    s.push_str("add chain inet curb output { type filter hook output priority -150; }\n");

    for r in rules {
        if let Some(down) = r.down_bps {
            s.push_str(&format!(
                "add rule inet curb input socket cgroupv2 level {lvl} \"{cg}\" \
                 limit rate over {rate} bytes/second burst {burst} bytes drop\n",
                lvl = cgroup::APP_LEVEL,
                cg = r.cgroup_rel,
                rate = down,
                burst = burst(down),
            ));
        }
        if let Some(up) = r.up_bps {
            s.push_str(&format!(
                "add rule inet curb output socket cgroupv2 level {lvl} \"{cg}\" \
                 limit rate over {rate} bytes/second burst {burst} bytes drop\n",
                lvl = cgroup::APP_LEVEL,
                cg = r.cgroup_rel,
                rate = up,
                burst = burst(up),
            ));
        }
    }
    run_script(&s)
}

/// Remove the CURB nftables table entirely.
pub fn clear() {
    let _ = Command::new(nft_bin())
        .args(["delete", "table", "inet", "curb"])
        .output();
}

/// Feed an nft script to `nft -f -` over stdin.
fn run_script(script: &str) -> Result<()> {
    debug!(script = %script, "applying nft ruleset");
    let mut child = Command::new(nft_bin())
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("spawning nft: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("nft stdin unavailable"))?
        .write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("nft failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
