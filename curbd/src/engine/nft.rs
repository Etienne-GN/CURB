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

use curb_proto::Scope;

use super::cgroup;

/// Private/LAN address ranges (loopback + RFC1918 + CGNAT + link-local), IPv4.
const V4_PRIVATE: &str =
    "127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, 100.64.0.0/10";
/// Private/LAN address ranges (loopback + ULA + link-local), IPv6.
const V6_PRIVATE: &str = "::1, fc00::/7, fe80::/10";

/// One app's policing parameters for the ruleset generator.
pub struct AppRule {
    /// App cgroup path relative to the cgroup root (e.g. `curb/firefox-3f9a`).
    pub cgroup_rel: String,
    pub down_bps: Option<u64>,
    pub up_bps: Option<u64>,
    /// Restrict the rule to LAN, Internet, or both remotes.
    pub scope: Scope,
    /// When set, drop all of the app's traffic (quota exceeded, P5). Takes
    /// precedence over the rate caps.
    pub blocked: bool,
}

/// Build the address-match clause(s) for a scope+direction, one per IP family.
///
/// `field` is the remote-address field for the direction: `saddr` for inbound
/// (download, input hook), `daddr` for outbound (upload, output hook). Returns
/// a clause per family; `Both` yields a single empty clause (match all).
fn scope_clauses(scope: Scope, field: &str) -> Vec<String> {
    match scope {
        Scope::Both => vec![String::new()],
        Scope::Lan => vec![
            format!("ip {field} {{ {V4_PRIVATE} }} "),
            format!("ip6 {field} {{ {V6_PRIVATE} }} "),
        ],
        Scope::Internet => vec![
            format!("ip {field} != {{ {V4_PRIVATE} }} "),
            format!("ip6 {field} != {{ {V6_PRIVATE} }} "),
        ],
    }
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
        // Quota exceeded: drop everything for this app, ignore rate caps.
        if r.blocked {
            s.push_str(&format!(
                "add rule inet curb input socket cgroupv2 level {lvl} \"{cg}\" drop\n",
                lvl = cgroup::APP_LEVEL,
                cg = r.cgroup_rel,
            ));
            s.push_str(&format!(
                "add rule inet curb output socket cgroupv2 level {lvl} \"{cg}\" drop\n",
                lvl = cgroup::APP_LEVEL,
                cg = r.cgroup_rel,
            ));
            continue;
        }
        // Download: police inbound at the input hook; remote is the source.
        if let Some(down) = r.down_bps {
            for clause in scope_clauses(r.scope, "saddr") {
                s.push_str(&format!(
                    "add rule inet curb input socket cgroupv2 level {lvl} \"{cg}\" \
                     {clause}limit rate over {rate} bytes/second burst {burst} bytes drop\n",
                    lvl = cgroup::APP_LEVEL,
                    cg = r.cgroup_rel,
                    rate = down,
                    burst = burst(down),
                ));
            }
        }
        // Upload: police outbound at the output hook; remote is the destination.
        if let Some(up) = r.up_bps {
            for clause in scope_clauses(r.scope, "daddr") {
                s.push_str(&format!(
                    "add rule inet curb output socket cgroupv2 level {lvl} \"{cg}\" \
                     {clause}limit rate over {rate} bytes/second burst {burst} bytes drop\n",
                    lvl = cgroup::APP_LEVEL,
                    cg = r.cgroup_rel,
                    rate = up,
                    burst = burst(up),
                ));
            }
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
