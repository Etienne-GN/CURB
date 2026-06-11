//! Persistent daemon configuration (interface preference, etc.).
//! Stored at `CURB_STATE_DIR/config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;

fn config_path() -> PathBuf {
    let dir = std::env::var_os("CURB_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/curb"));
    dir.join("config.json")
}

#[derive(Default, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Preferred shaping interface. `None` means auto-detect via default route.
    pub interface: Option<String>,
}

pub fn load() -> DaemonConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            debug!(error = %e, "daemon config unreadable; using defaults");
            DaemonConfig::default()
        }),
        Err(_) => DaemonConfig::default(),
    }
}

pub fn save_interface(name: &str) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let cfg = DaemonConfig {
        interface: Some(name.to_string()),
    };
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(&cfg) {
        if let Ok(f) = std::fs::File::create(&tmp) {
            use std::io::Write;
            if std::io::BufWriter::new(&f).write_all(&bytes).is_ok() {
                let _ = f.sync_all();
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}
