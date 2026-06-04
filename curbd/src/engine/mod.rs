//! Host-wide traffic shaping engine (P2).
//!
//! Implements rate limiting by shelling out to `tc`/`ip` (the pragmatic,
//! debuggable path from the plan; a netlink rewrite can come later).
//!
//! * **Egress (upload)** is shaped directly: an HTB qdisc on the physical
//!   interface with a single rate-capped class.
//! * **Ingress (download)** can't be shaped directly on Linux, so we attach an
//!   `ingress` qdisc to the interface, redirect all incoming packets to a
//!   dedicated **IFB** device with `mirred`, and shape *that* device's egress
//!   with HTB. This is precise-ish but best-effort (packets are already at the
//!   NIC); documented as a known limitation.
//!
//! Each change tears the whole setup down and rebuilds it from the desired
//! state — simple and idempotent. Requires `CAP_NET_ADMIN` (root).

mod tc;

use std::sync::Mutex;

use anyhow::{Context, Result};
use curb_proto::{HostLimit, LimiterState};
use tracing::{info, warn};

/// Dedicated IFB device name for ingress shaping (≤15 chars).
const IFB_DEV: &str = "ifbcurb";

/// Owns the desired limiter state and reconciles it onto the kernel.
pub struct Engine {
    iface: String,
    state: Mutex<LimiterState>,
}

impl Engine {
    /// Detect the egress interface and create an idle (disabled) engine.
    ///
    /// No kernel changes are made until a limit is applied, so this succeeds
    /// even unprivileged; the privileged operations fail later if `curbd`
    /// lacks `CAP_NET_ADMIN`.
    pub fn new() -> Result<Self> {
        let iface = tc::default_interface().context("detecting default interface")?;
        info!(interface = %iface, "shaping engine bound to interface");
        Ok(Self {
            state: Mutex::new(LimiterState {
                enabled: false,
                host: HostLimit::default(),
                interface: iface.clone(),
            }),
            iface,
        })
    }

    /// Current limiter state.
    pub fn state(&self) -> LimiterState {
        self.state.lock().unwrap().clone()
    }

    /// Whether shaping is currently enabled.
    pub fn enabled(&self) -> bool {
        self.state.lock().unwrap().enabled
    }

    /// Set host-wide caps and enable limiting.
    pub fn set_host_limit(&self, down_bps: Option<u64>, up_bps: Option<u64>) -> Result<LimiterState> {
        let mut st = self.state.lock().unwrap();
        st.host = HostLimit { down_bps, up_bps };
        st.enabled = true;
        self.reconcile(&st)?;
        Ok(st.clone())
    }

    /// Toggle the master switch, keeping configured caps.
    pub fn set_enabled(&self, enabled: bool) -> Result<LimiterState> {
        let mut st = self.state.lock().unwrap();
        st.enabled = enabled;
        self.reconcile(&st)?;
        Ok(st.clone())
    }

    /// Tear down any existing CURB qdiscs and rebuild from `st`.
    fn reconcile(&self, st: &LimiterState) -> Result<()> {
        self.teardown();
        if !st.enabled {
            info!("limiter disabled; shaping removed");
            return Ok(());
        }
        if let Some(up) = st.host.up_bps {
            tc::apply_egress(&self.iface, up).context("applying upload cap")?;
        }
        if let Some(down) = st.host.down_bps {
            tc::apply_ingress(&self.iface, IFB_DEV, down).context("applying download cap")?;
        }
        info!(
            interface = %self.iface,
            down = ?st.host.down_bps, up = ?st.host.up_bps,
            "host limits applied"
        );
        Ok(())
    }

    /// Remove all CURB shaping state. Best-effort; missing qdiscs are ignored.
    fn teardown(&self) {
        tc::clear_egress(&self.iface);
        tc::clear_ingress(&self.iface, IFB_DEV);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Don't leave the user's interface shaped if the daemon exits.
        if self.state.lock().map(|s| s.enabled).unwrap_or(false) {
            warn!("engine dropping while enabled; removing shaping");
            self.teardown();
        }
    }
}
