//! Traffic shaping/policing engine.
//!
//! * **Host-wide caps (P2)** — HTB shaping via `tc` on the physical interface
//!   (egress) and an IFB device (ingress).
//! * **Per-application caps (P3)** — cgroup v2 per app + nftables `socket
//!   cgroupv2` policing in the input/output hooks. A background reconciler
//!   keeps each app's freshly-spawned processes placed in its cgroup so rules
//!   follow the application by executable path.
//!
//! The host (tc) and per-app (nft) layers are independent and compose: an
//! inbound packet is first IFB-shaped for the host cap, then policed by any
//! per-app rule. Requires `CAP_NET_ADMIN` (root).

mod cgroup;
mod ebpf;
mod nft;
mod tc;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use curb_proto::{AppLimit, HostLimit, LimiterState, Scope};
use ebpf::EbpfShaper;
use tracing::{info, warn};

/// Dedicated IFB device name for ingress shaping (≤15 chars).
const IFB_DEV: &str = "ifbcurb";
/// How often app cgroup membership is reconciled against running processes.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
/// First HTB class minor assigned to per-app egress classes (1:10, 1:11, …).
const FIRST_MINOR: u16 = 0x10;

/// HTB class handle (`skb->priority`) for the given minor under root `1:`.
fn classid(minor: u16) -> u32 {
    (1u32 << 16) | minor as u32
}

/// A configured per-application rule.
#[derive(Clone)]
struct AppRuleState {
    /// Display name (executable basename).
    name: String,
    /// Sanitized cgroup leaf directory name for this app.
    dir: String,
    down_bps: Option<u64>,
    up_bps: Option<u64>,
    scope: Scope,
    /// Set by the quota manager when the app's budget is exceeded (P5).
    blocked: bool,
    /// Stable HTB class minor for this app's eBPF egress shaping (E1).
    minor: u16,
}

impl AppRuleState {
    /// True when this rule has no effect and its entry/cgroup can be dropped.
    fn is_empty(&self) -> bool {
        self.down_bps.is_none() && self.up_bps.is_none() && !self.blocked
    }
}

struct EngineInner {
    iface: String,
    /// Host-wide caps + master switch.
    state: Mutex<LimiterState>,
    /// Per-application rules, keyed by executable path.
    apps: Mutex<HashMap<String, AppRuleState>>,
    /// eBPF egress classifier; `None` falls back to nftables policing for
    /// upload (E1). When present, per-app upload is HTB-shaped, not policed.
    shaper: Option<EbpfShaper>,
    /// Next HTB class minor to hand out.
    next_minor: Mutex<u16>,
}

impl EngineInner {
    /// Allocate a fresh, never-reused HTB class minor.
    fn alloc_minor(&self) -> u16 {
        let mut m = self.next_minor.lock().unwrap();
        let v = *m;
        *m = m.checked_add(1).unwrap_or(FIRST_MINOR);
        v
    }
}

/// Owns desired limiter state and reconciles it onto the kernel.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

impl Engine {
    /// Detect the egress interface, create an idle engine, and start the
    /// per-app membership reconciler. No kernel changes are made until a limit
    /// is applied.
    pub fn new() -> Result<Self> {
        let iface = tc::default_interface().context("detecting default interface")?;
        info!(interface = %iface, "shaping engine bound to interface");

        // Try to load the eBPF egress classifier for smooth per-app upload
        // shaping; fall back to nftables policing if unavailable.
        let shaper = match EbpfShaper::attach_egress(&iface) {
            Ok(s) => {
                info!("per-app upload: eBPF HTB shaping");
                Some(s)
            }
            Err(e) => {
                warn!(error = %e, "eBPF egress shaping unavailable; using nftables policing for upload");
                None
            }
        };

        let inner = Arc::new(EngineInner {
            state: Mutex::new(LimiterState {
                enabled: false,
                host: HostLimit::default(),
                interface: iface.clone(),
            }),
            apps: Mutex::new(HashMap::new()),
            shaper,
            next_minor: Mutex::new(FIRST_MINOR),
            iface,
        });

        // Reconciler: keep app cgroups populated with their processes.
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-reconcile".into())
                .spawn(move || loop {
                    std::thread::sleep(RECONCILE_INTERVAL);
                    reconcile_membership(&inner);
                })
                .ok();
        }

