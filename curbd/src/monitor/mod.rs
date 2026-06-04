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
#[derive(Default)]
struct Accum {
    /// Keyed by executable path; empty string = unresolved traffic.
    apps: HashMap<String, AppAccum>,
    host_down: u64,
    host_up: u64,
    last_host_down: u64,
    last_host_up: u64,
}

struct Inner {
    procmap: RwLock<Arc<ProcMap>>,
    accum: Mutex<Accum>,
    snapshot: RwLock<MonitorSnapshot>,
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
                    let fresh = Arc::new(ProcMap::build());
                    *inner.procmap.write().unwrap() = fresh;
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

        let mut acc = inner.accum.lock().unwrap();
        {
            let app = acc.apps.entry(exe).or_default();
            if s.outgoing {
                app.up_total += s.bytes;
            } else {
                app.down_total += s.bytes;
            }
        }
        if s.outgoing {
            acc.host_up += s.bytes;
        } else {
            acc.host_down += s.bytes;
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

        let host = HostTotals {
            down_bps: acc.host_down - acc.last_host_down,
            up_bps: acc.host_up - acc.last_host_up,
            down_total: acc.host_down,
            up_total: acc.host_up,
        };
        acc.last_host_down = acc.host_down;
        acc.last_host_up = acc.host_up;
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
