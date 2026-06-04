//! `curb` — the CURB command-line client.
//!
//! A thin front-end over [`curb_proto::Client`]. P0 shipped `ping`/`status`;
//! P1 adds `apps` (one-shot live snapshot) and `top` (auto-refreshing view).
//! `host`, `app`, and `quota` subcommands arrive with their phases.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use curb_proto::{AppStat, Client, HostTotals, LimiterState, MonitorSnapshot, Request, Response};

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
    /// Print a one-shot snapshot of live per-application traffic.
    Apps,
    /// Live, auto-refreshing per-application traffic (like `top`).
    Top {
        /// Refresh interval in seconds.
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
    },
    /// Host-wide bandwidth limits (master switch + total caps).
    #[command(subcommand)]
    Host(HostCmd),
}

#[derive(Subcommand)]
enum HostCmd {
    /// Set host-wide caps and enable limiting. Omitting a direction leaves it
    /// unlimited (this is how you do inbound-only / outbound-only).
    Limit(HostLimitArgs),
    /// Disable limiting (keeps configured caps for the next `on`).
    Off,
    /// Re-enable limiting with the previously configured caps.
    On,
    /// Show the current limiter state.
    Show,
}

#[derive(Args)]
struct HostLimitArgs {
    /// Download cap, e.g. `10mbit`, `500kbit`, `2MB`. Omit for unlimited.
    #[arg(long, value_name = "RATE")]
    down: Option<String>,
    /// Upload cap, e.g. `2mbit`, `256kbit`, `1MB`. Omit for unlimited.
    #[arg(long, value_name = "RATE")]
    up: Option<String>,
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
        Cmd::Apps => {
            let snap = fetch_apps(&mut client).await?;
            print!("{}", render_table(&snap, usize::MAX));
        }
        Cmd::Top { interval } => run_top(&mut client, interval).await?,
        Cmd::Host(host) => run_host(&mut client, host).await?,
    }

    Ok(())
}

