//! CURB IPC contract.
//!
//! This crate is the **frozen wire protocol** shared by every component:
//! the `curbd` daemon (server), the `curb` CLI (client), and the Tauri GUI
//! backend (client). It defines the request/response message types and a
//! small length-delimited transport over a Unix domain socket.
//!
//! Wire format: each frame is a big-endian `u32` length prefix followed by a
//! JSON-serialized [`Request`] or [`Response`]. JSON is used (rather than a
//! binary codec) so the protocol stays inspectable during early development;
//! it can be swapped for `bincode` later without changing the message types.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bumped whenever [`Request`]/[`Response`] change incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default socket location used by the packaged daemon.
pub const DEFAULT_SOCKET_PATH: &str = "/run/curbd.sock";

/// Resolve the control socket path.
///
/// Order of precedence: explicit `$CURB_SOCK` env var, then
/// [`DEFAULT_SOCKET_PATH`]. The env override lets the daemon and clients run
/// unprivileged during development (e.g. `CURB_SOCK=/tmp/curbd.sock`).
pub fn socket_path() -> PathBuf {
    std::env::var_os("CURB_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// A command sent from a client to the daemon.
///
/// New variants are appended as features land (host limits in P2, app rules in
/// P3, quotas in P5, live subscriptions in P1). Keeping this enum the single
/// source of truth is what lets the CLI and the GUI share one client library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Liveness check; expects [`Response::Pong`].
    Ping,
    /// Request a [`DaemonStatus`] snapshot.
    GetStatus,
    /// Request the current live per-application traffic snapshot (P1).
    ListApps,
    /// Set the host-wide rate caps and enable limiting (P2/NL-2). `None` for a
    /// direction means "unlimited". `down_bps`/`up_bps` are the total ("all
    /// traffic") caps; the `lan_*`/`inet_*` fields cap LAN and Internet traffic
    /// separately. This replaces the entire host limit (callers merge first).
    SetHostLimit {
        down_bps: Option<u64>,
        up_bps: Option<u64>,
        #[serde(default)]
        lan_down_bps: Option<u64>,
        #[serde(default)]
        lan_up_bps: Option<u64>,
        #[serde(default)]
        inet_down_bps: Option<u64>,
        #[serde(default)]
        inet_up_bps: Option<u64>,
    },
    /// Flip the global master switch without changing configured caps (P2).
    SetLimiterEnabled(bool),
    /// Read the current limiter state (P2).
    GetLimiter,
    /// Set per-application caps by executable path (P3). `None` in a direction
    /// means unlimited. Creating a rule starts tracking the app's processes.
    /// `scope` restricts the rule to LAN, Internet, or both (P4).
    SetAppLimit {
        exe: String,
        down_bps: Option<u64>,
        up_bps: Option<u64>,
        #[serde(default)]
        scope: Scope,
    },
    /// Remove a per-application rule and stop tracking the app (P3).
    ClearAppLimit { exe: String },
    /// List configured per-application rules (P3).
    ListAppLimits,
    /// Set a data quota for an application (P5). When usage exceeds the budget
    /// within the period, the app is blocked until the period resets.
    SetQuota {
        exe: String,
        budget_bytes: u64,
        period: QuotaPeriod,
        #[serde(default)]
        direction: Direction,
    },
    /// Remove an application's quota (P5).
    ClearQuota { exe: String },
    /// List configured quotas with live usage (P5).
    ListQuotas,
    /// List all active TCP/UDP connections attributed to their owning app.
    ListConnections,
}

/// The daemon's reply to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::GetStatus`].
    Status(DaemonStatus),
    /// Reply to [`Request::ListApps`]: live per-application traffic.
    Apps(MonitorSnapshot),
    /// Reply to limiter requests ([`Request::GetLimiter`] etc.).
    Limiter(LimiterState),
    /// Reply to per-application limit requests (P3).
    AppLimits(Vec<AppLimit>),
    /// Reply to quota requests (P5).
    Quotas(Vec<QuotaStatus>),
    /// Reply to [`Request::ListConnections`].
    Connections(Vec<ConnectionInfo>),
    /// Generic success for requests with no payload.
    Ok,
    /// The request could not be served.
    Error { message: String },
}

/// A directional rate cap pair. `None` means unlimited in that direction.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ScopedCap {
    /// Inbound (download) cap, bytes/sec.
    pub down_bps: Option<u64>,
    /// Outbound (upload) cap, bytes/sec.
    pub up_bps: Option<u64>,
}

impl ScopedCap {
    /// True if neither direction is capped.
    pub fn is_empty(&self) -> bool {
        self.down_bps.is_none() && self.up_bps.is_none()
    }
}

