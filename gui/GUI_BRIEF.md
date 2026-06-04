# CURB GUI frontend brief (wire the approved mockup to live Tauri data)

You are building the **real frontend** for CURB, a Linux desktop app (Tauri v2)
for per-application network bandwidth monitoring and control. A static visual
mockup already exists and is **approved** — reproduce its look exactly, but
replace its fake data with **live data from the Tauri backend**.

## Inputs
- **Visual reference (match this exactly):** `../design/mockup/index.html`
  — same dark palette, layout, sidebar, top bar, applications table, and bottom
  detail strip. Reuse its CSS wholesale; only change the data wiring.

## Deliverable
Replace the files in `./src/`:
- `src/index.html`, `src/styles.css`, `src/main.js`
Vanilla JS only, **no build step, no external dependencies** (no npm imports).
Tauri exposes the backend on `window.__TAURI__.core.invoke` (global is enabled).
Start `main.js` with: `const { invoke } = window.__TAURI__.core;`
Do not edit anything under `src-tauri/`.

## Backend API (Tauri commands)
Call with `await invoke('<name>', <argsObject>)`. **Argument keys are camelCase**
(Tauri converts them to snake_case). **Response object fields are snake_case**
(exactly as listed). Every command may throw a string error (catch it).

Read (poll these):
- `invoke('list_apps')` → `MonitorSnapshot`
- `invoke('get_limiter')` → `LimiterState`
- `invoke('list_app_limits')` → `AppLimit[]`
- `invoke('list_quotas')` → `QuotaStatus[]`

Mutate (call on user action):
- `invoke('set_limiter_enabled', { enabled: boolean })` → LimiterState
- `invoke('set_host_limit', { downBps: number|null, upBps: number|null })` → LimiterState
- `invoke('set_app_limit', { exe: string, downBps: number|null, upBps: number|null, scope: 'both'|'lan'|'internet' })` → AppLimit[]
- `invoke('clear_app_limit', { exe: string })` → AppLimit[]
- `invoke('set_quota', { exe: string, budgetBytes: number, period: 'hourly'|'daily'|'weekly'|'monthly'|'total', direction: 'down'|'up'|'both' })` → QuotaStatus[]
- `invoke('clear_quota', { exe: string })` → QuotaStatus[]

## Data shapes (response fields are snake_case)
```
MonitorSnapshot { apps: AppStat[], host: HostTotals, timestamp_ms: number }
AppStat {
  exe: string,            // absolute path, "" for unresolved
  name: string,           // basename, e.g. "firefox"
  pids: number,
  down_bps: number,       // bytes/sec
  up_bps: number,
  down_total: number,     // bytes since start
  up_total: number,
  down_spark: number[],   // recent down rates (bytes/sec), oldest->newest, up to 60
  up_spark: number[],
  status: string          // ignore; derive status yourself (see below)
}
HostTotals { down_bps, up_bps, down_total, up_total }   // all bytes or bytes/sec
LimiterState { enabled: boolean, host: { down_bps: number|null, up_bps: number|null }, interface: string }
AppLimit { exe, name, down_bps: number|null, up_bps: number|null, scope: 'Both'|'Lan'|'Internet', pids }
QuotaStatus { exe, name, budget_bytes, used_bytes, period, direction, exceeded: boolean, resets_in_secs: number|null }
```
Note response enum values are Capitalized ('Both','Lan','Internet'; 'Daily' etc.)
but when you SEND scope/period/direction you must use lowercase strings.

## Behavior
1. **Live loop:** every 1000 ms, `list_apps()` and refresh the table + host
   totals + sparklines from real data. Also refresh `get_limiter`,
   `list_app_limits`, `list_quotas` every ~1–2 s and **join by `exe`** so each
   app row shows its configured limits, scope, and quota usage.
2. **Applications table rows** (one per app from list_apps, busiest first; the
   backend already sorts):
   - Name + exe path (muted). Skip rows with empty exe or roll them into an
     "(unresolved)" row labeled as such.
   - **Status pill** — derive: `Throttled` (rose) if a matching quota has
     `exceeded`; else `Limited` (emerald) if a matching AppLimit exists; else
     `Watching` (muted).
   - **↓ Rate / ↑ Rate** — `down_bps`/`up_bps` formatted as bit-rate, plus a
     live sparkline drawn from `down_spark`/`up_spark` (real arrays).
   - **↓ Limit / ↑ Limit** — show the matching AppLimit's caps as clickable
     chips ("5 Mbit" or "—"). Clicking prompts for a rate (accept e.g.
     `5mbit`, `500kbit`, `2MB`; parse to bytes/sec) and calls `set_app_limit`
     with the current scope (default 'both'); empty/clear input → unlimited for
     that direction (send null). If both become null and no quota, call
     `clear_app_limit`.
   - **Scope** — pills (LAN / Internet / Both). Clicking cycles
     both→lan→internet and re-applies via `set_app_limit` (only if the app has
     a limit; otherwise just remember the choice for the next limit).
   - **Today** — quota usage bar from the matching QuotaStatus
     (`used_bytes`/`budget_bytes`, label like "3.2 / 10 GB"), tinted rose when
     `exceeded`. If no quota, show a faint "—" or empty bar; clicking the cell
     prompts to set a quota (budget like `10GB`, default period daily,
     direction both) via `set_quota`.
3. **Top bar:**
   - **Master switch "Limiter ON/OFF"** reflects `LimiterState.enabled`;
     clicking calls `set_limiter_enabled(!enabled)`.
   - **Host ↓/↑ totals** from `host` with live sparklines (keep a rolling
     client-side history array, ~60 points).
   - **Host limit chip** shows configured host caps (`LimiterState.host`);
     clicking prompts for down/up rates → `set_host_limit`.
   - Show the `interface` name somewhere subtle.
4. **Bottom detail strip:** when a row is selected, show that app's
   `down_spark`/`up_spark` as the two 60-second area graphs (cyan down, amber
   up). Select the first row by default.
5. **Error handling:** if any invoke throws (daemon down / not root), show a
   dismissible banner like "⚠ Cannot reach curbd — is the daemon running as
   root?" and keep the last-rendered UI; keep retrying on the next tick.

## Formatting helpers (implement in JS)
- bit-rate: bytes/sec × 8 → `Gbit`/`Mbit`/`Kbit`/`bit` (1 decimal).
- bytes: binary `GB`/`MB`/`KB`/`B`.
- rate parser: suffixes `gbit|mbit|kbit|bit` (bits/s) and `gb|mb|kb|b` (bytes),
  bare number = bytes/sec → return bytes/sec.
- size parser (quota budget): `gb|mb|kb|b` (+ `gib|mib|kib`) → bytes.

## Polish
Match the mockup's hover states, rounded corners, tabular numerals, and smooth
number transitions. Keep it feeling like a live, professional network tool.
Optimize for "looks and behaves like the mockup, but with real data."
