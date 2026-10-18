//! Per-application data quotas (P5).
//!
//! Accounts each quota'd app's usage from the live [`Monitor`] totals,
//! persists it across daemon restarts, and enforces the budget by asking the
//! [`Engine`] to block the app (nft drop on its cgroup) once exceeded. Usage
//! resets at the period boundary, which unblocks the app.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use curb_proto::{Direction, QuotaPeriod, QuotaStatus};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::engine::Engine;
use crate::monitor::Monitor;

/// How often usage is sampled and enforcement re-evaluated.
const TICK: Duration = Duration::from_secs(2);

/// Resolve the state directory (override with `$CURB_STATE_DIR`).
fn state_path() -> PathBuf {
    let dir = std::env::var_os("CURB_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/curb"));
    dir.join("quota.json")
}

/// One quota's configuration + accounting cursor (persisted).
#[derive(Clone, Serialize, Deserialize)]
struct QuotaEntry {
    name: String,
    budget_bytes: u64,
    period: QuotaPeriod,
    direction: Direction,
    used_bytes: u64,
    /// Epoch ms when the current period started.
    period_start_ms: u64,
    exceeded: bool,
    /// Last observed monitor totals, to compute per-tick deltas.
    last_down_total: u64,
    last_up_total: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct QuotaData {
    quotas: HashMap<String, QuotaEntry>,
}

struct Inner {
    monitor: Monitor,
    engine: Engine,
    path: PathBuf,
    data: Mutex<QuotaData>,
}

/// Handle to the running quota manager.
#[derive(Clone)]
pub struct QuotaManager {
    inner: Arc<Inner>,
}

impl QuotaManager {
    /// Load persisted quotas, re-apply any active blocks, and start the
    /// accounting loop.
    pub fn start(monitor: Monitor, engine: Engine) -> Self {
        let path = state_path();
        let mut data = load(&path);

        // On restart the monitor's totals begin at zero, so reset cursors and
        // re-block anything still over budget.
        let now = now_ms();
        for (exe, q) in data.quotas.iter_mut() {
            q.last_down_total = 0;
            q.last_up_total = 0;
            if q.exceeded {
                if let Err(e) = engine.set_app_blocked(exe, true) {
                    warn!(exe, error = %e, "re-applying quota block failed");
                }
            }
        }
        let _ = now;

        let inner = Arc::new(Inner {
            monitor,
            engine,
            path,
            data: Mutex::new(data),
        });

        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-quota".into())
                .spawn(move || loop {
                    std::thread::sleep(TICK);
                    inner.tick();
                })
                .ok();
        }
        info!("quota manager started");
        Self { inner }
    }

    /// Create or replace a quota and begin counting from now.
    pub fn set_quota(
        &self,
        exe: String,
        budget_bytes: u64,
        period: QuotaPeriod,
        direction: Direction,
    ) -> anyhow::Result<Vec<QuotaStatus>> {
        // Track the app now so the cgroup exists and its processes are placed,
        // making enforcement effective the moment the budget is exceeded. A
        // quota that can't be enforced must not be reported as set.
        self.inner
            .engine
            .ensure_tracked(&exe)
            .map_err(|e| e.context("quota cannot be enforced"))?;
        let (down, up) = self.inner.totals_for(&exe);
        let mut unblock = false;
        {
            let mut data = self.inner.data.lock().unwrap();
            let entry = match data.quotas.get(&exe) {
                // Editing an existing quota with the same period and direction
                // keeps its usage so far; otherwise counting starts fresh.
                Some(old) if old.period == period && old.direction == direction => QuotaEntry {
                    budget_bytes,
                    exceeded: old.exceeded && old.used_bytes >= budget_bytes,
                    ..old.clone()
                },
                old => QuotaEntry {
                    name: basename(&exe),
                    budget_bytes,
                    period,
                    direction,
                    used_bytes: 0,
                    period_start_ms: now_ms(),
                    exceeded: false,
                    last_down_total: old.map_or(down, |o| o.last_down_total),
                    last_up_total: old.map_or(up, |o| o.last_up_total),
                },
            };
            if let Some(old) = data.quotas.get(&exe) {
                unblock = old.exceeded && !entry.exceeded;
            }
            data.quotas.insert(exe.clone(), entry);
            save(&self.inner.path, &data);
        }
        if unblock {
            if let Err(e) = self.inner.engine.set_app_blocked(&exe, false) {
                warn!(exe, error = %e, "unblocking app after quota edit failed");
            }
        }
        self.inner.tick(); // evaluate immediately
        Ok(self.list())
    }

    /// Remove a quota and unblock the app if it was blocked.
    pub fn clear_quota(&self, exe: &str) -> Vec<QuotaStatus> {
        let was = {
            let mut data = self.inner.data.lock().unwrap();
            let removed = data.quotas.remove(exe);
            save(&self.inner.path, &data);
            removed
        };
        if was.map(|q| q.exceeded).unwrap_or(false) {
            let _ = self.inner.engine.set_app_blocked(exe, false);
        }
        // Release the cgroup if no manual limit keeps the app tracked.
        self.inner.engine.untrack_if_empty(exe);
        self.list()
    }

