//! Persistent limiter rules (master switch, host caps, per-app caps).
//! Stored at `CURB_STATE_DIR/rules.json` so configuration survives restarts.

use std::path::PathBuf;

use curb_proto::{HostLimit, Scope};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

fn rules_path() -> PathBuf {
    let dir = std::env::var_os("CURB_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/curb"));
    dir.join("rules.json")
}

/// One persisted per-application rule.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedApp {
    pub exe: String,
    pub down_bps: Option<u64>,
    pub up_bps: Option<u64>,
    pub scope: Scope,
}

#[derive(Default, Serialize, Deserialize)]
pub struct SavedRules {
    pub enabled: bool,
    pub host: HostLimit,
    pub apps: Vec<SavedApp>,
}

pub fn load() -> SavedRules {
    let path = rules_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            warn!(error = %e, "rules file unreadable; starting with no rules");
            SavedRules::default()
        }),
        Err(_) => SavedRules::default(),
    }
}

/// Persist atomically (write temp + rename).
pub fn save(rules: &SavedRules) {
    let path = rules_path();
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            debug!(error = %e, "creating state dir");
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec_pretty(rules) {
        Ok(bytes) => {
            if let Ok(f) = std::fs::File::create(&tmp) {
                use std::io::Write;
                if std::io::BufWriter::new(&f).write_all(&bytes).is_ok() {
                    let _ = f.sync_all();
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
        Err(e) => debug!(error = %e, "serializing rules"),
    }
}
