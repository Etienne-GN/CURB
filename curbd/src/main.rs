//! `curbd` — the CURB control daemon.
//!
//! P0 scope: own the control socket and answer the baseline protocol
//! ([`Request::Ping`], [`Request::GetStatus`]). The traffic engine (eBPF, tc,
//! cgroups) is layered on in later phases behind this same socket; the
//! accept/dispatch loop here is the stable spine they plug into.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod engine;
mod monitor;
mod quota;

use anyhow::{Context, Result};
use clap::Parser;
use curb_proto::transport::Connection;
use curb_proto::{socket_path, DaemonStatus, Request, Response, PROTOCOL_VERSION};
use engine::Engine;
use monitor::Monitor;
use quota::QuotaManager;
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "curbd",
    version,
    about = "CURB daemon — per-application bandwidth-control engine"
)]
struct Args {
    /// Control socket path (overrides $CURB_SOCK and the built-in default).
    #[arg(long)]
    socket: Option<PathBuf>,
}

/// Process-wide daemon state shared across connections.
struct State {
    started: Instant,
    /// Live traffic monitor; `None` if capture couldn't start (no CAP_NET_RAW).
    monitor: Option<Monitor>,
    /// Host-wide shaping engine; `None` if no default interface was detected.
    engine: Option<Engine>,
    /// Per-app quota manager; `Some` only when both monitor and engine exist.
    quota: Option<QuotaManager>,
}

impl State {
    fn status(&self) -> DaemonStatus {
        DaemonStatus {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            uptime_secs: self.started.elapsed().as_secs(),
            limiter_enabled: self.engine.as_ref().is_some_and(Engine::enabled),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let path = args.socket.unwrap_or_else(socket_path);

    // Ensure the parent directory exists (systemd creates /run/curbd/ via
    // RuntimeDirectory=curbd; when running manually it may not exist yet).
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }

    // Single-instance assumption: clear a stale socket file from a previous
    // run so bind() succeeds. (A future version should refuse if a live daemon
    // still answers on it.)
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing stale socket {}", path.display()))?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding control socket {}", path.display()))?;

    // 0o660 so members of the `curb` group can drive the daemon without root.
    // When running under systemd with Group=curb the process GID is already
    // `curb`, so the socket inherits it automatically.  When started manually
    // (dev / direct invocation) we chown explicitly so the dev workflow also
    // works after `groupadd curb && usermod -aG curb $USER`.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("setting permissions on {}", path.display()))?;
    set_socket_group(&path);

    // Start the live traffic monitor. If capture can't open (missing
    // CAP_NET_RAW), keep serving — status/ping still work and ListApps reports
    // the monitor as unavailable.
    let monitor = match Monitor::start() {
        Ok(m) => Some(m),
        Err(e) => {
            warn!(error = %e, "traffic monitor unavailable (need CAP_NET_RAW / run as root)");
            None
        }
    };

    // Initialize the shaping engine (detects the default interface; applies no
    // kernel changes until a limit is set).
    let engine = match Engine::new() {
        Ok(e) => Some(e),
        Err(e) => {
            warn!(error = %e, "shaping engine unavailable (no default route?)");
            None
        }
    };

    // The quota manager needs both live accounting (monitor) and enforcement
    // (engine); start it only when both are available.
    let quota = match (&monitor, &engine) {
        (Some(m), Some(e)) => Some(QuotaManager::start(m.clone(), e.clone())),
        _ => None,
    };

    let state = Arc::new(State {
        started: Instant::now(),
        monitor,
        engine,
        quota,
    });

    info!(socket = %path.display(), version = env!("CARGO_PKG_VERSION"), "curbd listening");

    // Serve until interrupted, then clean up the socket file so the next start
    // is tidy.
    let result = serve(&listener, state.clone()).await;

    // Remove all kernel shaping/policing state before exiting.
    if let Some(engine) = &state.engine {
        engine.shutdown();
    }
    if let Err(e) = std::fs::remove_file(&path) {
        debug!(error = %e, "could not remove socket on shutdown");
    }
    result
}

/// Accept loop. Returns `Ok(())` on a graceful Ctrl-C/SIGTERM.
async fn serve(listener: &UnixListener, state: Arc<State>) -> Result<()> {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.context("accepting connection")?;
                let state = state.clone();
                tokio::spawn(async move { handle(Connection::new(stream), state).await; });
            }
            _ = shutdown_signal() => {
                info!("shutdown signal received, stopping");
                return Ok(());
            }
        }
    }
}

/// Resolve when either SIGINT (Ctrl-C) or SIGTERM (systemd stop) arrives.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            // Fall back to Ctrl-C only if SIGTERM can't be registered.
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Serve one client connection until it closes or errors.
async fn handle(mut conn: Connection<UnixStream>, state: Arc<State>) {
    loop {
        match conn.recv::<Request>().await {
            Ok(Some(req)) => {
                let resp = dispatch(req, &state);
                if let Err(e) = conn.send(&resp).await {
                    debug!(error = %e, "failed to send response; dropping connection");
                    break;
                }
            }
            Ok(None) => break, // client closed cleanly
            Err(e) => {
                warn!(error = %e, "protocol error; dropping connection");
                break;
            }
        }
    }
}

