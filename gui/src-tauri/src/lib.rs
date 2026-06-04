//! CURB GUI backend (Tauri v2).
//!
//! Thin bridge between the web frontend and the `curbd` daemon: each command
//! opens a short-lived [`curb_proto::Client`] to the control socket, issues one
//! request, and returns the typed response (serialized to JSON for the
//! webview). The frontend polls `list_apps` for the live table and calls the
//! mutating commands for the controls. No state is held here — the daemon is
//! the single source of truth.

use curb_proto::{
    AppLimit, Client, Direction, LimiterState, MonitorSnapshot, QuotaPeriod, QuotaStatus, Request,
    Response, Scope,
};

/// Connect to the daemon at the resolved socket path (`$CURB_SOCK` or default).
async fn client() -> Result<Client, String> {
    Client::connect()
        .await
        .map_err(|e| format!("cannot reach curbd ({e}). Is the daemon running?"))
}

/// Send one request and return the response, mapping protocol errors to strings.
async fn call(req: Request) -> Result<Response, String> {
    let mut c = client().await?;
    c.request(&req).await.map_err(|e| e.to_string())
}

fn parse_scope(s: &str) -> Scope {
    match s {
        "lan" => Scope::Lan,
        "internet" => Scope::Internet,
        _ => Scope::Both,
    }
}

fn parse_direction(s: &str) -> Direction {
    match s {
        "down" => Direction::Down,
        "up" => Direction::Up,
        _ => Direction::Both,
    }
}

fn parse_period(s: &str) -> QuotaPeriod {
    match s {
        "hourly" => QuotaPeriod::Hourly,
        "weekly" => QuotaPeriod::Weekly,
        "monthly" => QuotaPeriod::Monthly,
        "total" => QuotaPeriod::Total,
        _ => QuotaPeriod::Daily,
    }
}

// ---- Read commands --------------------------------------------------------

/// Live per-application traffic snapshot (polled by the frontend).
#[tauri::command]
async fn list_apps() -> Result<MonitorSnapshot, String> {
    match call(Request::ListApps).await? {
        Response::Apps(s) => Ok(s),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Current host-wide limiter state.
#[tauri::command]
async fn get_limiter() -> Result<LimiterState, String> {
    match call(Request::GetLimiter).await? {
        Response::Limiter(s) => Ok(s),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Configured per-application rules.
#[tauri::command]
async fn list_app_limits() -> Result<Vec<AppLimit>, String> {
    match call(Request::ListAppLimits).await? {
        Response::AppLimits(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Configured quotas with live usage.
#[tauri::command]
async fn list_quotas() -> Result<Vec<QuotaStatus>, String> {
    match call(Request::ListQuotas).await? {
        Response::Quotas(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

// ---- Mutating commands ----------------------------------------------------

/// Set host-wide caps (bytes/sec; `null` = unlimited) and enable limiting.
#[tauri::command]
async fn set_host_limit(
    down_bps: Option<u64>,
    up_bps: Option<u64>,
) -> Result<LimiterState, String> {
    match call(Request::SetHostLimit { down_bps, up_bps }).await? {
        Response::Limiter(s) => Ok(s),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Flip the global master switch.
#[tauri::command]
async fn set_limiter_enabled(enabled: bool) -> Result<LimiterState, String> {
    match call(Request::SetLimiterEnabled(enabled)).await? {
        Response::Limiter(s) => Ok(s),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Create/update a per-application rule. `scope` is "both" | "lan" | "internet".
#[tauri::command]
async fn set_app_limit(
    exe: String,
    down_bps: Option<u64>,
    up_bps: Option<u64>,
    scope: String,
) -> Result<Vec<AppLimit>, String> {
    let req = Request::SetAppLimit {
        exe,
        down_bps,
        up_bps,
        scope: parse_scope(&scope),
    };
    match call(req).await? {
        Response::AppLimits(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Remove a per-application rule.
#[tauri::command]
async fn clear_app_limit(exe: String) -> Result<Vec<AppLimit>, String> {
    match call(Request::ClearAppLimit { exe }).await? {
        Response::AppLimits(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Set a data quota. `period` is "hourly"|"daily"|"weekly"|"monthly"|"total";
/// `direction` is "down"|"up"|"both".
#[tauri::command]
async fn set_quota(
    exe: String,
    budget_bytes: u64,
    period: String,
    direction: String,
) -> Result<Vec<QuotaStatus>, String> {
    let req = Request::SetQuota {
        exe,
        budget_bytes,
        period: parse_period(&period),
        direction: parse_direction(&direction),
    };
    match call(req).await? {
        Response::Quotas(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Remove an application's quota.
#[tauri::command]
async fn clear_quota(exe: String) -> Result<Vec<QuotaStatus>, String> {
    match call(Request::ClearQuota { exe }).await? {
        Response::Quotas(v) => Ok(v),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_apps,
            get_limiter,
            list_app_limits,
            list_quotas,
            set_host_limit,
            set_limiter_enabled,
            set_app_limit,
            clear_app_limit,
            set_quota,
            clear_quota,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
