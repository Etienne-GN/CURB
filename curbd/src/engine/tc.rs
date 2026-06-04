//! Thin wrappers over the `tc` and `ip` command-line tools.
//!
//! Rates are passed to `tc` in **bytes per second** using its `bps` suffix
//! (`bps` = bytes/sec in tc's grammar, *not* bits), so the daemon's internal
//! bytes/sec values map directly with no conversion.

use std::process::Command;

use anyhow::{anyhow, bail, Result};
use tracing::{debug, warn};

/// Locate a binary among common sbin locations, falling back to the bare name
/// (which relies on `PATH`). Daemons often run with a minimal `PATH`.
fn bin(name: &str) -> String {
    for dir in ["/sbin", "/usr/sbin", "/usr/bin", "/bin"] {
        let p = format!("{dir}/{name}");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

fn tc_bin() -> String {
    bin("tc")
}

fn ip_bin() -> String {
    bin("ip")
}

/// Run a command, returning an error if it exits non-zero.
fn run(program: &str, args: &[&str]) -> Result<()> {
    debug!(cmd = %format!("{program} {}", args.join(" ")), "exec");
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| anyhow!("failed to spawn {program}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("{program} {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Run a teardown command, ignoring failures (qdisc/device may not exist).
fn run_ignore(program: &str, args: &[&str]) {
    debug!(cmd = %format!("{program} {}", args.join(" ")), "exec (ignore-errors)");
    if let Ok(out) = Command::new(program).args(args).output() {
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            debug!(stderr = %stderr.trim(), "teardown command non-zero (ignored)");
        }
    }
}

/// Detect the interface used for the default route (e.g. `eno1`).
pub fn default_interface() -> Result<String> {
    let out = Command::new(ip_bin())
        .args(["route", "show", "default"])
        .output()
        .map_err(|e| anyhow!("running `ip route`: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "default via 192.168.0.1 dev eno1 proto dhcp metric 100"
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if let Some(i) = toks.iter().position(|&t| t == "dev") {
            if let Some(dev) = toks.get(i + 1) {
                return Ok((*dev).to_string());
            }
        }
    }
    Err(anyhow!("no default route interface found"))
}

/// Apply an egress (upload) rate cap on `iface`.
pub fn apply_egress(iface: &str, bps: u64) -> Result<()> {
    let rate = format!("{bps}bps");
    let tc = tc_bin();
    run(&tc, &["qdisc", "add", "dev", iface, "root", "handle", "1:", "htb", "default", "1"])?;
    run(
        &tc,
        &[
            "class", "add", "dev", iface, "parent", "1:", "classid", "1:1", "htb", "rate", &rate,
            "ceil", &rate,
        ],
    )?;
    Ok(())
}

/// Apply an ingress (download) rate cap on `iface` via an IFB device.
pub fn apply_ingress(iface: &str, ifb: &str, bps: u64) -> Result<()> {
    let rate = format!("{bps}bps");
    let tc = tc_bin();
    let ip = ip_bin();

    // Create the IFB device (ignore "exists"), then bring it up.
    run_ignore(&ip, &["link", "add", ifb, "type", "ifb"]);
    run(&ip, &["link", "set", "dev", ifb, "up"]).map_err(|e| {
        anyhow!("bringing up {ifb} (is the ifb kernel module available?): {e}")
    })?;

    // Ingress qdisc + redirect all packets to the IFB device.
    run(&tc, &["qdisc", "add", "dev", iface, "handle", "ffff:", "ingress"])?;
    run(
        &tc,
        &[
            "filter", "add", "dev", iface, "parent", "ffff:", "protocol", "all", "u32", "match",
            "u32", "0", "0", "action", "mirred", "egress", "redirect", "dev", ifb,
        ],
    )?;

    // Shape the IFB device's egress (which is iface's redirected ingress).
    run(&tc, &["qdisc", "add", "dev", ifb, "root", "handle", "1:", "htb", "default", "1"])?;
    run(
        &tc,
        &[
            "class", "add", "dev", ifb, "parent", "1:", "classid", "1:1", "htb", "rate", &rate,
            "ceil", &rate,
        ],
    )?;
    Ok(())
}

/// Remove the egress root qdisc (reverts to the kernel default).
pub fn clear_egress(iface: &str) {
    run_ignore(&tc_bin(), &["qdisc", "del", "dev", iface, "root"]);
}

/// Remove the ingress qdisc and tear down the IFB device.
pub fn clear_ingress(iface: &str, ifb: &str) {
    let tc = tc_bin();
    run_ignore(&tc, &["qdisc", "del", "dev", iface, "ingress"]);
    run_ignore(&tc, &["qdisc", "del", "dev", ifb, "root"]);
    run_ignore(&ip_bin(), &["link", "del", ifb]);
}

/// Verify the `ifb` module / device can be created; warn early if not.
#[allow(dead_code)]
pub fn ifb_available(ifb: &str) -> bool {
    let ip = ip_bin();
    if run(&ip, &["link", "add", ifb, "type", "ifb"]).is_ok() {
        run_ignore(&ip, &["link", "del", ifb]);
        true
    } else {
        warn!("ifb device unavailable; download limiting will not work");
        false
    }
}
