//! `curb` — the CURB command-line client.
//!
//! A thin front-end over [`curb_proto::Client`]. P0 ships `ping` and `status`;
//! `host`, `app`, `top`, and `quota` subcommands arrive with their phases.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use curb_proto::{Client, Request, Response};

#[derive(Parser)]
#[command(
    name = "curb",
    version,
    about = "CURB — control the per-application bandwidth engine"
)]
struct Cli {
    /// Control socket path (overrides $CURB_SOCK and the built-in default).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check that the daemon is alive and measure round-trip latency.
    Ping,
    /// Show the daemon's status.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut client = match &cli.socket {
        Some(path) => Client::connect_to(path).await,
        None => Client::connect().await,
    }
    .map_err(|e| anyhow::anyhow!("could not connect to curbd ({e}). Is the daemon running?"))?;

    match cli.cmd {
        Cmd::Ping => {
            let start = Instant::now();
            match client.request(&Request::Ping).await? {
                Response::Pong => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    println!("pong ({ms:.2} ms)");
                }
                other => bail!("unexpected response to ping: {other:?}"),
            }
        }
        Cmd::Status => match client.request(&Request::GetStatus).await? {
            Response::Status(s) => {
                let on = if s.limiter_enabled { "on" } else { "off" };
                println!("curbd v{}  (pid {})", s.daemon_version, s.pid);
                println!("  protocol : v{}", s.protocol_version);
                println!("  uptime   : {}", format_uptime(s.uptime_secs));
                println!("  limiter  : {on}");
            }
            Response::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected response to status: {other:?}"),
        },
    }

    Ok(())
}

/// Render a duration in seconds as `1d 2h 3m 4s`, trimming leading zero units.
fn format_uptime(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 || !parts.is_empty() {
        parts.push(format!("{h}h"));
    }
    if m > 0 || !parts.is_empty() {
        parts.push(format!("{m}m"));
    }
    parts.push(format!("{s}s"));
    parts.join(" ")
}