/// Host-wide rate caps (P2/NL-2). The top-level `down_bps`/`up_bps` apply to all
/// host traffic; `lan` and `internet` additionally cap those zones separately
/// (classified by remote address). `None` anywhere means unlimited.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct HostLimit {
    /// Total inbound (download) cap across all traffic; `None` = unlimited.
    pub down_bps: Option<u64>,
    /// Total outbound (upload) cap across all traffic; `None` = unlimited.
    pub up_bps: Option<u64>,
    /// Caps applied only to LAN (private-address) traffic.
    #[serde(default)]
    pub lan: ScopedCap,
    /// Caps applied only to Internet (public-address) traffic.
    #[serde(default)]
    pub internet: ScopedCap,
}

/// Traffic direction a quota counts (P5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Direction {
    /// Count download (inbound) bytes only.
    Down,
    /// Count upload (outbound) bytes only.
    Up,
    /// Count both directions.
    #[default]
    Both,
}

/// How often a quota's usage counter resets (P5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaPeriod {
    Hourly,
    Daily,
    Weekly,
    Monthly,
    /// Never resets — a lifetime cap.
    Total,
}

/// A configured quota plus its live usage (P5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaStatus {
    /// Absolute executable path the quota targets.
    pub exe: String,
    /// Display name (executable basename).
    pub name: String,
    /// Byte budget per period.
    pub budget_bytes: u64,
    /// Bytes used in the current period.
    pub used_bytes: u64,
    /// Reset period.
    pub period: QuotaPeriod,
    /// Direction counted.
    pub direction: Direction,
    /// Whether the budget is currently exceeded (app blocked).
    pub exceeded: bool,
    /// Seconds until the period resets (`None` for [`QuotaPeriod::Total`]).
    pub resets_in_secs: Option<u64>,
}

/// An individual TCP/UDP connection attributed to an application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    /// Absolute executable path of the owning process (`""` if unknown).
    pub exe: String,
    /// Display name (basename, or `"?"` if unknown).
    pub name: String,
    /// Owning PID (0 if unknown).
    pub pid: u32,
    /// `"TCP"` or `"UDP"`.
    pub proto: String,
    /// `"addr:port"`.
    pub local_addr: String,
    /// `"addr:port"`.
    pub remote_addr: String,
    /// TCP state string (`"ESTABLISHED"`, `"TIME_WAIT"`, …); `"UDP"` for UDP.
    pub state: String,
}

/// Which traffic a limit applies to, by remote address class (P4).
///
/// LAN = private/link-local ranges (RFC1918, CGNAT, IPv6 ULA/link-local);
/// Internet = everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Scope {
    /// Apply to all remote addresses.
    #[default]
    Both,
    /// Apply only to LAN (private) remotes.
    Lan,
    /// Apply only to Internet (public) remotes.
    Internet,
}

/// A per-application bandwidth rule, identified by executable path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLimit {
    /// Absolute executable path the rule targets.
    pub exe: String,
    /// Display name (executable basename).
    pub name: String,
    /// Inbound (download) cap, bytes/sec; `None` = unlimited.
    pub down_bps: Option<u64>,
    /// Outbound (upload) cap, bytes/sec; `None` = unlimited.
    pub up_bps: Option<u64>,
    /// Which traffic the rule applies to.
    #[serde(default)]
    pub scope: Scope,
    /// Live processes currently tracked for this app.
    pub pids: u32,
}

/// The limiter's current configuration and master-switch state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimiterState {
    /// Master switch: when false, no shaping is applied even if caps are set.
    pub enabled: bool,
    /// Configured host-wide caps.
    pub host: HostLimit,
    /// The interface CURB is shaping (e.g. `eno1`).
    pub interface: String,
}

/// A point-in-time snapshot of the daemon's health and master switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Protocol version the daemon speaks ([`PROTOCOL_VERSION`]).
    pub protocol_version: u32,
    /// `curbd` package version.
    pub daemon_version: String,
    /// Daemon process id.
    pub pid: u32,
    /// Seconds since the daemon started serving.
    pub uptime_secs: u64,
    /// Global master switch for limiting. Always `false` until P2 lands the
    /// traffic engine; surfaced now so the GUI can render the toggle early.
    pub limiter_enabled: bool,
}

/// How CURB is currently treating an application's traffic.
///
/// In P1 everything is [`AppStatus::Watching`]; the limited/throttled states
/// arrive with the traffic engine (P2/P3) but are defined now so the GUI can
/// render the pills without another protocol bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppStatus {
    /// Observed only — no limit applied.
    Watching,
    /// A rate limit is configured and being enforced.
    Limited,
    /// Currently clamped to near-zero (e.g. quota exceeded).
    Throttled,
}

