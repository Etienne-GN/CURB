# CURB GUI redesign brief — NetLimiter-style treeview + arrow spinners

Evolve the existing CURB frontend (`gui/src/{index.html,styles.css,main.js}`)
into a **NetLimiter-style tree** with inline up/down **arrow spinner** limit
controls. KEEP the current dark theme, palette, top bar, sparklines, and live
polling — reuse `styles.css` and extend it; do not restart from scratch. Tauri
globals are on (`const { invoke } = window.__TAURI__.core;`). No build step, no
external deps. Only edit files under `gui/src/`.

## The main view becomes a TREE (replacing the flat applications table)

A single tree grid with expandable rows. Columns stay the same as today:
`NAME | STATUS | ↓ RATE | ↑ RATE | ↓ LIMIT | ↑ LIMIT | SCOPE | TODAY`.

Top-level rows:
1. **Host** (label it "This computer") — a root node showing the host's total
   ↓/↑ **rate** (from `list_apps().host`) and total ↓/↑ **limit** controls.
   Expand it (▸/▾) to reveal two child rows:
   - **Local Network** — its own ↓/↑ limit controls (caps LAN traffic).
   - **Internet** — its own ↓/↑ limit controls (caps Internet traffic).
   (The host's rate columns can be blank on the LAN/Internet children, or show
   the totals; limits are the point there.)
2. **Applications** group header, then one row per app from `list_apps().apps`
   (busiest first, skip empty `exe`), exactly like the current table rows
   (status pill, live ↓/↑ rate + sparkline, ↓/↑ limit controls, scope pills,
   Today quota bar). Indent app rows one level under the group.

Use disclosure triangles for Host (expanded by default) and the Applications
group. Indent children with a left guide line for the tree feel.

## Arrow spinner limit control (the ↓ LIMIT / ↑ LIMIT cells)

Replace the plain limit chips with a compact control: the current value (e.g.
`5 Mbit`, or `—` for unlimited) with tiny **▲** and **▼** buttons. Behaviour:
- **▲** steps the cap UP to the next preset; **▼** steps DOWN; below the lowest
  preset goes to **unlimited** (`—`, sent as `null`).
- Preset ladder (bytes/sec shown as labels): unlimited, 256 Kbit, 512 Kbit,
  1 Mbit, 2 Mbit, 5 Mbit, 10 Mbit, 20 Mbit, 50 Mbit, 100 Mbit, 200 Mbit,
  500 Mbit, 1 Gbit. (Convert: Mbit→bytes/s = value*1e6/8.)
- Clicking the value text opens an inline text input to type any rate
  (`5mbit`, `500kbit`, `2MB`); Enter applies, Esc cancels. Implement a small
  rate parser (bit units `kbit|mbit|gbit`, byte units `kb|mb|gb`, bare =
  bytes/s).
- Show a subtle "applying…" state and revert on error (display the error
  banner).

## Wiring (Tauri commands)

Reads (poll as today): `list_apps()`, `get_limiter()`, `list_app_limits()`,
`list_quotas()`. Response fields are snake_case.

`get_limiter()` → `LimiterState`:
```
{ enabled, interface,
  host: { down_bps|null, up_bps|null,
          lan:      { down_bps|null, up_bps|null },
          internet: { down_bps|null, up_bps|null } } }
```

Mutations (args camelCase, null = unlimited):
- Host limits — `set_host_limit` REPLACES the whole host limit, so always send
  the full current host state with the one field you changed:
  `invoke('set_host_limit', { downBps, upBps, lanDownBps, lanUpBps, inetDownBps, inetUpBps })`
  e.g. changing the LAN ↓ control: keep `downBps/upBps/lanUpBps/inetDownBps/inetUpBps`
  from the current `get_limiter().host` and set `lanDownBps` to the new value.
- Per-app limits:
  `invoke('set_app_limit', { exe, downBps, upBps, scope })` (scope:
  'both'|'lan'|'internet'); if both become null and no quota, call
  `invoke('clear_app_limit', { exe })`.
- Master switch (top bar): `invoke('set_limiter_enabled', { enabled })`.
- Quotas (Today cell, as today): `set_quota`/`clear_quota`.

Join apps↔limits↔quotas by `exe` for status pills (Throttled if quota exceeded,
Limited if an AppLimit exists, else Watching), limit values, scope, and the
Today bar. Derive each row's current limit for the spinner from
`list_app_limits()`; for the Host/LAN/Internet rows use `get_limiter().host`.

## Keep
The dark palette + colors (cyan down / amber up / emerald brand / rose danger),
top bar master switch + host totals + sparklines, the bottom detail strip with
60s graphs, hover states, tabular numerals, and the 1s live refresh. The result
should read like NetLimiter: a tidy tree with quick ▲▼ limit nudging per row,
host split into Local Network and Internet.
