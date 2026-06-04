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

/// Effective "unlimited" rate for the default class (~80 Gbit/s in bytes/sec).
pub const LINE_RATE_BPS: u64 = 10_000_000_000;

/// Build the egress HTB root with a default class (the host upload cap, or
/// line rate when unlimited). Per-app classes are added under it with
/// [`egress_app_class`]; the eBPF classifier steers each app's packets into its
/// class via `skb->priority`. Used for the eBPF shaping path.
pub fn egress_root(iface: &str, host_up_bps: Option<u64>) -> Result<()> {
    let tc = tc_bin();
    let default_rate = host_up_bps.unwrap_or(LINE_RATE_BPS);
    let rate = format!("{default_rate}bps");
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

/// Add a per-app egress HTB class `1:<minor>` at `rate_bps` (ceil `ceil_bps`).
pub fn egress_app_class(iface: &str, minor: u16, rate_bps: u64, ceil_bps: u64) -> Result<()> {
    let tc = tc_bin();
    let classid = format!("1:{minor:x}");
    let rate = format!("{rate_bps}bps");
    let ceil = format!("{ceil_bps}bps");
    run(
        &tc,
        &[
            "class", "add", "dev", iface, "parent", "1:", "classid", &classid, "htb", "rate",
            &rate, "ceil", &ceil,
        ],
    )?;
    Ok(())
}

/// Remove the clsact qdisc (detaches the eBPF classifier).
pub fn clear_clsact(iface: &str) {
    run_ignore(&tc_bin(), &["qdisc", "del", "dev", iface, "clsact"]);
}

/// Priority of the `mirred` ingress-redirect filter (runs after the eBPF
/// set-priority filter at priority 1).
const INGRESS_REDIRECT_PRIO: &str = "2";

/// Add the `mirred` ingress redirect to the IFB device on the clsact ingress
/// hook. This is the reinjection-correct redirect (as the P2 host path uses);
/// the eBPF set-priority filter at a lower priority has already classified the
/// packet by then.
pub fn add_ingress_redirect(iface: &str, ifb: &str) -> Result<()> {
    run(
        &tc_bin(),
        &[
            "filter", "add", "dev", iface, "ingress", "pref", INGRESS_REDIRECT_PRIO, "matchall",
            "action", "mirred", "egress", "redirect", "dev", ifb,
        ],
    )
}

/// Remove the `mirred` ingress redirect filter (stops sending ingress to IFB).
pub fn del_ingress_redirect(iface: &str) {
    run_ignore(
        &tc_bin(),
        &["filter", "del", "dev", iface, "ingress", "pref", INGRESS_REDIRECT_PRIO],
    );
}

/// Bring up the IFB device and build its HTB root for the eBPF ingress path.
///
pub fn ifb_root(ifb: &str, host_down_bps: Option<u64>) -> Result<()> {
    let tc = tc_bin();
    let ip = ip_bin();
    run_ignore(&ip, &["link", "add", ifb, "type", "ifb"]);
    run(&ip, &["link", "set", "dev", ifb, "up"])
        .map_err(|e| anyhow!("bringing up {ifb} (ifb module available?): {e}"))?;

    let default_rate = host_down_bps.unwrap_or(LINE_RATE_BPS);
    let rate = format!("{default_rate}bps");
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

/// Add a per-app ingress HTB class `1:<minor>` on the IFB device.
pub fn ifb_app_class(ifb: &str, minor: u16, rate_bps: u64, ceil_bps: u64) -> Result<()> {
    let tc = tc_bin();
    let classid = format!("1:{minor:x}");
    let rate = format!("{rate_bps}bps");
    let ceil = format!("{ceil_bps}bps");
    run(
        &tc,
        &[
            "class", "add", "dev", ifb, "parent", "1:", "classid", &classid, "htb", "rate", &rate,
            "ceil", &ceil,
        ],
    )?;
    Ok(())
}

/// Tear down the IFB device used by the eBPF ingress path.
pub fn ifb_clear(ifb: &str) {
    let tc = tc_bin();
    run_ignore(&tc, &["qdisc", "del", "dev", ifb, "root"]);
    run_ignore(&ip_bin(), &["link", "del", ifb]);
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
