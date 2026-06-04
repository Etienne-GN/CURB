# CURB — GUI visual mockup brief (throwaway, look-and-feel only)

You are designing the main window of **CURB**, a Linux desktop app for
**per-application network bandwidth monitoring and control** — think NetLimiter
(Windows), but it must look **distinct, modern, and more intuitive**, not a
clone. This is a **static visual mockup only**: no real data, no backend.

## Deliverable
A **single self-contained `index.html`** (all CSS and JS inline, no external
fonts/CDNs except system fonts) that opens directly in a browser at a desktop
window size (~1440×900). Use a small amount of vanilla JS to make the numbers
**feel alive**: gently animate the per-app download/upload rates and the
sparklines every ~1s with random-walk values so it looks like a live monitor.
No build step. Do not create any other files.

## Aesthetic
Dark, calm, high-contrast, professional. Rounded corners (8–10px), subtle
borders, soft shadows, no harsh pure-black. Tabular numbers for rates. Density
similar to a pro network tool (compact but readable rows ~36–40px).

### Color palette (use exactly these)
- App background:        `#0b0e14`
- Surface / panels:      `#131722`
- Elevated surface/rows: `#1a2030`
- Border / dividers:     `#232a39`
- Text primary:          `#e6e9ef`
- Text muted:            `#8a93a6`
- Inbound / download:    `#38bdf8`  (cyan)
- Outbound / upload:     `#fb923c`  (amber)
- Brand / primary action:`#34d399`  (emerald) — used for the master switch & CTAs
- Over-quota / danger:   `#fb7185`  (rose)
- LAN pill:              muted teal; Internet pill: muted indigo

## Layout (three zones)

1. **Left sidebar** (~220px): the CURB wordmark/logo at top (a simple emerald
   mark is fine), then vertical nav with icons + labels:
   Dashboard, Applications (active), Rules, Quotas, Connections, Settings.
   Subtle active-item highlight.

2. **Top bar** (across the main area): on the left a large **master switch**
   labeled "Limiter" reading **ON** in emerald. In the center/right, two live
   host-total readouts — **↓ total** in cyan and **↑ total** in amber, each with
   a tiny live sparkline — plus a compact "Host limit: ↓ 100 Mbit / ↑ 20 Mbit"
   control chip. A search box on the far right.

3. **Main content — the Applications table** (the hero of the app). A live list
   of applications, one row each, with these columns:
   - **Application**: a small icon tile + app name (bold) + executable path (muted, smaller).
   - **Status**: a pill — "Limited" (emerald), "Watching" (muted), or "Throttled" (rose).
   - **↓ Rate**: live download rate (cyan, tabular) + a tiny inline sparkline.
   - **↑ Rate**: live upload rate (amber, tabular) + a tiny inline sparkline.
   - **↓ Limit / ↑ Limit**: the configured caps, shown as small editable-looking
     chips (e.g. "5 Mbit", or "—" for no limit). They don't need to function.
   - **Scope**: small pills showing LAN and/or Internet.
   - **Today**: a slim horizontal quota-usage bar (used vs cap) with a label like
     "3.2 / 10 GB"; one row should be near-full and tinted rose (over quota).

   Use **8–10 realistic rows**, e.g.: Firefox, Steam, Spotify, Visual Studio
   Code, qBittorrent (the throttled/over-quota one), Discord, Dropbox, OBS
   Studio, curl, systemd-resolved. Give them plausible rates (qBittorrent high,
   resolved tiny, etc.).

4. **Bottom detail strip** (optional, nice-to-have): when a row is "selected"
   (just style the first row as selected), show a slim panel with two larger live
   area-graphs (download cyan, upload amber) for that app over the last 60s.

## Interaction polish (cosmetic only)
- Hover states on rows and nav items.
- The master switch and the limit chips should *look* clickable.
- Smooth number transitions on the animated rates.

## Out of scope
No routing, no real settings, no other pages — just this one window rendered
beautifully. Optimize for "screenshot that sells the aesthetic."
