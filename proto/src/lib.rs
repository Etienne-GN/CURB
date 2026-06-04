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
    /// Set the host-wide rate caps and enable limiting (P2). `None` for a
    /// direction means "unlimited", which is how inbound-only / outbound-only
    /// limits are expressed.
    SetHostLimit {
        down_bps: Option<u64>,
        up_bps: Option<u64>,
    },
    /// Flip the global master switch without changing configured caps (P2).
    SetLimiterEnabled(bool),
    /// Read the current limiter state (P2).
    GetLimiter,
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
    /// Generic success for requests with no payload.
    Ok,
    /// The request could not be served.
    Error { message: String },
}

/// Host-wide rate caps. `None` in a direction means unlimited (no shaping),
/// which expresses inbound-only / outbound-only limits.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct HostLimit {
    /// Inbound (download) cap, bytes/sec; `None` = unlimited.
    pub down_bps: Option<u64>,
    /// Outbound (upload) cap, bytes/sec; `None` = unlimited.
    pub up_bps: Option<u64>,
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
}

/// Aggregate host-wide traffic totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostTotals {
    pub down_bps: u64,
    pub up_bps: u64,
    pub down_total: u64,
    pub up_total: u64,
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
