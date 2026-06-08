//! Live per-application traffic monitor (P1).
//!
//! Wires three background threads:
//! 1. **capture** — reads packets from [`capture`], attributes each to an
//!    application via the current [`ProcMap`], and accumulates byte totals.
//! 2. **refresher** — rebuilds the [`ProcMap`] on an interval and swaps it in.
//! 3. **sampler** — once per second, turns accumulated totals into per-second
//!    rates + sparkline history and publishes a [`MonitorSnapshot`].
//!
//! Everything is plain threads + `std` sync primitives; the daemon's tokio
//! tasks only ever read the published snapshot.

mod capture;
mod procmap;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use curb_proto::{AppStat, AppStatus, HostTotals, MonitorSnapshot};
use tracing::{info, warn};

use procmap::ProcMap;

/// How many recent rate samples to retain per app for sparklines.
const SPARK_LEN: usize = 60;
/// Sampler tick; also the denominator for rate computation (1s ⇒ bytes/sec).
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the `/proc` attribution map is rebuilt. Kept short so short-lived
/// connections are caught before they close.
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// Cap on the resolved-flow cache (see [`capture_loop`]); cleared when exceeded.
const FLOW_CACHE_MAX: usize = 16_384;
/// Drop an app after this many consecutive idle samples with no live pids.
const IDLE_EVICT: u32 = 60;

/// Per-application running accumulator (capture thread writes, sampler reads).
#[derive(Default)]
struct AppAccum {
    down_total: u64,
    up_total: u64,
    last_down: u64,
    last_up: u64,
    down_spark: VecDeque<u32>,
    up_spark: VecDeque<u32>,
    idle: u32,
}

/// All accumulated byte counters, behind one mutex.
/// A pair of cumulative byte counters plus the previous-sample values (so the
/// sampler can compute per-second rates).
#[derive(Default)]
struct Counter {
    down: u64,
    up: u64,
    last_down: u64,
    last_up: u64,
}

impl Counter {
    /// (down_bps, up_bps) since the last call; advances the cursor.
    fn rates(&mut self) -> (u64, u64) {
        let d = self.down - self.last_down;
        let u = self.up - self.last_up;
        self.last_down = self.down;
        self.last_up = self.up;
        (d, u)
    }
}

#[derive(Default)]
struct Accum {
    /// Keyed by executable path; empty string = unresolved traffic.
    apps: HashMap<String, AppAccum>,
    host: Counter,
    /// Host traffic split by remote-address zone.
    lan: Counter,
    internet: Counter,
}

/// A network prefix (CIDR) for LAN classification.
#[derive(Clone, Copy)]
struct Cidr {
    net: std::net::IpAddr,
    prefix: u8,
}

impl Cidr {
    fn contains(&self, ip: std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        match (self.net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix.min(32)) };
                (u32::from(n) & mask) == (u32::from(a) & mask)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix.min(128)) };
                (u128::from(n) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }
}

/// The set of address ranges treated as "local network": the fixed private
/// ranges plus the machine's directly-connected subnets (so a LAN peer on a
/// global IPv6 prefix from your ISP is still LAN, not Internet).
#[derive(Default)]
struct LanRanges {
    subnets: Vec<Cidr>,
}

impl LanRanges {
    fn contains(&self, ip: std::net::IpAddr) -> bool {
        self.subnets.iter().any(|c| c.contains(ip))
    }
}

/// Parse a CIDR like `192.168.0.0/24` or a bare address (`::1`).
fn parse_cidr(s: &str) -> Option<Cidr> {
    let (net_s, prefix) = match s.split_once('/') {
        Some((n, p)) => (n, p.parse().ok()?),
        None => (s, if s.contains(':') { 128 } else { 32 }),
    };
    let net: std::net::IpAddr = net_s.parse().ok()?;
    Some(Cidr { net, prefix })
}