/// Map a request to a response. This is where every future feature dispatches.
fn dispatch(req: Request, state: &State) -> Response {
    match req {
        Request::Ping => Response::Pong,
        Request::GetStatus => Response::Status(state.status()),
        Request::ListApps => match &state.monitor {
            Some(m) => Response::Apps(m.snapshot()),
            None => Response::Error {
                message: "traffic monitor unavailable (curbd needs CAP_NET_RAW; run as root)"
                    .to_string(),
            },
        },
        Request::GetLimiter => match &state.engine {
            Some(e) => Response::Limiter(e.state()),
            None => engine_unavailable(),
        },
        Request::SetHostLimit {
            down_bps,
            up_bps,
            lan_down_bps,
            lan_up_bps,
            inet_down_bps,
            inet_up_bps,
        } => match &state.engine {
            Some(e) => {
                let host = curb_proto::HostLimit {
                    down_bps,
                    up_bps,
                    lan: curb_proto::ScopedCap {
                        down_bps: lan_down_bps,
                        up_bps: lan_up_bps,
                    },
                    internet: curb_proto::ScopedCap {
                        down_bps: inet_down_bps,
                        up_bps: inet_up_bps,
                    },
                };
                match e.set_host_limit(host) {
                    Ok(state) => Response::Limiter(state),
                    Err(err) => Response::Error {
                        message: format!("{err:#}"),
                    },
                }
            }
            None => engine_unavailable(),
        },
        Request::SetLimiterEnabled(enabled) => match &state.engine {
            Some(e) => match e.set_enabled(enabled) {
                Ok(state) => Response::Limiter(state),
                Err(err) => Response::Error {
                    message: format!("{err:#}"),
                },
            },
            None => engine_unavailable(),
        },
        Request::SetAppLimit {
            exe,
            down_bps,
            up_bps,
            scope,
        } => match &state.engine {
            Some(e) => match e.set_app_limit(exe, down_bps, up_bps, scope) {
                Ok(limits) => Response::AppLimits(limits),
                Err(err) => Response::Error {
                    message: format!("{err:#}"),
                },
            },
            None => engine_unavailable(),
        },
        Request::ClearAppLimit { exe } => match &state.engine {
            Some(e) => match e.clear_app_limit(&exe) {
                Ok(limits) => Response::AppLimits(limits),
                Err(err) => Response::Error {
                    message: format!("{err:#}"),
                },
            },
            None => engine_unavailable(),
        },
        Request::ListAppLimits => match &state.engine {
            Some(e) => Response::AppLimits(e.list_app_limits()),
            None => engine_unavailable(),
        },
        Request::SetQuota {
            exe,
            budget_bytes,
            period,
            direction,
        } => match &state.quota {
            Some(q) => Response::Quotas(q.set_quota(exe, budget_bytes, period, direction)),
            None => quota_unavailable(),
        },
        Request::ClearQuota { exe } => match &state.quota {
            Some(q) => Response::Quotas(q.clear_quota(&exe)),
            None => quota_unavailable(),
        },
        Request::ListQuotas => match &state.quota {
            Some(q) => Response::Quotas(q.list()),
            None => quota_unavailable(),
        },
        Request::ListConnections => match &state.engine {
            Some(e) => Response::Connections(e.list_connections()),
            None => Response::Connections(Vec::new()),
        },
    }
}

fn quota_unavailable() -> Response {
    Response::Error {
        message: "quotas unavailable (need both the traffic monitor and shaping engine; run as root)"
            .to_string(),
    }
}

fn engine_unavailable() -> Response {
    Response::Error {
        message: "shaping engine unavailable (curbd needs CAP_NET_ADMIN and a default route)"
            .to_string(),
    }
}

/// Set the control socket's group to `curb` (gid looked up at runtime).
///
/// When systemd runs the daemon with `Group=curb` this is a no-op (the socket
/// already inherits the process GID). When started manually as root it makes
/// the socket accessible to members of the `curb` group without a systemd
/// restart, which is the normal dev workflow.
fn set_socket_group(path: &std::path::Path) {
    use std::ffi::CString;
    let cname = CString::new("curb").unwrap();
    // SAFETY: getgrnam is thread-safe when called before threads are spawned;
    // we call it synchronously during startup, before the Tokio runtime accepts
    // any connections.
    let gid = unsafe {
        let grp = libc::getgrnam(cname.as_ptr());
        if grp.is_null() {
            warn!(
                "group `curb` not found — socket is root-only. \
                 Run the install script or: sudo groupadd curb && sudo usermod -aG curb $USER"
            );
            return;
        }
        (*grp).gr_gid
    };
    let c_path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: chown on a path we just created; errors are non-fatal.
    let rc = unsafe { libc::chown(c_path.as_ptr(), libc::uid_t::MAX, gid) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        warn!(error = %err, "could not chown socket to curb group");
    } else {
        info!("socket group set to `curb` (gid {gid})");
    }
}