/// Live traffic for a single application, identified by its executable path.
///
/// Rates are bytes per second over the last sampling interval; totals are bytes
/// accumulated since the daemon started observing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStat {
    /// Absolute executable path — the stable application identity.
    pub exe: String,
    /// Display name (executable basename).
    pub name: String,
    /// Number of live processes currently attributed to this application.
    pub pids: u32,
    /// Current download (inbound) rate, bytes/sec.
    pub down_bps: u64,
    /// Current upload (outbound) rate, bytes/sec.
    pub up_bps: u64,
    /// Total bytes downloaded since observation began.
    pub down_total: u64,
    /// Total bytes uploaded since observation began.
    pub up_total: u64,
    /// Recent download rates (oldest→newest) for sparklines, bytes/sec.
    pub down_spark: Vec<u32>,
    /// Recent upload rates (oldest→newest) for sparklines, bytes/sec.
    pub up_spark: Vec<u32>,
    /// How CURB is treating this app's traffic.
    pub status: AppStatus,
    /// Zone breakdown — bytes/sec attributed to LAN remote addresses.
    #[serde(default)]
    pub lan_down_bps: u64,
    #[serde(default)]
    pub lan_up_bps: u64,
    /// Zone breakdown — bytes/sec attributed to Internet remote addresses.
    #[serde(default)]
    pub inet_down_bps: u64,
    #[serde(default)]
    pub inet_up_bps: u64,
}

/// Aggregate host-wide traffic totals, with a LAN/Internet rate breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostTotals {
    pub down_bps: u64,
    pub up_bps: u64,
    pub down_total: u64,
    pub up_total: u64,
    /// Current rates split by remote-address zone (bytes/sec).
    #[serde(default)]
    pub lan_down_bps: u64,
    #[serde(default)]
    pub lan_up_bps: u64,
    #[serde(default)]
    pub inet_down_bps: u64,
    #[serde(default)]
    pub inet_up_bps: u64,
}

/// A complete point-in-time view of live traffic, returned by [`Request::ListApps`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    /// Per-application stats, sorted by the daemon (busiest first).
    pub apps: Vec<AppStat>,
    /// Host-wide totals.
    pub host: HostTotals,
    /// Unix epoch milliseconds when this snapshot was produced.
    pub timestamp_ms: u64,
}

/// Errors raised by the transport and client helpers.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("daemon closed the connection")]
    Closed,
}

pub type Result<T> = std::result::Result<T, ProtoError>;

/// Length-delimited, JSON-framed connection over any async byte stream.
///
/// Generic over the underlying transport so the daemon (server side, from
/// `accept()`) and clients (from `connect()`) frame messages identically.
pub mod transport {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio_util::codec::{Framed, LengthDelimitedCodec};

    /// A framed message connection.
    pub struct Connection<T> {
        inner: Framed<T, LengthDelimitedCodec>,
    }

    impl<T> Connection<T>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        /// Wrap an async byte stream in the CURB framing.
        pub fn new(io: T) -> Self {
            Self {
                inner: Framed::new(io, LengthDelimitedCodec::new()),
            }
        }

        /// Serialize and send one message frame.
        pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<()> {
            let bytes = serde_json::to_vec(msg)?;
            self.inner.send(bytes.into()).await?;
            Ok(())
        }

        /// Receive and deserialize the next message frame.
        ///
        /// Returns `Ok(None)` on a clean end-of-stream (peer closed).
        pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>> {
            match self.inner.next().await {
                Some(frame) => {
                    let frame = frame?;
                    Ok(Some(serde_json::from_slice(&frame)?))
                }
                None => Ok(None),
            }
        }
    }
}

/// A thin client for talking to `curbd` over the control socket.
pub struct Client {
    conn: transport::Connection<tokio::net::UnixStream>,
}

impl Client {
    /// Connect to the daemon at the resolved [`socket_path`].
    pub async fn connect() -> Result<Self> {
        Self::connect_to(socket_path()).await
    }

    /// Connect to the daemon at a specific socket path.
    pub async fn connect_to(path: impl AsRef<Path>) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(path.as_ref()).await?;
        Ok(Self {
            conn: transport::Connection::new(stream),
        })
    }

    /// Send one request and await exactly one response.
    pub async fn request(&mut self, req: &Request) -> Result<Response> {
        self.conn.send(req).await?;
        self.conn.recv::<Response>().await?.ok_or(ProtoError::Closed)
    }
}
