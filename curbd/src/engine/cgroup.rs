//! cgroup v2 management for per-application classification (P3).
//!
//! Each tracked application gets a leaf cgroup under `/sys/fs/cgroup/curb/`.
//! Placing a process there lets nftables match its traffic via `socket
//! cgroupv2`. We enable no controllers — membership alone drives classification,
//! and children of a placed process inherit the cgroup automatically.

use std::fs;
use std::io::Write;

use anyhow::{Context, Result};
use tracing::debug;

const CG_ROOT: &str = "/sys/fs/cgroup";
/// Parent cgroup holding one leaf per tracked app.
const CG_BASE: &str = "curb";
/// Depth of an app cgroup below the cgroup root (`curb/<dir>`), for nft's
/// `socket cgroupv2 level N` match.
pub const APP_LEVEL: u32 = 2;

/// Absolute filesystem path of an app's cgroup.
fn app_path(dir: &str) -> String {
    format!("{CG_ROOT}/{CG_BASE}/{dir}")
}

/// Path of an app cgroup relative to the cgroup root, as nft expects it.
pub fn app_rel(dir: &str) -> String {
    format!("{CG_BASE}/{dir}")
}

/// Remove the (expected-empty) base cgroup; ignored if absent or non-empty.
pub fn remove_base() {
    let _ = fs::remove_dir(format!("{CG_ROOT}/{CG_BASE}"));
}

/// Create the app's cgroup (and the base) if absent.
pub fn ensure_app(dir: &str) -> Result<()> {
    fs::create_dir_all(app_path(dir))
        .with_context(|| format!("creating cgroup {}", app_path(dir)))?;
    Ok(())
}

/// Place a process into the app's cgroup. Dead pids (ESRCH) are ignored;
/// re-adding an existing member is a no-op.
pub fn add_pid(dir: &str, pid: u32) {
    let procs = format!("{}/cgroup.procs", app_path(dir));
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&procs) {
        if let Err(e) = write!(f, "{pid}") {
            debug!(pid, error = %e, "could not move pid into cgroup (likely exited)");
        }
    }
}

/// Number of processes currently in the app's cgroup.
pub fn member_count(dir: &str) -> u32 {
    let procs = format!("{}/cgroup.procs", app_path(dir));
    fs::read_to_string(procs)
        .map(|s| s.split_whitespace().count() as u32)
        .unwrap_or(0)
}

/// Remove an app's cgroup, first evicting any remaining members back to the
/// root cgroup (a cgroup can only be removed once empty).
pub fn remove_app(dir: &str) {
    let procs = format!("{}/cgroup.procs", app_path(dir));
    if let Ok(content) = fs::read_to_string(&procs) {
        let root_procs = format!("{CG_ROOT}/cgroup.procs");
        for pid in content.split_whitespace() {
            if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&root_procs) {
                let _ = write!(f, "{pid}");
            }
        }
    }
    if let Err(e) = fs::remove_dir(app_path(dir)) {
        debug!(dir, error = %e, "could not remove app cgroup");
    }
}
