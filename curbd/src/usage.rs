//! Persistent per-application data usage accounting (daily / weekly / monthly).
//!
//! Unlike the monitor's session totals (which reset on daemon restart), the
//! usage tracker accumulates bytes across restarts by persisting to
//! `CURB_STATE_DIR/usage.json`.  It polls the monitor every 10 seconds, takes
//! byte-total deltas per app, and adds them to the appropriate calendar buckets.
//! Buckets reset automatically when the UTC calendar period rolls over.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::monitor::Monitor;

/// How often the usage tracker samples the monitor.
const TICK: Duration = Duration::from_secs(10);
/// Persist to disk every this many ticks (10s × 3 = 30s).
const SAVE_EVERY: u32 = 3;

fn state_path() -> PathBuf {
    let dir = std::env::var_os("CURB_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/curb"));
    dir.join("usage.json")
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Bucket {
    down: u64,
    up: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct AppEntry {
    today: Bucket,
    /// ISO date of the day this bucket covers, e.g. "2026-06-11".
    today_date: String,
    week: Bucket,
    /// ISO date of the Monday that started this week, e.g. "2026-06-09".
    week_start: String,
    month: Bucket,
    /// ISO date of the 1st of this month, e.g. "2026-06-01".
    month_start: String,
    /// Monitor's down_total seen in the last tick (for delta computation).
    last_down: u64,
    /// Monitor's up_total seen in the last tick.
    last_up: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct UsageData {
    entries: HashMap<String, AppEntry>,
}

/// Usage snapshot for a single app — returned to callers.
pub struct AppUsage {
    pub today_down: u64,
    pub today_up: u64,
    pub week_down: u64,
    pub week_up: u64,
    pub month_down: u64,
    pub month_up: u64,
}

struct Inner {
    monitor: Monitor,
    path: PathBuf,
    data: Mutex<UsageData>,
}

/// Handle to the running usage tracker (cloneable).
#[derive(Clone)]
pub struct UsageTracker {
    inner: Arc<Inner>,
}

impl UsageTracker {
    /// Load persisted data and start the background accounting thread.
    pub fn start(monitor: Monitor) -> Self {
        let path = state_path();
        let data = load(&path);
        let inner = Arc::new(Inner {
            monitor,
            path,
            data: Mutex::new(data),
        });
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("curb-usage".into())
                .spawn(move || {
                    let mut tick_count: u32 = 0;
                    loop {
                        std::thread::sleep(TICK);
                        inner.tick();
                        tick_count += 1;
                        if tick_count % SAVE_EVERY == 0 {
                            let data = inner.data.lock().unwrap();
                            save(&inner.path, &data);
                        }
                    }
                })
                .ok();
        }
        info!("usage tracker started");
        Self { inner }
    }

    /// Return the latest usage for every tracked app, keyed by exe path.
    pub fn get_all(&self) -> HashMap<String, AppUsage> {
        let data = self.inner.data.lock().unwrap();
        data.entries
            .iter()
            .map(|(exe, e)| {
                (
                    exe.clone(),
                    AppUsage {
                        today_down: e.today.down,
                        today_up: e.today.up,
                        week_down: e.week.down,
                        week_up: e.week.up,
                        month_down: e.month.down,
                        month_up: e.month.up,
                    },
                )
            })
            .collect()
    }

    /// Flush to disk before daemon shutdown.
    pub fn flush(&self) {
        let data = self.inner.data.lock().unwrap();
        save(&self.inner.path, &data);
    }
}

impl Inner {
    fn tick(&self) {
        let snap = self.monitor.snapshot();
        let today = today_str();
        let week_start = week_start_str();
        let month_start = month_start_str();

        let mut data = self.data.lock().unwrap();
        for app in snap.apps.iter().filter(|a| !a.exe.is_empty()) {
            let entry = data.entries.entry(app.exe.clone()).or_insert_with(|| AppEntry {
                today: Bucket::default(),
                today_date: today.clone(),
                week: Bucket::default(),
                week_start: week_start.clone(),
                month: Bucket::default(),
                month_start: month_start.clone(),
                last_down: app.down_total,
                last_up: app.up_total,
            });

            // Roll over any buckets whose calendar period has ended.
            if entry.today_date != today {
                entry.today = Bucket::default();
                entry.today_date = today.clone();
            }
            if entry.week_start != week_start {
                entry.week = Bucket::default();
                entry.week_start = week_start.clone();
            }
            if entry.month_start != month_start {
                entry.month = Bucket::default();
                entry.month_start = month_start.clone();
            }

            // Compute delta since last sample (saturating_sub handles restarts
            // where monitor totals begin at zero again).
            let dd = app.down_total.saturating_sub(entry.last_down);
            let du = app.up_total.saturating_sub(entry.last_up);
            entry.last_down = app.down_total;
            entry.last_up = app.up_total;

            entry.today.down += dd;
            entry.today.up += du;
            entry.week.down += dd;
            entry.week.up += du;
            entry.month.down += dd;
            entry.month.up += du;
        }
    }
}

// ── Date helpers (UTC) ─────────────────────────────────────────────────────

fn today_str() -> String {
    let d = Utc::now().date_naive();
    d.format("%Y-%m-%d").to_string()
}

fn week_start_str() -> String {
    let d = Utc::now().date_naive();
    // ISO week starts on Monday.
    let monday = d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64);
    monday.format("%Y-%m-%d").to_string()
}

fn month_start_str() -> String {
    let d = Utc::now().date_naive();
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string()
}

// ── Persistence ────────────────────────────────────────────────────────────

fn load(path: &PathBuf) -> UsageData {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            debug!(error = %e, "usage state unreadable; starting fresh");
            UsageData::default()
        }),
        Err(_) => UsageData::default(),
    }
}

fn save(path: &PathBuf, data: &UsageData) {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            debug!(error = %e, "creating state dir for usage");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec_pretty(data) {
        Ok(bytes) => {
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
        Err(e) => debug!(error = %e, "serializing usage state"),
    }
}