    /// Current quotas with live usage.
    pub fn list(&self) -> Vec<QuotaStatus> {
        let now = now_ms();
        let data = self.inner.data.lock().unwrap();
        let mut out: Vec<QuotaStatus> = data
            .quotas
            .iter()
            .map(|(exe, q)| QuotaStatus {
                exe: exe.clone(),
                name: q.name.clone(),
                budget_bytes: q.budget_bytes,
                used_bytes: q.used_bytes,
                period: q.period,
                direction: q.direction,
                exceeded: q.exceeded,
                resets_in_secs: period_len(q.period).map(|len| {
                    let end = q.period_start_ms + len.as_millis() as u64;
                    end.saturating_sub(now) / 1000
                }),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

impl Inner {
    /// Look up an app's current monitor totals (down, up).
    fn totals_for(&self, exe: &str) -> (u64, u64) {
        self.monitor
            .snapshot()
            .apps
            .iter()
            .find(|a| a.exe == exe)
            .map(|a| (a.down_total, a.up_total))
            .unwrap_or((0, 0))
    }

    /// Sample usage, handle resets, and (un)block apps as needed.
    fn tick(&self) {
        let snap = self.monitor.snapshot();
        let totals: HashMap<&str, (u64, u64)> = snap
            .apps
            .iter()
            .filter(|a| !a.exe.is_empty())
            .map(|a| (a.exe.as_str(), (a.down_total, a.up_total)))
            .collect();
        let now = now_ms();

        // (exe, should_block) actions applied after releasing the lock so we
        // don't run nft under it.
        let mut actions: Vec<(String, bool)> = Vec::new();
        {
            let mut data = self.data.lock().unwrap();
            let mut dirty = false;
            for (exe, q) in data.quotas.iter_mut() {
                // Period reset.
                if let Some(len) = period_len(q.period) {
                    if now >= q.period_start_ms + len.as_millis() as u64 {
                        q.period_start_ms = now;
                        q.used_bytes = 0;
                        dirty = true;
                        if q.exceeded {
                            q.exceeded = false;
                            actions.push((exe.clone(), false));
                        }
                        if let Some(&(d, u)) = totals.get(exe.as_str()) {
                            q.last_down_total = d;
                            q.last_up_total = u;
                        }
                    }
                }
                // Accumulate this tick's delta (guard against counter resets).
                if let Some(&(d, u)) = totals.get(exe.as_str()) {
                    let dd = d.saturating_sub(q.last_down_total);
                    let du = u.saturating_sub(q.last_up_total);
                    let delta = match q.direction {
                        Direction::Down => dd,
                        Direction::Up => du,
                        Direction::Both => dd + du,
                    };
                    dirty |= delta > 0 || q.last_down_total != d || q.last_up_total != u;
                    q.last_down_total = d;
                    q.last_up_total = u;
                    q.used_bytes += delta;
                }
                if q.used_bytes >= q.budget_bytes && !q.exceeded {
                    q.exceeded = true;
                    actions.push((exe.clone(), true));
                }
            }
            if dirty || !actions.is_empty() {
                save(&self.path, &data);
            }
        }

        for (exe, block) in actions {
            if block {
                info!(exe, "quota exceeded; blocking app");
            } else {
                info!(exe, "quota period reset; unblocking app");
            }
            if let Err(e) = self.engine.set_app_blocked(&exe, block) {
                warn!(exe, error = %e, "quota enforcement failed");
            }
        }
    }
}

fn period_len(p: QuotaPeriod) -> Option<Duration> {
    match p {
        QuotaPeriod::Hourly => Some(Duration::from_secs(3_600)),
        QuotaPeriod::Daily => Some(Duration::from_secs(86_400)),
        QuotaPeriod::Weekly => Some(Duration::from_secs(604_800)),
        QuotaPeriod::Monthly => Some(Duration::from_secs(2_592_000)), // 30 days
        QuotaPeriod::Total => None,
    }
}

fn load(path: &PathBuf) -> QuotaData {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            warn!(error = %e, "quota state corrupt; starting empty");
            QuotaData::default()
        }),
        Err(_) => QuotaData::default(),
    }
}

/// Persist atomically (write temp + rename).
fn save(path: &PathBuf, data: &QuotaData) {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            debug!(error = %e, "creating state dir");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec_pretty(data) {
        Ok(bytes) => {
            if let Ok(f) = std::fs::File::create(&tmp) {
                use std::io::Write;
                if std::io::BufWriter::new(&f).write_all(&bytes).is_ok() {
                    let _ = f.sync_all();
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
        Err(e) => debug!(error = %e, "serializing quota state"),
    }
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