/// Handle the `host` subcommands.
async fn run_host(client: &mut Client, cmd: HostCmd) -> Result<()> {
    let req = match &cmd {
        HostCmd::Limit(args) => {
            let down_bps = args
                .down
                .as_deref()
                .map(parse_rate)
                .transpose()
                .context("parsing --down")?;
            let up_bps = args
                .up
                .as_deref()
                .map(parse_rate)
                .transpose()
                .context("parsing --up")?;
            if down_bps.is_none() && up_bps.is_none() {
                bail!("specify at least one of --down or --up");
            }
            Request::SetHostLimit { down_bps, up_bps }
        }
        HostCmd::Off => Request::SetLimiterEnabled(false),
        HostCmd::On => Request::SetLimiterEnabled(true),
        HostCmd::Show => Request::GetLimiter,
    };

    match client.request(&req).await? {
        Response::Limiter(state) => print_limiter(&state),
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

fn print_limiter(s: &LimiterState) {
    let sw = if s.enabled { "ON" } else { "OFF" };
    let fmt = |c: Option<u64>| c.map(format_rate).unwrap_or_else(|| "unlimited".to_string());
    println!("limiter  : {sw}   (interface {})", s.interface);
    println!("  ↓ down : {}", fmt(s.host.down_bps));
    println!("  ↑ up   : {}", fmt(s.host.up_bps));
}

/// Fetch a single live snapshot, mapping protocol errors to anyhow.
async fn fetch_apps(client: &mut Client) -> Result<MonitorSnapshot> {
    match client.request(&Request::ListApps).await? {
        Response::Apps(snap) => Ok(snap),
        Response::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected response to apps: {other:?}"),
    }
}

/// Auto-refreshing table until interrupted with Ctrl-C.
async fn run_top(client: &mut Client, interval: f64) -> Result<()> {
    let interval = std::time::Duration::from_secs_f64(interval.max(0.1));
    // Alternate screen buffer + hidden cursor for a clean live view.
    print!("\x1b[?1049h\x1b[?25l");
    let result = top_loop(client, interval).await;
    // Restore the terminal no matter how we exit.
    print!("\x1b[?25h\x1b[?1049l");
    use std::io::Write;
    std::io::stdout().flush().ok();
    result
}

async fn top_loop(client: &mut Client, interval: std::time::Duration) -> Result<()> {
    loop {
        let snap = fetch_apps(client).await?;
        // Clear + home, then draw.
        print!("\x1b[2J\x1b[H{}", render_table(&snap, 25));
        println!("\n  refreshing every {:.1}s — Ctrl-C to quit", interval.as_secs_f64());
        use std::io::Write;
        std::io::stdout().flush().ok();

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// Render the host header plus a per-application table, capped at `max_rows`.
fn render_table(snap: &MonitorSnapshot, max_rows: usize) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let HostTotals {
        down_bps,
        up_bps,
        down_total,
        up_total,
    } = snap.host;

    let _ = writeln!(
        out,
        "  HOST   ↓ {:>11}  ↑ {:>11}     (total ↓ {} / ↑ {})",
        format_rate(down_bps),
        format_rate(up_bps),
        format_bytes(down_total),
        format_bytes(up_total),
    );
    let _ = writeln!(
        out,
        "  {:<28} {:>3}  {:>11}  {:>11}  {:>9}  {:>9}",
        "APPLICATION", "PID", "↓ RATE", "↑ RATE", "↓ TOTAL", "↑ TOTAL"
    );
    let _ = writeln!(out, "  {}", "─".repeat(80));

    if snap.apps.is_empty() {
        let _ = writeln!(out, "  (no traffic observed yet)");
        return out;
    }

    for app in snap.apps.iter().take(max_rows) {
        let AppStat {
            ref name,
            pids,
            down_bps,
            up_bps,
            down_total,
            up_total,
            ..
        } = *app;
        let _ = writeln!(
            out,
            "  {:<28} {:>3}  {:>11}  {:>11}  {:>9}  {:>9}",
            truncate(name, 28),
            pids,
            format_rate(down_bps),
            format_rate(up_bps),
            format_bytes(down_total),
            format_bytes(up_total),
        );
    }
    out
}

/// Parse a human rate string into **bytes per second**.
///
/// Accepts bit units (`kbit`, `mbit`, `gbit`) and byte units (`kb`/`kbyte`,
/// `mb`/`mbyte`, `gb`/`gbyte`); a bare number is bytes/sec. Case-insensitive,
/// optional spaces, e.g. `10mbit` = 1_250_000 B/s, `2MB` = 2_097_152 B/s.
fn parse_rate(s: &str) -> Result<u64> {
    let t = s.trim().to_ascii_lowercase().replace(' ', "");
    // Longest suffixes first so "mbit" matches before "mb".
    const UNITS: &[(&str, f64)] = &[
        ("gbit", 1e9 / 8.0),
        ("mbit", 1e6 / 8.0),
        ("kbit", 1e3 / 8.0),
        ("bit", 1.0 / 8.0),
        ("gbyte", (1u64 << 30) as f64),
        ("mbyte", (1u64 << 20) as f64),
        ("kbyte", (1u64 << 10) as f64),
        ("gb", (1u64 << 30) as f64),
        ("mb", (1u64 << 20) as f64),
        ("kb", (1u64 << 10) as f64),
        ("b", 1.0),
    ];
    for (suffix, factor) in UNITS {
        if let Some(num) = t.strip_suffix(suffix) {
            let v: f64 = num
                .parse()
                .with_context(|| format!("invalid number in rate '{s}'"))?;
            if v < 0.0 {
                bail!("rate cannot be negative: '{s}'");
            }
            return Ok((v * factor) as u64);
        }
    }
    // No recognized unit: treat as bytes/sec.
    let v: f64 = t
        .parse()
        .with_context(|| format!("unrecognized rate '{s}' (try e.g. 10mbit, 2MB)"))?;
    Ok(v as u64)
}

/// Format a byte/second rate in bits/second (NetLimiter convention).
fn format_rate(bytes_per_sec: u64) -> String {
    let bits = bytes_per_sec as f64 * 8.0;
    for (unit, scale) in [
        ("Gbit", 1e9),
        ("Mbit", 1e6),
        ("Kbit", 1e3),
    ] {
        if bits >= scale {
            return format!("{:.1} {}", bits / scale, unit);
        }
    }
    format!("{bits:.0} bit")
}

/// Format a byte count in binary units (KB/MB/GB).
fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    for (unit, scale) in [
        ("GB", 1u64 << 30),
        ("MB", 1u64 << 20),
        ("KB", 1u64 << 10),
    ] {
        if bytes >= scale {
            return format!("{:.1} {}", b / scale as f64, unit);
        }
    }
    format!("{bytes} B")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bit_and_byte_rates() {
        assert_eq!(parse_rate("10mbit").unwrap(), 1_250_000);
        assert_eq!(parse_rate("8bit").unwrap(), 1);
        assert_eq!(parse_rate("1kbit").unwrap(), 125);
        assert_eq!(parse_rate("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_rate("512kb").unwrap(), 512 * 1024);
        assert_eq!(parse_rate("1000").unwrap(), 1000); // bare = bytes/sec
        assert_eq!(parse_rate("3 Mbit").unwrap(), 375_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_rate("fast").is_err());
        assert!(parse_rate("-5mbit").is_err());
    }
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