/// Locate the `ip` binary (daemons may have a minimal PATH).
fn ip_bin() -> String {
    for p in ["/usr/sbin/ip", "/sbin/ip", "/usr/bin/ip", "/bin/ip"] {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    "ip".to_string()
}

/// Build the LAN range set: static private ranges + directly-connected subnets
/// from the routing table (routes without a `via` next hop are on-link = LAN).
fn load_lan_ranges() -> LanRanges {
    let mut subnets: Vec<Cidr> = [
        "127.0.0.0/8",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "169.254.0.0/16",
        "100.64.0.0/10",
        "::1/128",
        "fc00::/7",
        "fe80::/10",
    ]
    .iter()
    .filter_map(|s| parse_cidr(s))
    .collect();

    let ip = ip_bin();
    for args in [["route", "show"].as_slice(), ["-6", "route", "show"].as_slice()] {
        if let Ok(out) = std::process::Command::new(&ip).args(args).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let mut toks = line.split_whitespace();
                let Some(first) = toks.next() else { continue };
                // Skip the default route and any routed (off-link) prefix.
                if first == "default" || line.contains(" via ") {
                    continue;
                }
                if let Some(cidr) = parse_cidr(first) {
                    subnets.push(cidr);
                }
            }
        }
    }
    LanRanges { subnets }
}

struct Inner {
    procmap: RwLock<Arc<ProcMap>>,
    accum: Mutex<Accum>,
    snapshot: RwLock<MonitorSnapshot>,
    /// LAN address ranges (refreshed with the proc map).
    lan: RwLock<Arc<LanRanges>>,
}

/// Handle to the running monitor. Cloneable; threads keep it alive.
#[derive(Clone)]
pub struct Monitor {
    inner: Arc<Inner>,
}

impl Monitor {
    /// Open the capture socket and spawn the monitor threads.
    ///
    /// Fails if the capture socket can't be opened (needs `CAP_NET_RAW`).
    pub fn start() -> std::io::Result<Self> {
        let socket = capture::open_socket()?;
        let inner = Arc::new(Inner {
            procmap: RwLock::new(Arc::new(ProcMap::build())),
            accum: Mutex::new(Accum::default()),
            snapshot: RwLock::new(MonitorSnapshot::default()),
            lan: RwLock::new(Arc::new(load_lan_ranges())),
        });

        // Capture thread.
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-capture".into())
                .spawn(move || capture_loop(socket, inner))?;
        }
        // ProcMap refresher.
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-procmap".into())
                .spawn(move || loop {
                    std::thread::sleep(REFRESH_INTERVAL);
                    *inner.procmap.write().unwrap() = Arc::new(ProcMap::build());
                    *inner.lan.write().unwrap() = Arc::new(load_lan_ranges());
                })?;
        }
        // Sampler.
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-sampler".into())
                .spawn(move || loop {
                    std::thread::sleep(SAMPLE_INTERVAL);
                    inner.sample();
                })?;
        }

        info!("traffic monitor started");
        Ok(Self { inner })
    }

    /// Latest published snapshot.
    pub fn snapshot(&self) -> MonitorSnapshot {
        self.inner.snapshot.read().unwrap().clone()
    }
}

/// A flow's identity, used to key the resolved-flow cache.
type FlowKey = (procmap::Proto, std::net::IpAddr, u16, std::net::IpAddr, u16);