        Ok(Self { inner })
    }

    /// Current host limiter state.
    pub fn state(&self) -> LimiterState {
        self.inner.state.lock().unwrap().clone()
    }

    /// Whether shaping is currently enabled.
    pub fn enabled(&self) -> bool {
        self.inner.state.lock().unwrap().enabled
    }

    // ---- Host-wide caps (P2) ----------------------------------------------

    /// Set host-wide caps and enable limiting.
    pub fn set_host_limit(&self, down_bps: Option<u64>, up_bps: Option<u64>) -> Result<LimiterState> {
        {
            let mut st = self.inner.state.lock().unwrap();
            st.host = HostLimit { down_bps, up_bps };
            st.enabled = true;
        }
        self.inner.reconcile_egress()?;
        self.inner.reconcile_ingress()?;
        self.inner.reconcile_nft()?;
        Ok(self.state())
    }

    /// Toggle the master switch, keeping configured caps. Affects the egress
    /// (HTB/eBPF), ingress (IFB), and per-app (nft) layers.
    pub fn set_enabled(&self, enabled: bool) -> Result<LimiterState> {
        self.inner.state.lock().unwrap().enabled = enabled;
        self.inner.reconcile_egress()?;
        self.inner.reconcile_ingress()?;
        self.inner.reconcile_nft()?;
        Ok(self.state())
    }

    // ---- Per-application caps (P3) -----------------------------------------

    /// Create or update a per-application rule and start tracking the app.
    pub fn set_app_limit(
        &self,
        exe: String,
        down_bps: Option<u64>,
        up_bps: Option<u64>,
        scope: Scope,
    ) -> Result<Vec<AppLimit>> {
        let dir = sanitize_dir(&exe);
        cgroup::ensure_app(&dir).context("creating app cgroup")?;
        {
            let mut apps = self.inner.apps.lock().unwrap();
            // Preserve quota block + class minor if already tracked.
            let (blocked, minor) = apps
                .get(&exe)
                .map(|r| (r.blocked, r.minor))
                .unwrap_or((false, self.inner.alloc_minor()));
            apps.insert(
                exe.clone(),
                AppRuleState {
                    name: basename(&exe),
                    dir,
                    down_bps,
                    up_bps,
                    scope,
                    blocked,
                    minor,
                },
            );
        }
        // Configuring an app rule implies the limiter is on (mirrors host limits).
        self.inner.state.lock().unwrap().enabled = true;
        self.inner.reconcile_egress()?;
        self.inner.reconcile_nft()?;
        reconcile_membership(&self.inner); // place existing processes now
        Ok(self.list_app_limits())
    }

    /// Begin tracking an app (create its cgroup, place its processes) without
    /// applying any limit yet. Used by the quota manager so that an exceeded
    /// budget can immediately block the app's new connections.
    pub fn ensure_tracked(&self, exe: &str) -> Result<()> {
        let dir = sanitize_dir(exe);
        {
            let mut apps = self.inner.apps.lock().unwrap();
            if !apps.contains_key(exe) {
                cgroup::ensure_app(&dir).context("creating app cgroup")?;
                let minor = self.inner.alloc_minor();
                apps.insert(
                    exe.to_string(),
                    AppRuleState {
                        name: basename(exe),
                        dir,
                        down_bps: None,
                        up_bps: None,
                        scope: Scope::Both,
                        blocked: false,
                        minor,
                    },
                );
            }
        }
        reconcile_membership(&self.inner);
        Ok(())
    }

    /// Drop a tracked app's entry/cgroup if it carries no limit or block.
    pub fn untrack_if_empty(&self, exe: &str) {
        let removed = {
            let mut apps = self.inner.apps.lock().unwrap();
            match apps.get(exe) {
                Some(r) if r.is_empty() => apps.remove(exe).map(|r| r.dir),
                _ => None,
            }
        };
        if let Some(dir) = removed {
            cgroup::remove_app(&dir);
        }
    }

    /// Block or unblock all of an app's traffic (quota enforcement, P5).
    ///
    /// Tracks the app (creating its cgroup) if it isn't already; when unblocking
    /// an app that has no rate caps either, the entry and cgroup are released.
    pub fn set_app_blocked(&self, exe: &str, blocked: bool) -> Result<()> {
        let dir = sanitize_dir(exe);
        let mut removed_dir = None;
        {
            let mut apps = self.inner.apps.lock().unwrap();
            match apps.get_mut(exe) {
                Some(rule) => {
                    rule.blocked = blocked;
                    if rule.is_empty() {
                        removed_dir = Some(rule.dir.clone());
                        apps.remove(exe);
                    }
                }
                None if blocked => {
                    cgroup::ensure_app(&dir).context("creating app cgroup")?;
                    let minor = self.inner.alloc_minor();
                    apps.insert(
                        exe.to_string(),
                        AppRuleState {
                            name: basename(exe),
                            dir,
                            down_bps: None,
                            up_bps: None,
                            scope: Scope::Both,
                            blocked: true,
                            minor,
                        },
                    );
                }
                None => return Ok(()), // unblocking an untracked app: nothing to do
            }
        }
        if blocked {
            self.inner.state.lock().unwrap().enabled = true;
        }
        self.inner.reconcile_nft()?;
        if let Some(dir) = removed_dir {
            cgroup::remove_app(&dir);
        } else {
            reconcile_membership(&self.inner);
        }
        Ok(())
    }

    /// Remove a per-application rule and release its cgroup.
    pub fn clear_app_limit(&self, exe: &str) -> Result<Vec<AppLimit>> {
        let removed = self.inner.apps.lock().unwrap().remove(exe);
        self.inner.reconcile_egress()?;
        self.inner.reconcile_nft()?;
        if let Some(rule) = removed {
            cgroup::remove_app(&rule.dir);
        }
        Ok(self.list_app_limits())
    }

    /// List configured per-application rules with live process counts.
    pub fn list_app_limits(&self) -> Vec<AppLimit> {
        let apps = self.inner.apps.lock().unwrap();
        let mut out: Vec<AppLimit> = apps
            .iter()
            .map(|(exe, r)| AppLimit {
                exe: exe.clone(),
                name: r.name.clone(),
                down_bps: r.down_bps,
                up_bps: r.up_bps,
                scope: r.scope,
                pids: cgroup::member_count(&r.dir),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Remove all CURB kernel state. Call on daemon shutdown.
    pub fn shutdown(&self) {
        tc::clear_egress(&self.inner.iface);
        tc::clear_ingress(&self.inner.iface, IFB_DEV);
        tc::clear_clsact(&self.inner.iface);
        nft::clear();
        let mut apps = self.inner.apps.lock().unwrap();
        for rule in apps.values() {
            cgroup::remove_app(&rule.dir);
        }
        apps.clear();
        cgroup::remove_base();
        info!("engine shut down; all shaping/policing removed");
    }
}

impl EngineInner {
    /// Rebuild egress (upload) shaping: host default class + per-app classes.
    ///
    /// With the eBPF shaper present, per-app uploads are HTB-shaped (the
    /// classifier steers them by `skb->priority`); without it, only the host
    /// cap is shaped here and per-app upload is policed by nftables.
    fn reconcile_egress(&self) -> Result<()> {
        let st = self.state.lock().unwrap().clone();
        tc::clear_egress(&self.iface);

        let Some(shaper) = &self.shaper else {
            // Fallback path: just the host upload cap.
            if st.enabled {
                if let Some(up) = st.host.up_bps {
                    tc::apply_egress(&self.iface, up).context("applying host upload cap")?;
                }
            }
            return Ok(());
        };

        shaper.clear_all().ok();
        if !st.enabled {
            return Ok(());
        }
        // Root + default class (host cap or line rate); per-app classes steered
        // by the classifier.
        tc::egress_root(&self.iface, st.host.up_bps).context("building egress HTB root")?;
        let ceil = st.host.up_bps.unwrap_or(tc::LINE_RATE_BPS);
        let apps: Vec<(u16, u64, String)> = {
            let apps = self.apps.lock().unwrap();
            apps.values()
                .filter_map(|r| r.up_bps.map(|up| (r.minor, up, r.dir.clone())))
                .collect()
        };
        for (minor, up, dir) in apps {
            tc::egress_app_class(&self.iface, minor, up, ceil.max(up))
                .context("adding per-app egress class")?;
            if let Some(cgid) = cgroup::cgroup_id(&dir) {
                shaper.set_class(cgid, classid(minor)).ok();
            }
        }
        info!(host_up = ?st.host.up_bps, "egress HTB + eBPF classes applied");
        Ok(())
    }

    /// Rebuild ingress (download) host shaping via the IFB device.
    fn reconcile_ingress(&self) -> Result<()> {
        let st = self.state.lock().unwrap().clone();
        tc::clear_ingress(&self.iface, IFB_DEV);
        if st.enabled {
            if let Some(down) = st.host.down_bps {
                tc::apply_ingress(&self.iface, IFB_DEV, down).context("applying host download cap")?;
            }
        }
        Ok(())
    }

    /// Rebuild the nftables per-app ruleset from the current apps + master switch.
    ///
    /// When the eBPF shaper handles egress, the upload caps are omitted here
    /// (they're HTB-shaped, not policed); nftables then covers per-app download
    /// policing and quota blocks.
    fn reconcile_nft(&self) -> Result<()> {
        let enabled = self.state.lock().unwrap().enabled;
        let ebpf_egress = self.shaper.is_some();
        let rules: Vec<nft::AppRule> = {
            let apps = self.apps.lock().unwrap();
            apps.values()
                .map(|r| nft::AppRule {
                    cgroup_rel: cgroup::app_rel(&r.dir),
                    down_bps: r.down_bps,
                    up_bps: if ebpf_egress { None } else { r.up_bps },
                    scope: r.scope,
                    blocked: r.blocked,
                })
                .collect()
        };
        if !enabled || rules.is_empty() {
            nft::clear();
            return Ok(());
        }
        nft::apply(&rules).context("applying nft per-app rules")
    }
}

/// Place every running process of a tracked app into its cgroup.
///
/// Scans `/proc` once and matches each process's executable against the rule
/// set, so newly launched instances of a limited app are captured within one
/// reconcile tick. Children of an already-placed process inherit the cgroup.
fn reconcile_membership(inner: &EngineInner) {
    // exe -> cgroup dir for all tracked apps.
    let wanted: HashMap<String, String> = {
        let apps = inner.apps.lock().unwrap();
        if apps.is_empty() {
            return;
        }
        apps.iter().map(|(exe, r)| (exe.clone(), r.dir.clone())).collect()
    };

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for ent in entries.flatten() {
        let Some(pid) = ent
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let exe = read_exe(pid);
        if exe.is_empty() {
            continue;
        }
        if let Some(dir) = wanted.get(&exe) {
            cgroup::add_pid(dir, pid);
        }
    }
}

/// Read `/proc/<pid>/exe`, stripping the kernel's " (deleted)" suffix.
fn read_exe(pid: u32) -> String {
    let raw = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    raw.strip_suffix(" (deleted)")
        .map(str::to_string)
        .unwrap_or(raw)
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Derive a stable, filesystem-safe cgroup directory name from an exe path:
/// `<sanitized-basename>-<hash8>` so distinct paths never collide.
fn sanitize_dir(exe: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut h);
    let hash = h.finish();

    let base: String = basename(exe)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(24)
        .collect();
    format!("{base}-{:08x}", (hash & 0xffff_ffff) as u32)
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Only the last handle (besides the reconciler thread) cleans up; the
        // daemon also calls shutdown() explicitly on graceful exit.
        if Arc::strong_count(&self.inner) <= 2 && self.enabled() {
            warn!("engine dropping while enabled; removing shaping");
            tc::clear_egress(&self.inner.iface);
            tc::clear_ingress(&self.inner.iface, IFB_DEV);
            nft::clear();
        }
    }
}
