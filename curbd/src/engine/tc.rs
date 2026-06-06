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

/// IPv4 LAN/private ranges (loopback + RFC1918 + CGNAT + link-local).
const V4_LAN: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
];
/// IPv6 LAN/private ranges (loopback + ULA + link-local).
const V6_LAN: &[&str] = &["::1/128", "fc00::/7", "fe80::/10"];

// HTB class layout under root `1:`:
//   1:1  total ("all") parent cap
//   1:2  LAN leaf       (child of 1:1)
//   1:3  Internet leaf  (child of 1:1, also the qdisc default)
//   1:10+ per-app leaves (children of 1:1, steered by the eBPF classifier)
const CLS_TOTAL: &str = "1:1";
const CLS_LAN: &str = "1:2";
const CLS_INET: &str = "1:3";

/// Add `u32` filters that send LAN-addressed packets to the LAN class. `field`
/// is `dst` for egress (remote = destination) or `src` for ingress.
fn add_lan_filters(dev: &str, field: &str) -> Result<()> {
    let tc = tc_bin();
    for cidr in V4_LAN {
        run(&tc, &[
            "filter", "add", "dev", dev, "parent", "1:", "protocol", "ip", "prio", "1", "u32",
            "match", "ip", field, cidr, "flowid", CLS_LAN,
        ])?;
    }
    for cidr in V6_LAN {
        run(&tc, &[
            "filter", "add", "dev", dev, "parent", "1:", "protocol", "ipv6", "prio", "1", "u32",
            "match", "ip6", field, cidr, "flowid", CLS_LAN,
        ])?;
    }
    Ok(())
}

/// Build the scoped HTB tree (total → LAN + Internet + per-app) on a device.
///
/// `field` selects the remote-address direction (`dst` egress / `src` ingress).
/// `apps` is the list of per-app `(class minor, rate)` leaves to create under
/// the total class (empty when the eBPF classifier isn't steering this device).
fn build_tree(
    dev: &str,
    field: &str,
    total: Option<u64>,
    lan: Option<u64>,
    inet: Option<u64>,
    apps: &[(u16, u64)],
) -> Result<()> {
    let tc = tc_bin();
    let total = total.unwrap_or(LINE_RATE_BPS);
    let rate = |bps: u64| format!("{bps}bps");
    let ceil = rate(total);

    // default 3 -> unmatched (non-app, non-LAN) traffic goes to the Internet leaf.
    run(&tc, &["qdisc", "add", "dev", dev, "root", "handle", "1:", "htb", "default", "3"])?;
    run(&tc, &[
        "class", "add", "dev", dev, "parent", "1:", "classid", CLS_TOTAL, "htb", "rate",
        &rate(total), "ceil", &ceil,
    ])?;
    for (classid, cap) in [(CLS_LAN, lan), (CLS_INET, inet)] {
        run(&tc, &[
            "class", "add", "dev", dev, "parent", CLS_TOTAL, "classid", classid, "htb", "rate",
            &rate(cap.unwrap_or(total)), "ceil", &ceil,
        ])?;
    }
    for (minor, cap) in apps {
        let classid = format!("1:{minor:x}");
        run(&tc, &[
            "class", "add", "dev", dev, "parent", CLS_TOTAL, "classid", &classid, "htb", "rate",
            &rate(*cap), "ceil", &ceil,
        ])?;
    }
    add_lan_filters(dev, field)
}

/// Build the scoped egress (upload) HTB tree on the interface.
pub fn build_egress_tree(
    iface: &str,
    total_up: Option<u64>,
    lan_up: Option<u64>,
    inet_up: Option<u64>,
    apps: &[(u16, u64)],
) -> Result<()> {
    build_tree(iface, "dst", total_up, lan_up, inet_up, apps)
}

/// Bring up the IFB device and build the scoped ingress (download) HTB tree,
/// then redirect the interface's ingress to it with `mirred` (the P2-proven,
/// reinjection-correct redirect).
pub fn build_ingress_scoped(
    iface: &str,
    ifb: &str,
    total_down: Option<u64>,
    lan_down: Option<u64>,
    inet_down: Option<u64>,
    apps: &[(u16, u64)],
) -> Result<()> {
    let tc = tc_bin();
    let ip = ip_bin();
    run_ignore(&ip, &["link", "add", ifb, "type", "ifb"]);
    run(&ip, &["link", "set", "dev", ifb, "up"])
        .map_err(|e| anyhow!("bringing up {ifb}: {e}"))?;

    build_tree(ifb, "src", total_down, lan_down, inet_down, apps)?;

    // Redirect the interface's ingress to the IFB device.
    run(&tc, &["qdisc", "add", "dev", iface, "handle", "ffff:", "ingress"])?;
    run(&tc, &[
        "filter", "add", "dev", iface, "parent", "ffff:", "protocol", "all", "u32", "match",
        "u32", "0", "0", "action", "mirred", "egress", "redirect", "dev", ifb,
    ])?;
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