/// Capture loop: parse each frame, attribute it, accumulate bytes.
///
/// Attribution is `/proc`-based, which can miss a flow during the brief window
/// before its socket appears in `/proc/net/*`. To recover trailing packets of
/// short-lived connections, every successful resolution is remembered in a
/// per-flow cache and reused when `/proc` no longer lists the (now-closed)
/// socket. The cache is local to this thread, so no locking is needed.
fn capture_loop(socket: std::os::fd::OwnedFd, inner: Arc<Inner>) {
    let mut buf = vec![0u8; 65536];
    let mut flow_cache: HashMap<FlowKey, String> = HashMap::new();
    loop {
        let (n, outgoing) = match capture::recv(&socket, &mut buf) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "capture recv failed; stopping capture thread");
                return;
            }
        };
        let Some(s) = capture::parse_frame(&buf[..n], outgoing) else {
            continue;
        };

        let key: FlowKey = (s.proto, s.local_ip, s.local_port, s.rem_ip, s.rem_port);
        // Authoritative live lookup first; fall back to the cache for flows
        // whose socket has already closed.
        let pm = inner.procmap.read().unwrap().clone();
        let exe = match pm.lookup(s.proto, s.local_ip, s.local_port, s.rem_ip, s.rem_port) {
            Some(owner) => {
                if flow_cache.len() >= FLOW_CACHE_MAX {
                    flow_cache.clear();
                }
                flow_cache
                    .entry(key)
                    .or_insert_with(|| owner.exe.clone())
                    .clone()
            }
            None => flow_cache.get(&key).cloned().unwrap_or_default(),
        };

        let is_lan = inner.lan.read().unwrap().contains(s.rem_ip);
        let mut acc = inner.accum.lock().unwrap();
        {
            let app = acc.apps.entry(exe).or_default();
            if s.outgoing {
                app.up_total += s.bytes;
            } else {
                app.down_total += s.bytes;
            }
        }
        let zone = if is_lan {
            &mut acc.lan
        } else {
            &mut acc.internet
        };
        if s.outgoing {
            zone.up += s.bytes;
        } else {
            zone.down += s.bytes;
        }
        if s.outgoing {
            acc.host.up += s.bytes;
        } else {
            acc.host.down += s.bytes;
        }
    }
}

impl Inner {
    /// Turn accumulated totals into rates + sparklines and publish a snapshot.
    fn sample(&self) {
        let pm = self.procmap.read().unwrap().clone();
        let mut acc = self.accum.lock().unwrap();

        let mut apps = Vec::with_capacity(acc.apps.len());
        let mut evict = Vec::new();
        for (exe, a) in acc.apps.iter_mut() {
            let down_bps = a.down_total - a.last_down;
            let up_bps = a.up_total - a.last_up;
            a.last_down = a.down_total;
            a.last_up = a.up_total;
            push_spark(&mut a.down_spark, down_bps as u32);
            push_spark(&mut a.up_spark, up_bps as u32);

            let pids = pm.pids_for(exe);
            if down_bps == 0 && up_bps == 0 && pids == 0 {
                a.idle += 1;
                if a.idle > IDLE_EVICT {
                    evict.push(exe.clone());
                    continue;
                }
            } else {
                a.idle = 0;
            }

            let (name, exe_out) = if exe.is_empty() {
                ("(unresolved)".to_string(), String::new())
            } else {
                (basename(exe), exe.clone())
            };
            apps.push(AppStat {
                exe: exe_out,
                name,
                pids,
                down_bps,
                up_bps,
                down_total: a.down_total,
                up_total: a.up_total,
                down_spark: a.down_spark.iter().copied().collect(),
                up_spark: a.up_spark.iter().copied().collect(),
                status: AppStatus::Watching,
            });
        }
        for k in evict {
            acc.apps.remove(&k);
        }

        let (host_down_bps, host_up_bps) = acc.host.rates();
        let (lan_down_bps, lan_up_bps) = acc.lan.rates();
        let (inet_down_bps, inet_up_bps) = acc.internet.rates();
        let host = HostTotals {
            down_bps: host_down_bps,
            up_bps: host_up_bps,
            down_total: acc.host.down,
            up_total: acc.host.up,
            lan_down_bps,
            lan_up_bps,
            inet_down_bps,
            inet_up_bps,
        };
        drop(acc);

        // Busiest first.
        apps.sort_by_key(|s| std::cmp::Reverse(s.down_bps + s.up_bps));

        *self.snapshot.write().unwrap() = MonitorSnapshot {
            apps,
            host,
            timestamp_ms: now_ms(),
        };
    }
}

fn push_spark(spark: &mut VecDeque<u32>, v: u32) {
    if spark.len() == SPARK_LEN {
        spark.pop_front();
    }
    spark.push_back(v);
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
