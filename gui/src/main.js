const { invoke } = window.__TAURI__.core;

// --- Constants ---
const PRESETS = [
    null,
    256000 / 8, 512000 / 8,
    1000000 / 8, 2000000 / 8, 5000000 / 8,
    10000000 / 8, 20000000 / 8, 50000000 / 8,
    100000000 / 8, 200000000 / 8, 500000000 / 8,
    1000000000 / 8
];

// --- State ---
let apps = [];
let hostTotals = { down_bps: 0, up_bps: 0, down_total: 0, up_total: 0 };
let limiterState = { enabled: false, host: { down_bps: null, up_bps: null, lan: { down_bps: null, up_bps: null }, internet: { down_bps: null, up_bps: null } }, interface: "" };
let appLimits = [];
let quotas = [];
let connections = [];
let selectedExe = null;
let searchQuery = "";
let connFilter = "";
let hostDownHistory = Array(60).fill(0);
let hostUpHistory   = Array(60).fill(0);
let lanDownHistory  = Array(60).fill(0);
let lanUpHistory    = Array(60).fill(0);
let inetDownHistory = Array(60).fill(0);
let inetUpHistory   = Array(60).fill(0);
let expandedNodes = new Set(['host', 'lan', 'internet']); // all expanded by default
let currentView = 'Dashboard';
let pinnedApps = JSON.parse(localStorage.getItem('curb-pinned') || '[]');
let dashGrid = null;
let dashEditing = false;
let dashActiveWidgets = [];

// --- Helpers ---

const APP_COLORS = ['#38bdf8','#fb923c','#34d399','#f472b6','#a78bfa','#fbbf24','#60a5fa','#4ade80','#f87171','#2dd4bf','#e879f9','#fb7185'];
function appColor(str) {
    let h = 0;
    for (let i = 0; i < str.length; i++) h = Math.imul(31, h) + str.charCodeAt(i) | 0;
    return APP_COLORS[Math.abs(h) % APP_COLORS.length];
}

function togglePin(exe) {
    const idx = pinnedApps.indexOf(exe);
    if (idx === -1) pinnedApps.push(exe);
    else pinnedApps.splice(idx, 1);
    localStorage.setItem('curb-pinned', JSON.stringify(pinnedApps));
    renderTable();
    if (currentView === 'Dashboard') initPinnedBars();
}

function formatRate(bps) {
    if (bps === null || bps === undefined) return "—";
    if (bps >= 1e9) return (bps / 1e9).toFixed(2) + " GB/s";
    if (bps >= 1e6) return (bps / 1e6).toFixed(2) + " MB/s";
    if (bps >= 1e3) return (bps / 1e3).toFixed(1) + " KB/s";
    return bps.toFixed(0) + " B/s";
}

function formatSize(bytes) {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(1) + " GB";
    if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + " MB";
    if (bytes >= 1024) return (bytes / 1024).toFixed(1) + " KB";
    return bytes.toFixed(0) + " B";
}

function formatUsage(down, up) {
    const total = (down || 0) + (up || 0);
    return total > 0 ? formatSize(total) : '—';
}

function parseRate(input) {
    if (!input || input === "—" || input === "unlimited" || input === "0") return null;
    const match = input.toLowerCase().match(/^([\d\.]+)\s*(gbit|mbit|kbit|bit|gb|mb|kb|b)?$/);
    if (!match) return null;
    const val = parseFloat(match[1]);
    const unit = match[2] || "b/s";

    switch (unit) {
        case "gbit": return (val * 1000000000) / 8;
        case "mbit": return (val * 1000000) / 8;
        case "kbit": return (val * 1000) / 8;
        case "bit": return val / 8;
        case "gb": return val * 1024 * 1024 * 1024;
        case "mb": return val * 1024 * 1024;
        case "kb": return val * 1024;
        case "b": return val;
        default: return val; // bytes/sec
    }
}

function parseSize(input) {
    if (!input) return null;
    const match = input.toLowerCase().match(/^([\d\.]+)\s*(gib|mib|kib|gb|mb|kb|b)?$/);
    if (!match) return null;
    const val = parseFloat(match[1]);
    const unit = match[2] || "b";

    switch (unit) {
        case "gib":
        case "gb": return val * 1024 * 1024 * 1024;
        case "mib":
        case "mb": return val * 1024 * 1024;
        case "kib":
        case "kb": return val * 1024;
        case "b": return val;
        default: return val;
    }
}

function createSparkline(svg, data, color, width = 40, height = 14) {
    if (!svg) return;
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
    const max = Math.max(...data, 1);
    const points = data.map((d, i) => {
        const x = (i / (data.length - 1)) * width;
        const y = height - (d / max) * height;
        return `${x},${y}`;
    }).join(" ");

    svg.innerHTML = `<polyline points="${points}" stroke="${color}" />`;
}

function createAreaGraph(svg, data, color) {
    if (!svg) return;
    const W = 1000, H = 100;
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);

    // Need ≥ 2 points to draw a line; pad to 60 when data is still accumulating.
    const d = data.length >= 2 ? data : Array(60).fill(0);

    const max = Math.max(...d, 1);
    // Reserve 3px at the bottom so the zero-traffic baseline is always visible
    // (without this, all-zero data places every point at y=H and gets clipped).
    const usableH = H - 3;

    const pts = d.map((v, i) => {
        const x = ((i / (d.length - 1)) * W).toFixed(1);
        const y = (usableH - (v / max) * usableH).toFixed(1);
        return `${x},${y}`;
    });

    const line = pts.join(' ');
    const area = `0,${H} ${line} ${W},${H}`;

    svg.innerHTML = `
        <polygon points="${area}" fill="${color}" class="area-fill"/>
        <polyline points="${line}" stroke="${color}"/>
    `;
}

// --- Helpers ---

function escHtml(s) {
    return String(s)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

function formatResetTime(secs) {
    if (secs === null || secs === undefined) return 'never';
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
    return `${Math.floor(secs / 86400)}d ${Math.floor((secs % 86400) / 3600)}h`;
}

// --- Data Fetching ---

async function poll() {
    try {
        const fetchConns = currentView === 'Connections';
        const [snapshot, limiter, limits, q, conns] = await Promise.all([
            invoke('list_apps'),
            invoke('get_limiter'),
            invoke('list_app_limits'),
            invoke('list_quotas'),
            fetchConns ? invoke('list_connections') : Promise.resolve([]),
        ]);

        apps = snapshot.apps;
        hostTotals = snapshot.host;
        limiterState = limiter;
        appLimits = limits;
        quotas = q;
        if (fetchConns) connections = conns;

        // Update host history
        hostDownHistory.push(hostTotals.down_bps); hostDownHistory.shift();
        hostUpHistory.push(hostTotals.up_bps);     hostUpHistory.shift();
        lanDownHistory.push(hostTotals.lan_down_bps || 0); lanDownHistory.shift();
        lanUpHistory.push(hostTotals.lan_up_bps   || 0);   lanUpHistory.shift();
        inetDownHistory.push(hostTotals.inet_down_bps || 0); inetDownHistory.shift();
        inetUpHistory.push(hostTotals.inet_up_bps   || 0);   inetUpHistory.shift();

        document.getElementById('error-banner').style.display = 'none';
        renderUI();
    } catch (err) {
        console.error("Poll error:", err);
        const msg = (err && (err.message || err.toString())) || String(err);
        const isConn = msg.includes('connect') || msg.includes('socket') || msg.includes('ENOENT');
        document.getElementById('error-banner-msg').textContent =
            isConn ? '⚠ Cannot reach curbd — is the daemon running?'
                   : `⚠ UI error: ${msg}`;
        document.getElementById('error-banner').style.display = 'flex';
    }
}

// --- UI Components ---

function createSpinner(value, onUpdate) {
    const container = document.createElement('div');
    container.className = 'spinner-control';
    
    const valDisplay = document.createElement('div');
    valDisplay.className = 'spinner-value';
    valDisplay.textContent = formatRate(value);
    
    const btns = document.createElement('div');
    btns.className = 'spinner-btns';
    
    const upBtn = document.createElement('div');
    upBtn.className = 'spinner-btn';
    upBtn.textContent = '▲';
    upBtn.onclick = (e) => {
        e.stopPropagation();
        const currentIdx = PRESETS.findIndex(p => p !== null && p >= value);
        const next = currentIdx === -1 ? PRESETS[1] : PRESETS[Math.min(currentIdx + (value === PRESETS[currentIdx] ? 1 : 0), PRESETS.length - 1)];
        onUpdate(next);
    };
    
    const downBtn = document.createElement('div');
    downBtn.className = 'spinner-btn';
    downBtn.textContent = '▼';
    downBtn.onclick = (e) => {
        e.stopPropagation();
        const currentIdx = PRESETS.findIndex(p => p !== null && p >= value);
        const next = currentIdx <= 1 ? null : PRESETS[currentIdx - 1];
        onUpdate(next);
    };

    valDisplay.onclick = (e) => {
        e.stopPropagation();
        const input = document.createElement('input');
        input.className = 'spinner-input';
        input.value = value ? formatRate(value).replace(/\s/g, '').toLowerCase() : "";
        input.onkeydown = (ev) => {
            if (ev.key === 'Enter') {
                onUpdate(parseRate(input.value));
                container.removeChild(input);
            } else if (ev.key === 'Escape') {
                container.removeChild(input);
            }
        };
        input.onblur = () => {
            if (container.contains(input)) container.removeChild(input);
        };
        container.appendChild(input);
        input.focus();
    };

    btns.appendChild(upBtn);
    btns.appendChild(downBtn);
    container.appendChild(valDisplay);
    container.appendChild(btns);
    
    return container;
}

// --- UI Rendering ---

function renderUI() {
    renderTopBar();

    const v = currentView;
    document.getElementById('dashboard-view').style.display    = v === 'Dashboard'    ? 'flex' : 'none';
    document.getElementById('table-container').style.display   = v === 'Applications' ? ''     : 'none';
    document.getElementById('rules-view').style.display        = v === 'Rules'        ? ''     : 'none';
    document.getElementById('quotas-view').style.display       = v === 'Quotas'       ? ''     : 'none';
    document.getElementById('connections-view').style.display  = v === 'Connections'  ? ''     : 'none';
    document.getElementById('settings-view').style.display     = v === 'Settings'     ? ''     : 'none';

    if (!popupOpen) {
        if      (v === 'Dashboard')    renderDashboard();
        else if (v === 'Applications') renderTable();
        else if (v === 'Rules')        renderRules();
        else if (v === 'Quotas')       renderQuotas();
        else if (v === 'Connections')  renderConnections();
        else if (v === 'Settings')     loadSettingsData();
    }
    renderDetailStrip();
}

// ── Dashboard widget system ────────────────────────────────────────────────

function topAppsHtml() {
    const top = [...apps].sort((a, b) => (b.down_bps + b.up_bps) - (a.down_bps + a.up_bps)).slice(0, 5);
    if (top.length === 0) return `<div class="dash-label">Top Applications</div><div class="dash-sub" style="margin-top:14px;">No traffic observed yet.</div>`;
    return `<div class="dash-label">Top Applications</div>
        <div class="dash-app-list">${top.map(a => `
            <div class="dash-app-row">
                <div class="dash-app-icon" style="border-top-color:${appColor(a.exe)}">${escHtml(a.name[0].toUpperCase())}</div>
                <span class="dash-app-name">${escHtml(a.name)}</span>
                <div class="dash-app-rates">
                    <span class="download-text">↓ ${formatRate(a.down_bps)}</span>
                    <span class="upload-text">↑ ${formatRate(a.up_bps)}</span>
                </div>
            </div>`).join('')}
        </div>`;
}

const WIDGET_DEFS = {
    download: {
        title: 'Download', defaultPos: { x: 0, y: 0, w: 4, h: 4 },
        html: () => `<div class="dash-label">↓ Download</div>
            <div class="dash-big download-text" id="wd-dv">${formatRate(hostTotals.down_bps)}</div>
            <svg class="dash-spark" id="wd-ds"></svg>
            <div class="dash-sub" id="wd-dt">${formatSize(hostTotals.down_total)} total since start</div>`,
        tick() {
            const v = document.getElementById('wd-dv'); if (!v) return;
            v.textContent = formatRate(hostTotals.down_bps);
            document.getElementById('wd-dt').textContent = formatSize(hostTotals.down_total) + ' total since start';
            createSparkline(document.getElementById('wd-ds'), hostDownHistory, 'var(--download)', 200, 40);
        }
    },
    upload: {
        title: 'Upload', defaultPos: { x: 4, y: 0, w: 4, h: 4 },
        html: () => `<div class="dash-label">↑ Upload</div>
            <div class="dash-big upload-text" id="wd-uv">${formatRate(hostTotals.up_bps)}</div>
            <svg class="dash-spark" id="wd-us"></svg>
            <div class="dash-sub" id="wd-ut">${formatSize(hostTotals.up_total)} total since start</div>`,
        tick() {
            const v = document.getElementById('wd-uv'); if (!v) return;
            v.textContent = formatRate(hostTotals.up_bps);
            document.getElementById('wd-ut').textContent = formatSize(hostTotals.up_total) + ' total since start';
            createSparkline(document.getElementById('wd-us'), hostUpHistory, 'var(--upload)', 200, 40);
        }
    },
    limiter: {
        title: 'Limiter Status', defaultPos: { x: 8, y: 0, w: 4, h: 4 },
        html: () => `<div class="dash-label">Limiter</div>
            <div class="dash-limiter-status" id="wd-ls" style="color:${limiterState.enabled ? 'var(--brand)' : 'var(--text-muted)'}">
                ${limiterState.enabled ? 'ON' : 'OFF'}</div>
            <div class="dash-iface" id="wd-li">${escHtml(limiterState.interface || '—')}</div>`,
        tick() {
            const s = document.getElementById('wd-ls'); if (!s) return;
            s.textContent = limiterState.enabled ? 'ON' : 'OFF';
            s.style.color = limiterState.enabled ? 'var(--brand)' : 'var(--text-muted)';
            document.getElementById('wd-li').textContent = limiterState.interface || '—';
        }
    },
    lan: {
        title: 'Local Network', defaultPos: { x: 0, y: 4, w: 4, h: 4 },
        html: () => `<div class="dash-label">Local Network</div>
            <div class="dash-zone-rates">
                <div class="dash-zone-row">
                    <span class="dash-zone-arrow download-text">↓</span>
                    <span class="dash-zone-val download-text" id="wd-lnd">${formatRate(hostTotals.lan_down_bps || 0)}</span>
                    <svg class="dash-spark dash-zone-spark" id="wd-lds"></svg>
                </div>
                <div class="dash-zone-row">
                    <span class="dash-zone-arrow upload-text">↑</span>
                    <span class="dash-zone-val upload-text" id="wd-lnu">${formatRate(hostTotals.lan_up_bps || 0)}</span>
                    <svg class="dash-spark dash-zone-spark" id="wd-lus"></svg>
                </div>
            </div>`,
        tick() {
            const v = document.getElementById('wd-lnd'); if (!v) return;
            v.textContent = formatRate(hostTotals.lan_down_bps || 0);
            document.getElementById('wd-lnu').textContent = formatRate(hostTotals.lan_up_bps || 0);
            createSparkline(document.getElementById('wd-lds'), lanDownHistory, 'var(--download)', 120, 22);
            createSparkline(document.getElementById('wd-lus'), lanUpHistory, 'var(--upload)', 120, 22);
        }
    },
    internet: {
        title: 'Internet', defaultPos: { x: 4, y: 4, w: 4, h: 4 },
        html: () => `<div class="dash-label">Internet</div>
            <div class="dash-zone-rates">
                <div class="dash-zone-row">
                    <span class="dash-zone-arrow download-text">↓</span>
                    <span class="dash-zone-val download-text" id="wd-ind">${formatRate(hostTotals.inet_down_bps || 0)}</span>
                    <svg class="dash-spark dash-zone-spark" id="wd-ids"></svg>
                </div>
                <div class="dash-zone-row">
                    <span class="dash-zone-arrow upload-text">↑</span>
                    <span class="dash-zone-val upload-text" id="wd-inu">${formatRate(hostTotals.inet_up_bps || 0)}</span>
                    <svg class="dash-spark dash-zone-spark" id="wd-ius"></svg>
                </div>
            </div>`,
        tick() {
            const v = document.getElementById('wd-ind'); if (!v) return;
            v.textContent = formatRate(hostTotals.inet_down_bps || 0);
            document.getElementById('wd-inu').textContent = formatRate(hostTotals.inet_up_bps || 0);
            createSparkline(document.getElementById('wd-ids'), inetDownHistory, 'var(--download)', 120, 22);
            createSparkline(document.getElementById('wd-ius'), inetUpHistory, 'var(--upload)', 120, 22);
        }
    },
    topapps: {
        title: 'Top Applications', defaultPos: { x: 8, y: 4, w: 4, h: 4 },
        html: () => topAppsHtml(),
        tick() {
            const el = document.getElementById('wgt-topapps'); if (!el) return;
            el.innerHTML = topAppsHtml();
        }
    },
};

const DEFAULT_LAYOUT = [
    { id: 'download', x: 0, y: 0, w: 4, h: 4 },
    { id: 'upload',   x: 4, y: 0, w: 4, h: 4 },
    { id: 'limiter',  x: 8, y: 0, w: 4, h: 4 },
    { id: 'lan',      x: 0, y: 4, w: 4, h: 4 },
    { id: 'internet', x: 4, y: 4, w: 4, h: 4 },
    { id: 'topapps',  x: 8, y: 4, w: 4, h: 4 },
];

function setupDashboard() {
    const container = document.getElementById('dashboard-view');
    container.innerHTML = `
        <div id="dash-pinned-zone"></div>
        <div class="dash-edit-bar" id="dash-edit-bar">
            <div style="position:relative;" id="dash-add-wrap">
                <button class="dash-ctrl-btn" id="dash-add-btn">+ Add Widget</button>
                <div class="dash-add-menu" id="dash-add-menu" style="display:none;"></div>
            </div>
            <button class="dash-ctrl-btn dash-ctrl-primary" id="dash-save-btn">Save Layout</button>
            <button class="dash-ctrl-btn" id="dash-cancel-btn">Cancel</button>
            <button class="dash-ctrl-btn" id="dash-edit-toggle">Edit Dashboard</button>
        </div>
        <div class="grid-stack" id="dash-grid"></div>
    `;

    dashGrid = GridStack.init({
        column: 12,
        cellHeight: 50,
        margin: 8,
        float: false,
        staticGrid: true,
        draggable: { handle: '.gs-drag-handle' },
        resizable: { handles: 'se,s,e' },
    }, '#dash-grid');

    const saved = localStorage.getItem('curb-dash-layout');
    const layout = saved ? JSON.parse(saved) : DEFAULT_LAYOUT;
    dashActiveWidgets = [];
    for (const item of layout) {
        if (!WIDGET_DEFS[item.id]) continue;
        addDashWidget(item);
        dashActiveWidgets.push(item.id);
    }

    setDashEditMode(false);

    document.getElementById('dash-edit-toggle').onclick = () => setDashEditMode(true);
    document.getElementById('dash-save-btn').onclick = saveDashLayout;
    document.getElementById('dash-cancel-btn').onclick = cancelDashEdit;
    document.getElementById('dash-add-btn').onclick = (e) => {
        e.stopPropagation();
        const m = document.getElementById('dash-add-menu');
        const open = m.style.display !== 'none';
        m.style.display = open ? 'none' : 'block';
        if (!open) buildAddMenu();
    };
    document.getElementById('dash-grid').addEventListener('click', (e) => {
        const btn = e.target.closest('.gs-remove-btn');
        if (!btn) return;
        const id = btn.dataset.wid;
        const item = btn.closest('.grid-stack-item');
        dashGrid.removeWidget(item);
        dashActiveWidgets = dashActiveWidgets.filter(w => w !== id);
    });
    document.addEventListener('click', (e) => {
        if (!e.target.closest('#dash-add-wrap')) {
            const m = document.getElementById('dash-add-menu');
            if (m) m.style.display = 'none';
        }
    });

    initPinnedBars();
    tickDashboard();
}

function addDashWidget(item) {
    const def = WIDGET_DEFS[item.id];
    if (!def) return;
    dashGrid.addWidget({
        id: item.id,
        x: item.x, y: item.y, w: item.w, h: item.h,
        content: `<div class="gs-drag-handle"></div>
            <button class="gs-remove-btn" data-wid="${item.id}" title="Remove widget">✕</button>
            <div class="gs-widget-inner" id="wgt-${item.id}">${def.html()}</div>`,
    });
}

function tickDashboard() {
    for (const [id, def] of Object.entries(WIDGET_DEFS)) {
        if (document.getElementById(`wgt-${id}`)) def.tick();
    }
}

function setDashEditMode(on) {
    dashEditing = on;
    const container = document.getElementById('dashboard-view');
    if (!container) return;
    container.classList.toggle('dash-editing', on);
    if (dashGrid) dashGrid.setStatic(!on);
    const tog = document.getElementById('dash-edit-toggle');
    const sav = document.getElementById('dash-save-btn');
    const can = document.getElementById('dash-cancel-btn');
    const add = document.getElementById('dash-add-wrap');
    if (tog) tog.style.display = on ? 'none' : '';
    if (sav) sav.style.display = on ? '' : 'none';
    if (can) can.style.display = on ? '' : 'none';
    if (add) add.style.display = on ? '' : 'none';
}

function saveDashLayout() {
    if (!dashGrid) return;
    const items = dashGrid.save(false, false);
    const layout = items.map(n => ({ id: n.id, x: n.x, y: n.y, w: n.w, h: n.h })).filter(n => n.id);
    localStorage.setItem('curb-dash-layout', JSON.stringify(layout));
    setDashEditMode(false);
}

function cancelDashEdit() {
    if (dashGrid) { dashGrid.destroy(false); dashGrid = null; }
    setupDashboard();
}

function buildAddMenu() {
    const menu = document.getElementById('dash-add-menu');
    if (!menu) return;
    menu.innerHTML = '';
    const active = new Set(dashActiveWidgets);
    let count = 0;
    for (const [id, def] of Object.entries(WIDGET_DEFS)) {
        if (active.has(id)) continue;
        const btn = document.createElement('button');
        btn.className = 'dash-add-item';
        btn.textContent = def.title;
        btn.onclick = (e) => {
            e.stopPropagation();
            const pos = def.defaultPos || { x: 0, y: 99, w: 4, h: 4 };
            addDashWidget({ id, ...pos });
            dashActiveWidgets.push(id);
            menu.style.display = 'none';
            tickDashboard();
        };
        menu.appendChild(btn);
        count++;
    }
    if (count === 0) {
        menu.innerHTML = '<div class="dash-add-item" style="color:var(--text-muted);cursor:default;">All widgets added</div>';
    }
}

function renderDashboard() {
    if (!dashGrid) {
        setupDashboard();
    } else {
        tickDashboard();
        // If pinned set changed (e.g. pinned from another tab), rebuild bars.
        // Otherwise tick in-place to avoid flicker.
        const zone = document.getElementById('dash-pinned-zone');
        const barCount = zone ? zone.querySelectorAll('.pab').length : 0;
        if (barCount !== pinnedApps.length) {
            initPinnedBars();
        } else {
            tickPinnedBars();
        }
    }
}

function initPinnedBars() {
    const zone = document.getElementById('dash-pinned-zone');
    if (!zone) return;
    zone.innerHTML = '';

    const pinned = pinnedApps.map(exe => apps.find(a => a.exe === exe)).filter(Boolean);
    if (pinned.length === 0) return;

    const section = document.createElement('div');
    section.className = 'pinned-section';

    for (const app of pinned) {
        const limit   = appLimits.find(l => l.exe === app.exe);
        const downLit = limit && limit.down_bps != null;
        const upLit   = limit && limit.up_bps   != null;
        const color   = appColor(app.exe);
        const exeId   = app.exe.replace(/\//g, '_');

        const bar = document.createElement('div');
        bar.className = 'pab';
        bar.style.setProperty('--pab-color', color);
        bar.innerHTML = `
            <div class="pab-icon" style="background:${color}22;color:${color}">${escHtml(app.name[0].toUpperCase())}</div>
            <div class="pab-meta">
                <span class="pab-name">${escHtml(app.name)}</span>
                <span class="pab-path">${escHtml(app.exe)}</span>
            </div>
            <div class="pab-stat download-text">
                <span class="pab-arrow">↓</span>
                <span class="pab-rate" id="prate-d-${exeId}">${formatRate(app.down_bps)}</span>
                <svg class="pab-spark" id="psp-d-${exeId}"></svg>
            </div>
            <div class="pab-stat upload-text">
                <span class="pab-arrow">↑</span>
                <span class="pab-rate" id="prate-u-${exeId}">${formatRate(app.up_bps)}</span>
                <svg class="pab-spark" id="psp-u-${exeId}"></svg>
            </div>
            <div class="pab-actions" id="pact-${exeId}"></div>
            <button class="pab-unpin" title="Unpin">✕</button>
        `;

        bar.querySelector('.pab-unpin').onclick = () => togglePin(app.exe);

        const actions = bar.querySelector('.pab-actions');
        const pDown = makeLimBtn('down', downLit);
        const pUp   = makeLimBtn('up',   upLit);
        pDown.onclick = (e) => {
            e.stopPropagation();
            showLimitPopup(pDown, `${app.exe}:down`, 'down', limit ? limit.down_bps : null,
                (v) => setAppLimit(app.exe, 'down', v));
        };
        pUp.onclick = (e) => {
            e.stopPropagation();
            showLimitPopup(pUp, `${app.exe}:up`, 'up', limit ? limit.up_bps : null,
                (v) => setAppLimit(app.exe, 'up', v));
        };
        actions.append(pDown, pUp);
        section.appendChild(bar);
    }

    zone.appendChild(section);

    // Draw sparklines after insertion so the elements are in the DOM
    for (const app of pinned) {
        const exeId = app.exe.replace(/\//g, '_');
        createSparkline(document.getElementById(`psp-d-${exeId}`), app.down_spark, 'var(--download)', 60, 22);
        createSparkline(document.getElementById(`psp-u-${exeId}`), app.up_spark,   'var(--upload)',   60, 22);
    }
}

function tickPinnedBars() {
    for (const exe of pinnedApps) {
        const app = apps.find(a => a.exe === exe);
        if (!app) continue;
        const exeId = exe.replace(/\//g, '_');
        const dEl = document.getElementById(`prate-d-${exeId}`);
        if (!dEl) continue;
        dEl.textContent = formatRate(app.down_bps);
        document.getElementById(`prate-u-${exeId}`).textContent = formatRate(app.up_bps);
        createSparkline(document.getElementById(`psp-d-${exeId}`), app.down_spark, 'var(--download)', 60, 22);
        createSparkline(document.getElementById(`psp-u-${exeId}`), app.up_spark,   'var(--upload)',   60, 22);
    }
}

function renderTopBar() {
    const statusText = document.getElementById('limiter-status-text');
    statusText.textContent = limiterState.enabled ? "ON" : "OFF";
    statusText.className = "switch-status" + (limiterState.enabled ? "" : " off");

    document.getElementById('global-down').textContent = formatRate(hostTotals.down_bps);
    document.getElementById('global-up').textContent = formatRate(hostTotals.up_bps);
    
    createSparkline(document.getElementById('global-down-spark'), hostDownHistory, "var(--download)", 60, 20);
    createSparkline(document.getElementById('global-up-spark'), hostUpHistory, "var(--upload)", 60, 20);

    document.getElementById('interface-name').textContent = limiterState.interface || "";
}

function toggleNode(id) {
    if (expandedNodes.has(id)) expandedNodes.delete(id);
    else expandedNodes.add(id);
    renderTable();
}

function classifyApp(app) {
    const lanTotal = (app.lan_down_bps || 0) + (app.lan_up_bps || 0);
    const inetTotal = (app.inet_down_bps || 0) + (app.inet_up_bps || 0);
    if (lanTotal === 0 && inetTotal === 0) {
        // No live traffic: use the limit scope as a hint, default to internet.
        const limit = appLimits.find(l => l.exe === app.exe);
        return (limit && limit.scope.toLowerCase() === 'lan') ? 'lan' : 'internet';
    }
    return lanTotal >= inetTotal ? 'lan' : 'internet';
}

function renderTable() {
    const tbody = document.getElementById('apps-body');
    tbody.innerHTML = "";

    const filteredApps = apps.filter(app => {
        if (!app.exe) return false;
        if (!searchQuery) return true;
        return app.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
               app.exe.toLowerCase().includes(searchQuery.toLowerCase());
    });

    const lanApps  = filteredApps.filter(a => classifyApp(a) === 'lan');
    const inetApps = filteredApps.filter(a => classifyApp(a) === 'internet');

    renderHostRoot(tbody);
    if (expandedNodes.has('host')) {
        renderZoneRow(tbody, 'lan', 'Local Network',
            limiterState.host.lan.down_bps, limiterState.host.lan.up_bps, 1,
            hostTotals.lan_down_bps, hostTotals.lan_up_bps);
        if (expandedNodes.has('lan')) {
            lanApps.forEach(app => renderAppRow(tbody, app, 2));
        }

        renderZoneRow(tbody, 'internet', 'Internet',
            limiterState.host.internet.down_bps, limiterState.host.internet.up_bps, 1,
            hostTotals.inet_down_bps, hostTotals.inet_up_bps);
        if (expandedNodes.has('internet')) {
            inetApps.forEach(app => renderAppRow(tbody, app, 2));
        }
    }
}

// The host root row: disclosure toggle + total host rate + total limit spinners.
function renderHostRoot(tbody) {
    const isExpanded = expandedNodes.has('host');
    const tr = document.createElement('tr');
    tr.className = 'tree-row tree-root';
    tr.innerHTML = `
        <td>
            <div class="tree-indent" style="--level: 0">
                <div class="tree-toggle">${isExpanded ? '▼' : '▶'}</div>
                <span style="font-weight: 700;">This computer</span>
            </div>
        </td>
        <td><div class="rule-cell"></div></td>
        <td><span class="download-text">${formatRate(hostTotals.down_bps)}</span></td>
        <td><span class="upload-text">${formatRate(hostTotals.up_bps)}</span></td>
        <td></td>
        <td></td>
    `;
    const toggle = () => toggleNode('host');
    tr.querySelector('.tree-toggle').onclick = (e) => { e.stopPropagation(); toggle(); };
    tr.querySelector('.tree-indent > span').onclick = (e) => { e.stopPropagation(); toggle(); };

    const ruleCell = tr.querySelector('.rule-cell');
    const hDown = makeLimBtn('down', limiterState.host.down_bps != null);
    const hUp   = makeLimBtn('up',   limiterState.host.up_bps   != null);
    hDown.onclick = (e) => { e.stopPropagation(); showLimitPopup(hDown, 'host:down', 'down', limiterState.host.down_bps, (v) => setHostLimit('host', 'down', v)); };
    hUp.onclick   = (e) => { e.stopPropagation(); showLimitPopup(hUp,   'host:up',   'up',   limiterState.host.up_bps,   (v) => setHostLimit('host', 'up',   v)); };
    ruleCell.append(hDown, hUp);
    tbody.appendChild(tr);
}

function renderTreeGroup(tbody, id, label, level) {
    const tr = document.createElement('tr');
    tr.className = 'tree-group-header';
    const isExpanded = expandedNodes.has(id);
    
    tr.innerHTML = `
        <td colspan="8">
            <div class="tree-indent" style="--level: ${level}">
                <div class="tree-toggle">${isExpanded ? '▼' : '▶'}</div>
                <span>${label}</span>
            </div>
        </td>
    `;
    tr.onclick = () => {
        if (isExpanded) expandedNodes.delete(id);
        else expandedNodes.add(id);
        renderTable();
    };
    tbody.appendChild(tr);
}

function renderZoneRow(tbody, id, label, downLim, upLim, level, downRate, upRate) {
    const isExpanded = expandedNodes.has(id);
    const tr = document.createElement('tr');
    tr.className = 'tree-row';

    tr.innerHTML = `
        <td>
            <div class="tree-indent" style="--level: ${level}">
                <div class="tree-guide" style="--level: ${level}"></div>
                <div class="tree-toggle">${isExpanded ? '▼' : '▶'}</div>
                <span style="font-weight: 600;">${label}</span>
            </div>
        </td>
        <td><div class="rule-cell"></div></td>
        <td><span class="download-text">${formatRate(downRate || 0)}</span></td>
        <td><span class="upload-text">${formatRate(upRate || 0)}</span></td>
        <td></td>
        <td></td>
    `;

    tr.querySelector('.tree-toggle').onclick = (e) => { e.stopPropagation(); toggleNode(id); };
    tr.querySelector('.tree-indent > span').onclick = (e) => { e.stopPropagation(); toggleNode(id); };

    const zCell = tr.querySelector('.rule-cell');
    const zDown = makeLimBtn('down', downLim != null);
    const zUp   = makeLimBtn('up',   upLim   != null);
    zDown.onclick = (e) => { e.stopPropagation(); showLimitPopup(zDown, `${id}:down`, 'down', downLim, (v) => setHostLimit(id, 'down', v)); };
    zUp.onclick   = (e) => { e.stopPropagation(); showLimitPopup(zUp,   `${id}:up`,   'up',   upLim,   (v) => setHostLimit(id, 'up',   v)); };
    zCell.append(zDown, zUp);
    tbody.appendChild(tr);
}

function renderAppRow(tbody, app, level) {
    const limit = appLimits.find(l => l.exe === app.exe);
    const quota = quotas.find(q => q.exe === app.exe);

    let statusLabel = "Watching";
    let statusClass = "pill-muted";
    if (quota && quota.exceeded) {
        statusLabel = "Throttled";
        statusClass = "pill-rose";
    } else if (limit) {
        statusLabel = "Limited";
        statusClass = "pill-emerald";
    }

    const scope        = limit ? limit.scope : "Both";
    const quotaPercent = quota ? Math.min((quota.used_bytes / quota.budget_bytes) * 100, 100) : 0;
    const quotaExceeded = quota ? quota.exceeded : false;
    const downLit      = limit && limit.down_bps != null;
    const upLit        = limit && limit.up_bps   != null;

    const tr = document.createElement('tr');
    tr.className = 'tree-row';
    if (app.exe === selectedExe) tr.classList.add('selected');

    tr.onclick = () => {
        selectedExe = app.exe;
        renderDetailStrip();
        document.querySelectorAll('#apps-body tr').forEach(r => r.classList.remove('selected'));
        tr.classList.add('selected');
    };

    const exeId = app.exe.replace(/\//g, '_');
    tr.innerHTML = `
        <td>
            <div class="tree-indent" style="--level: ${level}">
                <div class="tree-guide" style="--level: ${level}"></div>
                <div class="app-cell">
                    <div class="app-icon" style="border-top: 2px solid ${appColor(app.exe)}">${app.name[0].toUpperCase()}</div>
                    <div class="app-info">
                        <span class="app-name">${escHtml(app.name)}</span>
                        <span class="app-path">${escHtml(app.exe)}</span>
                    </div>
                    <button class="pin-btn ${pinnedApps.includes(app.exe) ? 'pin-btn-active' : ''}" title="${pinnedApps.includes(app.exe) ? 'Unpin from Dashboard' : 'Pin to Dashboard'}"><svg viewBox="0 0 24 24" width="11" height="11" fill="currentColor" stroke="none"><path d="M16 9V4h1c.55 0 1-.45 1-1s-.45-1-1-1H7c-.55 0-1 .45-1 1s.45 1 1 1h1v5c0 1.66-1.34 3-3 3v2h5.97v7l1 1 1-1v-7H19v-2c-1.66 0-3-1.34-3-3z"/></svg></button>
                </div>
            </div>
        </td>
        <td><div class="rule-cell">
            <span class="pill ${statusClass}">${statusLabel}</span>
        </div></td>
        <td>
            <div class="rate-cell">
                <span class="download-text">${formatRate(app.down_bps)}</span>
                <svg class="inline-spark" id="spark-down-${exeId}"></svg>
            </div>
        </td>
        <td>
            <div class="rate-cell">
                <span class="upload-text">${formatRate(app.up_bps)}</span>
                <svg class="inline-spark" id="spark-up-${exeId}"></svg>
            </div>
        </td>
        <td>
            <div class="usage-cell">
                <span class="usage-value">${formatUsage(app.today_down, app.today_up)}</span>
                ${quota ? `<div class="quota-container"><div class="quota-bar-bg"><div class="quota-bar-fill ${quotaExceeded ? 'danger' : ''}" style="width: ${quotaPercent}%"></div></div><span class="quota-label">${formatSize(quota.used_bytes)} / ${formatSize(quota.budget_bytes)}</span></div>` : ''}
            </div>
        </td>
        <td>
            <span class="usage-value muted">${formatUsage(app.month_down, app.month_up)}</span>
        </td>
    `;

    // Append ▼▲ buttons to the rule cell
    const ruleCell = tr.querySelector('.rule-cell');
    const aDown = makeLimBtn('down', downLit);
    const aUp   = makeLimBtn('up',   upLit);
    aDown.onclick = (e) => {
        e.stopPropagation();
        showLimitPopup(aDown, `${app.exe}:down`, 'down', limit ? limit.down_bps : null,
            (v) => setAppLimit(app.exe, 'down', v));
    };
    aUp.onclick = (e) => {
        e.stopPropagation();
        showLimitPopup(aUp, `${app.exe}:up`, 'up', limit ? limit.up_bps : null,
            (v) => setAppLimit(app.exe, 'up', v));
    };
    ruleCell.append(aDown, aUp);

    const quotaEl = tr.querySelector('.quota-container');
    if (quotaEl) quotaEl.onclick = (e) => { e.stopPropagation(); promptQuota(app.exe); };
    tr.querySelector('.pin-btn').onclick = (e) => { e.stopPropagation(); togglePin(app.exe); };

    tbody.appendChild(tr);

    createSparkline(document.getElementById(`spark-down-${exeId}`), app.down_spark, "var(--download)");
    createSparkline(document.getElementById(`spark-up-${exeId}`), app.up_spark, "var(--upload)");
}

function renderDetailStrip() {
    const app = apps.find(a => a.exe === selectedExe);
    const detailInfo = document.getElementById('detail-app-info');
    const pillsContainer = detailInfo.querySelector('div:last-child');

    if (!app) {
        // No app selected — show host-wide totals and history.
        detailInfo.querySelector('.detail-title').textContent = "This computer";
        detailInfo.querySelector('.app-path').textContent = limiterState.interface || "—";
        pillsContainer.innerHTML = `<div class="pill pill-muted">${apps.length} app${apps.length !== 1 ? 's' : ''}</div>`;
        document.getElementById('detail-down-val').textContent = formatRate(hostTotals.down_bps);
        document.getElementById('detail-up-val').textContent = formatRate(hostTotals.up_bps);
        createAreaGraph(document.getElementById('svg-detail-down'), hostDownHistory, "var(--download)");
        createAreaGraph(document.getElementById('svg-detail-up'),   hostUpHistory,   "var(--upload)");
        return;
    }

    const limit = appLimits.find(l => l.exe === app.exe);
    const quota = quotas.find(q => q.exe === app.exe);

    detailInfo.querySelector('.detail-title').textContent = app.name;
    detailInfo.querySelector('.app-path').textContent = app.exe;

    let status = "Watching";
    let statusClass = "pill-muted";
    if (quota && quota.exceeded) { status = "Throttled"; statusClass = "pill-rose"; }
    else if (limit)               { status = "Limited";   statusClass = "pill-emerald"; }

    pillsContainer.innerHTML = `<div class="pill ${statusClass}">${status}</div>`;
    if (limit) {
        pillsContainer.innerHTML += `<div class="pill pill-${limit.scope.toLowerCase()}">${limit.scope}</div>`;
    }

    document.getElementById('detail-down-val').textContent = formatRate(app.down_bps);
    document.getElementById('detail-up-val').textContent = formatRate(app.up_bps);
    createAreaGraph(document.getElementById('svg-detail-down'), app.down_spark, "var(--download)");
    createAreaGraph(document.getElementById('svg-detail-up'),   app.up_spark,   "var(--upload)");
}

// --- Rules Tab ---

function renderRules() {
    const view = document.getElementById('rules-view');
    view.innerHTML = `
        <div class="tab-toolbar">
            <button class="tab-add-btn" id="rules-add-btn">+ Add Rule</button>
        </div>
    `;
    document.getElementById('rules-add-btn').onclick = () => showRuleModal(null);

    if (appLimits.length === 0) {
        view.insertAdjacentHTML('beforeend',
            '<div class="tab-empty">No rules configured. Set limits from the Applications tab or click Add.</div>');
        return;
    }

    const wrap = document.createElement('div');
    wrap.className = 'table-container';
    const table = document.createElement('table');
    table.innerHTML = `<thead><tr>
        <th>Application</th>
        <th style="width:90px;">Scope</th>
        <th style="width:110px;">↓ Download</th>
        <th style="width:110px;">↑ Upload</th>
        <th style="width:72px;">Actions</th>
    </tr></thead>`;
    const tbody = document.createElement('tbody');

    for (const r of appLimits) {
        const tr = document.createElement('tr');

        const appTd = document.createElement('td');
        const appCell = document.createElement('div');
        appCell.className = 'app-cell';
        const icon = document.createElement('div');
        icon.className = 'app-icon';
        icon.textContent = r.name[0].toUpperCase();
        const info = document.createElement('div');
        info.className = 'app-info';
        const nm = document.createElement('span');
        nm.className = 'app-name';
        nm.textContent = r.name;
        const pth = document.createElement('span');
        pth.className = 'app-path';
        pth.textContent = r.exe;
        info.append(nm, pth);
        appCell.append(icon, info);
        appTd.appendChild(appCell);

        const scopeTd = document.createElement('td');
        const pill = document.createElement('span');
        pill.className = `pill pill-${r.scope.toLowerCase()}`;
        pill.textContent = r.scope;
        scopeTd.appendChild(pill);

        const downTd = document.createElement('td');
        downTd.className = 'mono download-text';
        downTd.textContent = formatRate(r.down_bps);

        const upTd = document.createElement('td');
        upTd.className = 'mono upload-text';
        upTd.textContent = formatRate(r.up_bps);

        const actTd = document.createElement('td');
        const btns = document.createElement('div');
        btns.className = 'action-btns';
        const editBtn = document.createElement('button');
        editBtn.className = 'action-btn';
        editBtn.title = 'Edit rule';
        editBtn.textContent = '✏';
        editBtn.onclick = () => showRuleModal(r);
        const delBtn = document.createElement('button');
        delBtn.className = 'action-btn action-btn-del';
        delBtn.title = 'Remove rule';
        delBtn.textContent = '✕';
        delBtn.onclick = () => deleteRule(r.exe);
        btns.append(editBtn, delBtn);
        actTd.appendChild(btns);

        tr.append(appTd, scopeTd, downTd, upTd, actTd);
        tbody.appendChild(tr);
    }

    table.appendChild(tbody);
    wrap.appendChild(table);
    view.appendChild(wrap);
}

function showRuleModal(existing) {
    const isNew = !existing;
    const fields = [
        ...(isNew ? [{ id: 'exe', label: 'Executable path', placeholder: '/usr/bin/firefox', value: '' }] : []),
        { id: 'down', label: '↓ Download limit (e.g. 5 MB/s, or blank for unlimited)', placeholder: 'unlimited', value: existing ? formatRate(existing.down_bps) : '' },
        { id: 'up',   label: '↑ Upload limit',   placeholder: 'unlimited', value: existing ? formatRate(existing.up_bps)   : '' },
        {
            id: 'scope', label: 'Scope', type: 'select', value: existing ? existing.scope.toLowerCase() : 'both',
            options: [{ value: 'both', label: 'Both (LAN + Internet)' }, { value: 'lan', label: 'LAN only' }, { value: 'internet', label: 'Internet only' }],
        },
    ];
    showModal(isNew ? 'Add Rule' : `Edit Rule — ${existing.name}`, fields, async (vals) => {
        const exe   = isNew ? vals.exe.trim() : existing.exe;
        const downBps = vals.down.trim() ? parseRate(vals.down) : null;
        const upBps   = vals.up.trim()   ? parseRate(vals.up)   : null;
        const scope   = vals.scope;
        if (!exe) return;
        try {
            await invoke('set_app_limit', { exe, downBps, upBps, scope });
            poll();
        } catch (err) { alert(err); }
    });
}

async function deleteRule(exe) {
    try {
        await invoke('clear_app_limit', { exe });
        poll();
    } catch (err) { alert(err); }
}

// --- Quotas Tab ---

function renderQuotas() {
    const view = document.getElementById('quotas-view');
    view.innerHTML = `
        <div class="tab-toolbar">
            <button class="tab-add-btn" id="quotas-add-btn">+ Add Quota</button>
        </div>
    `;
    document.getElementById('quotas-add-btn').onclick = () => showQuotaModal(null);

    if (quotas.length === 0) {
        view.insertAdjacentHTML('beforeend',
            '<div class="tab-empty">No quotas configured. Click Add to set a data budget for an application.</div>');
        return;
    }

    const wrap = document.createElement('div');
    wrap.className = 'table-container';
    const table = document.createElement('table');
    table.innerHTML = `<thead><tr>
        <th>Application</th>
        <th style="width:90px;">Budget</th>
        <th style="width:150px;">Used</th>
        <th style="width:80px;">Period</th>
        <th style="width:60px;">Dir</th>
        <th style="width:90px;">Resets in</th>
        <th style="width:80px;">Status</th>
        <th style="width:72px;">Actions</th>
    </tr></thead>`;
    const tbody = document.createElement('tbody');

    for (const q of quotas) {
        const pct = Math.min((q.used_bytes / q.budget_bytes) * 100, 100);
        const barColor = q.exceeded ? 'var(--danger)' : pct > 80 ? '#fb923c' : 'var(--brand)';

        const tr = document.createElement('tr');

        // App cell
        const appTd = document.createElement('td');
        const appCell = document.createElement('div');
        appCell.className = 'app-cell';
        const icon = document.createElement('div');
        icon.className = 'app-icon';
        icon.textContent = q.name[0].toUpperCase();
        const info = document.createElement('div');
        info.className = 'app-info';
        const nm = document.createElement('span');
        nm.className = 'app-name';
        nm.textContent = q.name;
        const pth = document.createElement('span');
        pth.className = 'app-path';
        pth.textContent = q.exe;
        info.append(nm, pth);
        appCell.append(icon, info);
        appTd.appendChild(appCell);

        const budgetTd = document.createElement('td');
        budgetTd.className = 'mono';
        budgetTd.textContent = formatSize(q.budget_bytes);

        const usedTd = document.createElement('td');
        usedTd.innerHTML = `
            <div style="display:flex;flex-direction:column;gap:3px;">
                <div style="height:4px;background:var(--border);border-radius:2px;overflow:hidden;">
                    <div style="height:100%;width:${pct.toFixed(1)}%;background:${barColor};border-radius:2px;"></div>
                </div>
                <span class="mono" style="font-size:10px;color:var(--text-muted);">
                    ${formatSize(q.used_bytes)} / ${formatSize(q.budget_bytes)}
                </span>
            </div>`;

        const periodTd = document.createElement('td');
        periodTd.textContent = q.period;

        const dirTd = document.createElement('td');
        dirTd.textContent = q.direction === 'Down' ? '↓' : q.direction === 'Up' ? '↑' : '↓↑';

        const resetTd = document.createElement('td');
        resetTd.className = 'mono';
        resetTd.style.color = 'var(--text-muted)';
        resetTd.textContent = formatResetTime(q.resets_in_secs);

        const statusTd = document.createElement('td');
        const pill = document.createElement('span');
        pill.className = `pill ${q.exceeded ? 'pill-rose' : 'pill-emerald'}`;
        pill.textContent = q.exceeded ? 'EXCEEDED' : 'OK';
        statusTd.appendChild(pill);

        const actTd = document.createElement('td');
        const btns = document.createElement('div');
        btns.className = 'action-btns';
        const editBtn = document.createElement('button');
        editBtn.className = 'action-btn';
        editBtn.title = 'Edit quota';
        editBtn.textContent = '✏';
        editBtn.onclick = () => showQuotaModal(q);
        const delBtn = document.createElement('button');
        delBtn.className = 'action-btn action-btn-del';
        delBtn.title = 'Remove quota';
        delBtn.textContent = '✕';
        delBtn.onclick = () => deleteQuota(q.exe);
        btns.append(editBtn, delBtn);
        actTd.appendChild(btns);

        tr.append(appTd, budgetTd, usedTd, periodTd, dirTd, resetTd, statusTd, actTd);
        tbody.appendChild(tr);
    }

    table.appendChild(tbody);
    wrap.appendChild(table);
    view.appendChild(wrap);
}

function showQuotaModal(existing) {
    const isNew = !existing;
    const fields = [
        ...(isNew ? [{ id: 'exe', label: 'Executable path', placeholder: '/usr/bin/firefox', value: '' }] : []),
        { id: 'budget', label: 'Budget (e.g. 10 GB, 500 MB)', placeholder: '10 GB', value: existing ? formatSize(existing.budget_bytes) : '' },
        {
            id: 'period', label: 'Reset period', type: 'select',
            value: existing ? existing.period.toLowerCase() : 'daily',
            options: [
                { value: 'hourly', label: 'Hourly' }, { value: 'daily', label: 'Daily' },
                { value: 'weekly', label: 'Weekly' }, { value: 'monthly', label: 'Monthly' },
                { value: 'total', label: 'Total (never resets)' },
            ],
        },
        {
            id: 'direction', label: 'Count', type: 'select',
            value: existing ? existing.direction.toLowerCase() : 'both',
            options: [
                { value: 'both', label: 'Download + Upload' },
                { value: 'down', label: 'Download only' },
                { value: 'up',   label: 'Upload only' },
            ],
        },
    ];
    showModal(isNew ? 'Add Quota' : `Edit Quota — ${existing.name}`, fields, async (vals) => {
        const exe = isNew ? vals.exe.trim() : existing.exe;
        const budgetBytes = parseSize(vals.budget);
        if (!exe) { alert('Executable path is required.'); return; }
        if (!budgetBytes) { alert('Invalid budget. Use e.g. "10 GB" or "500 MB".'); return; }
        try {
            await invoke('set_quota', { exe, budgetBytes, period: vals.period, direction: vals.direction });
            poll();
        } catch (err) { alert(err); }
    });
}

async function deleteQuota(exe) {
    try {
        await invoke('clear_quota', { exe });
        poll();
    } catch (err) { alert(err); }
}

// --- Connections Tab ---

function renderConnections() {
    const tbody = document.getElementById('conn-tbody');
    if (!tbody) return;
    tbody.innerHTML = '';

    const q = connFilter.toLowerCase();
    const filtered = connections.filter(c =>
        !q || c.name.toLowerCase().includes(q) ||
        c.remote_addr.includes(q) || c.local_addr.includes(q) || c.exe.toLowerCase().includes(q)
    );

    const countEl = document.getElementById('conn-count');
    if (countEl) countEl.textContent = `${filtered.length} connection${filtered.length !== 1 ? 's' : ''}`;

    if (filtered.length === 0) {
        const tr = document.createElement('tr');
        const td = document.createElement('td');
        td.colSpan = 6;
        td.className = 'tab-empty';
        td.textContent = connections.length === 0
            ? 'No active connections found.'
            : 'No connections match the filter.';
        tr.appendChild(td);
        tbody.appendChild(tr);
        return;
    }

    for (const c of filtered) {
        const tr = document.createElement('tr');

        const nameTd = document.createElement('td');
        const appCell = document.createElement('div');
        appCell.className = 'app-cell';
        const icon = document.createElement('div');
        icon.className = 'app-icon';
        icon.style.fontSize = '9px';
        icon.textContent = (c.name[0] || '?').toUpperCase();
        const info = document.createElement('div');
        info.className = 'app-info';
        const nm = document.createElement('span');
        nm.className = 'app-name';
        nm.textContent = c.name;
        info.appendChild(nm);
        appCell.append(icon, info);
        nameTd.appendChild(appCell);

        const pidTd = document.createElement('td');
        pidTd.className = 'mono';
        pidTd.style.color = 'var(--text-muted)';
        pidTd.textContent = c.pid || '—';

        const protoTd = document.createElement('td');
        const protoPill = document.createElement('span');
        protoPill.className = `pill ${c.proto === 'TCP' ? 'pill-emerald' : 'pill-both'}`;
        protoPill.textContent = c.proto;
        protoTd.appendChild(protoPill);

        const localTd = document.createElement('td');
        localTd.className = 'mono addr-cell';
        localTd.textContent = c.local_addr;

        const remoteTd = document.createElement('td');
        remoteTd.className = 'mono addr-cell';
        remoteTd.textContent = c.remote_addr;

        const stateTd = document.createElement('td');
        const stateClass =
            c.state === 'ESTABLISHED' ? 'state-estab' :
            c.state === 'TIME_WAIT'   ? 'state-tw'    :
            c.state === 'LISTEN'      ? 'state-listen' :
            c.state === 'UDP'         ? 'state-udp'   : 'state-other';
        const statePill = document.createElement('span');
        statePill.className = `conn-state ${stateClass}`;
        statePill.textContent = c.state;
        stateTd.appendChild(statePill);

        tr.append(nameTd, pidTd, protoTd, localTd, remoteTd, stateTd);
        tbody.appendChild(tr);
    }
}

// --- Modal System ---

let modalOkHandler = null;

function showModal(title, fields, onOk) {
    modalOkHandler = onOk;
    document.getElementById('modal-title').textContent = title;

    const body = document.getElementById('modal-body');
    body.innerHTML = '';

    for (const f of fields) {
        const row = document.createElement('div');
        row.className = 'modal-field';

        const lbl = document.createElement('label');
        lbl.className = 'modal-label';
        lbl.textContent = f.label;
        lbl.htmlFor = 'mf-' + f.id;
        row.appendChild(lbl);

        if (f.type === 'select') {
            const sel = document.createElement('select');
            sel.id = 'mf-' + f.id;
            sel.className = 'modal-select';
            for (const opt of f.options) {
                const o = document.createElement('option');
                o.value = opt.value;
                o.textContent = opt.label;
                if (opt.value === f.value) o.selected = true;
                sel.appendChild(o);
            }
            row.appendChild(sel);
        } else {
            const inp = document.createElement('input');
            inp.id = 'mf-' + f.id;
            inp.className = 'modal-input';
            inp.type = 'text';
            inp.value = f.value || '';
            inp.placeholder = f.placeholder || '';
            inp.addEventListener('keydown', (e) => { if (e.key === 'Enter') applyModal(); });
            row.appendChild(inp);
        }

        body.appendChild(row);
    }

    document.getElementById('modal-overlay').style.display = 'flex';
    const first = body.querySelector('input, select');
    if (first) { first.focus(); if (first.select) first.select(); }
}

function hideModal() {
    document.getElementById('modal-overlay').style.display = 'none';
    modalOkHandler = null;
}

function applyModal() {
    if (!modalOkHandler) return;
    const body = document.getElementById('modal-body');
    const vals = {};
    body.querySelectorAll('input, select').forEach(el => {
        vals[el.id.replace('mf-', '')] = el.value;
    });
    const fn = modalOkHandler;
    hideModal();
    fn(vals);
}

// --- Mutations ---

async function toggleMasterLimiter() {
    try {
        await invoke('set_limiter_enabled', { enabled: !limiterState.enabled });
        poll();
    } catch (err) { alert(err); }
}

async function setHostLimit(type, dir, val) {
    const h = limiterState.host;
    let params = {
        downBps: h.down_bps,
        upBps: h.up_bps,
        lanDownBps: h.lan.down_bps,
        lanUpBps: h.lan.up_bps,
        inetDownBps: h.internet.down_bps,
        inetUpBps: h.internet.up_bps
    };

    if (type === 'host') {
        if (dir === 'down') params.downBps = val;
        else params.upBps = val;
    } else if (type === 'lan') {
        if (dir === 'down') params.lanDownBps = val;
        else params.lanUpBps = val;
    } else if (type === 'internet') {
        if (dir === 'down') params.inetDownBps = val;
        else params.inetUpBps = val;
    }

    try {
        await invoke('set_host_limit', params);
        poll();
    } catch (err) { alert(err); }
}

async function setAppLimit(exe, dir, val) {
    const limit = appLimits.find(l => l.exe === exe);
    let downBps = limit ? limit.down_bps : null;
    let upBps = limit ? limit.up_bps : null;
    let scope = limit ? limit.scope.toLowerCase() : 'both';

    if (dir === 'down') downBps = val;
    else upBps = val;

    try {
        if (downBps === null && upBps === null && (!quotas.find(q => q.exe === exe))) {
            await invoke('clear_app_limit', { exe });
        } else {
            await invoke('set_app_limit', { exe, downBps, upBps, scope });
        }
        poll();
    } catch (err) { alert(err); }
}

async function cycleScope(exe) {
    const limit = appLimits.find(l => l.exe === exe);
    if (!limit) return;

    const scopes = ['both', 'lan', 'internet'];
    let idx = scopes.indexOf(limit.scope.toLowerCase());
    const newScope = scopes[(idx + 1) % 3];

    try {
        await invoke('set_app_limit', { 
            exe, 
            downBps: limit.down_bps, 
            upBps: limit.up_bps, 
            scope: newScope 
        });
        poll();
    } catch (err) { alert(err); }
}

async function promptQuota(exe) {
    const quota = quotas.find(q => q.exe === exe);
    const input = prompt(`Daily budget for ${exe} (e.g. 10GB, 500MB, or empty to clear):`, 
        quota ? formatSize(quota.budget_bytes) : "");
    
    if (input === null) return;

    if (input.trim() === "") {
        try {
            await invoke('clear_quota', { exe });
            poll();
        } catch (err) { alert(err); }
        return;
    }

    const budgetBytes = parseSize(input);
    if (!budgetBytes) { alert("Invalid size"); return; }

    try {
        await invoke('set_quota', { 
            exe, 
            budgetBytes, 
            period: 'daily', 
            direction: 'both' 
        });
        poll();
    } catch (err) { alert(err); }
}

// --- Limit Popup ---

let popupOpen = false;
let popupApplyFn = null;
// Remembers last typed value per key so enable/disable doesn't lose the value
const lastLimitVal = {};

function showLimitPopup(btn, key, dir, currentBps, applyFn) {
    popupApplyFn = applyFn;
    popupOpen = true;

    const popup   = document.getElementById('limit-popup');
    const chk     = document.getElementById('lp-enabled');
    const inp     = document.getElementById('lp-value');
    const dirLbl  = document.getElementById('lp-dir-label');

    dirLbl.textContent = dir === 'down' ? '↓ Download limit' : '↑ Upload limit';
    dirLbl.style.color = dir === 'down' ? 'var(--download)' : 'var(--upload)';

    const isActive = currentBps != null;
    chk.checked = true;  // always start ready to set/keep a limit
    inp.value   = isActive ? formatRate(currentBps) : (lastLimitVal[key] || '');
    inp.disabled = false;

    chk.onchange = () => {
        inp.disabled = !chk.checked;
        if (chk.checked) { inp.focus(); inp.select(); }
    };

    popup.style.display = 'flex';
    const rect = btn.getBoundingClientRect();
    let top  = rect.bottom + 5;
    let left = rect.left - 85;
    if (left < 6) left = 6;
    if (left + 210 > window.innerWidth) left = window.innerWidth - 216;
    if (top + 110 > window.innerHeight) top = rect.top - 115;
    popup.style.top  = top  + 'px';
    popup.style.left = left + 'px';

    if (isActive) { inp.focus(); inp.select(); }
}

function hideLimitPopup() {
    document.getElementById('limit-popup').style.display = 'none';
    popupOpen    = false;
    popupApplyFn = null;
}

function initLimitPopup() {
    const okBtn = document.getElementById('lp-ok');
    const inp   = document.getElementById('lp-value');
    const chk   = document.getElementById('lp-enabled');

    const apply = async () => {
        if (!popupApplyFn) return;
        const fn      = popupApplyFn;
        const enabled = chk.checked;
        const raw     = inp.value.trim();
        const bps     = (enabled && raw) ? parseRate(raw) : null;
        hideLimitPopup();
        try { await fn(bps); } catch (err) { alert(err); }
        poll();
    };

    okBtn.onclick      = apply;
    inp.onkeydown = (e) => {
        if (e.key === 'Enter')  { e.preventDefault(); apply(); }
        if (e.key === 'Escape') { hideLimitPopup(); poll(); }
    };
    inp.oninput = () => { if (inp.value) lastLimitVal[currentPopupKey()] = inp.value; };

    document.addEventListener('mousedown', (e) => {
        if (popupOpen && !document.getElementById('limit-popup').contains(e.target)) {
            hideLimitPopup(); poll();
        }
    }, true);
}

function currentPopupKey() {
    // returns the key used to remember the last value
    return document.getElementById('lp-dir-label')?.textContent || '';
}

function makeLimBtn(dir, isLit) {
    const btn = document.createElement('button');
    btn.className = `lim-btn ${dir}${isLit ? ' lit' : ''}`;
    btn.title     = dir === 'down' ? 'Set download limit' : 'Set upload limit';
    btn.textContent = dir === 'down' ? '▼' : '▲';
    return btn;
}

// --- Init ---

// Wire the top-bar master switch and search (null-safe so one missing element
// never halts the rest of the script).
const masterToggle = document.getElementById('master-limiter-toggle');
if (masterToggle) masterToggle.onclick = toggleMasterLimiter;
const searchBox = document.getElementById('search-apps');
if (searchBox) searchBox.oninput = (e) => { searchQuery = e.target.value; renderTable(); };

const VIEW_LABEL = { dashboard: 'Dashboard', apps: 'Applications', rules: 'Rules', quotas: 'Quotas', connections: 'Connections', settings: 'Settings' };

// Wire the sidebar tabs using data-view attributes.
document.querySelectorAll('.nav-item[data-view]').forEach((item) => {
    item.onclick = (e) => {
        e.preventDefault();
        currentView = VIEW_LABEL[item.dataset.view] || item.dataset.view;
        document.querySelectorAll('.nav-item').forEach((n) => n.classList.remove('active'));
        item.classList.add('active');
        renderTable();
    };
});

// Init limit popup
initLimitPopup();

// Wire modal OK/Cancel
document.getElementById('modal-ok').onclick = applyModal;
document.getElementById('modal-cancel').onclick = hideModal;
document.getElementById('modal-overlay').addEventListener('mousedown', (e) => {
    if (e.target === document.getElementById('modal-overlay')) hideModal();
});

// Wire connections filter
document.addEventListener('input', (e) => {
    if (e.target.id === 'conn-filter') {
        connFilter = e.target.value;
        renderConnections();
    }
});

// ── Theme system ─────────────────────────────────────────────────────────────

const THEMES = {
    'CURB Dark': {
        '--app-bg': '#0b0e14', '--surface': '#131722', '--elevated': '#1a2030',
        '--border': '#232a39', '--text-primary': '#e6e9ef', '--text-muted': '#8a93a6',
        '--download': '#38bdf8', '--upload': '#fb923c', '--brand': '#34d399',
        '--danger': '#fb7185', '--lan': 'rgba(45,212,191,0.15)', '--lan-text': '#2dd4bf',
        '--internet': 'rgba(129,140,248,0.15)', '--internet-text': '#818cf8',
    },
    'Midnight': {
        '--app-bg': '#080c18', '--surface': '#0d1225', '--elevated': '#121930',
        '--border': '#1a2340', '--text-primary': '#c8d4f0', '--text-muted': '#6272a4',
        '--download': '#58a6ff', '--upload': '#ffa657', '--brand': '#56d364',
        '--danger': '#ff7b72', '--lan': 'rgba(86,211,100,0.15)', '--lan-text': '#56d364',
        '--internet': 'rgba(88,166,255,0.15)', '--internet-text': '#58a6ff',
    },
    'Dracula': {
        '--app-bg': '#1e1e2e', '--surface': '#21222c', '--elevated': '#2d2d3f',
        '--border': '#3a3a52', '--text-primary': '#f8f8f2', '--text-muted': '#8b949e',
        '--download': '#8be9fd', '--upload': '#ffb86c', '--brand': '#50fa7b',
        '--danger': '#ff5555', '--lan': 'rgba(80,250,123,0.15)', '--lan-text': '#50fa7b',
        '--internet': 'rgba(189,147,249,0.15)', '--internet-text': '#bd93f9',
    },
    'Nord': {
        '--app-bg': '#2e3440', '--surface': '#3b4252', '--elevated': '#434c5e',
        '--border': '#4c566a', '--text-primary': '#eceff4', '--text-muted': '#9099aa',
        '--download': '#88c0d0', '--upload': '#d08770', '--brand': '#a3be8c',
        '--danger': '#bf616a', '--lan': 'rgba(163,190,140,0.15)', '--lan-text': '#a3be8c',
        '--internet': 'rgba(136,192,208,0.15)', '--internet-text': '#88c0d0',
    },
    'Solarized Dark': {
        '--app-bg': '#002b36', '--surface': '#073642', '--elevated': '#0d4151',
        '--border': '#135361', '--text-primary': '#eee8d5', '--text-muted': '#839496',
        '--download': '#268bd2', '--upload': '#cb4b16', '--brand': '#2aa198',
        '--danger': '#dc322f', '--lan': 'rgba(42,161,152,0.15)', '--lan-text': '#2aa198',
        '--internet': 'rgba(38,139,210,0.15)', '--internet-text': '#268bd2',
    },
    'Gruvbox': {
        '--app-bg': '#1d2021', '--surface': '#282828', '--elevated': '#3c3836',
        '--border': '#504945', '--text-primary': '#ebdbb2', '--text-muted': '#928374',
        '--download': '#83a598', '--upload': '#fe8019', '--brand': '#b8bb26',
        '--danger': '#fb4934', '--lan': 'rgba(184,187,38,0.15)', '--lan-text': '#b8bb26',
        '--internet': 'rgba(131,165,152,0.15)', '--internet-text': '#83a598',
    },
    'Tokyo Night': {
        '--app-bg': '#1a1b2e', '--surface': '#1f2040', '--elevated': '#252650',
        '--border': '#2e3153', '--text-primary': '#c0caf5', '--text-muted': '#565f89',
        '--download': '#7dcfff', '--upload': '#ff9e64', '--brand': '#9ece6a',
        '--danger': '#f7768e', '--lan': 'rgba(158,206,106,0.15)', '--lan-text': '#9ece6a',
        '--internet': 'rgba(125,207,255,0.15)', '--internet-text': '#7dcfff',
    },
    'Catppuccin': {
        '--app-bg': '#1e1e2e', '--surface': '#181825', '--elevated': '#1e1e2e',
        '--border': '#313244', '--text-primary': '#cdd6f4', '--text-muted': '#6c7086',
        '--download': '#89dceb', '--upload': '#fab387', '--brand': '#a6e3a1',
        '--danger': '#f38ba8', '--lan': 'rgba(166,227,161,0.15)', '--lan-text': '#a6e3a1',
        '--internet': 'rgba(137,220,235,0.15)', '--internet-text': '#89dceb',
    },
    'One Dark': {
        '--app-bg': '#21252b', '--surface': '#282c34', '--elevated': '#2c313c',
        '--border': '#3e4451', '--text-primary': '#abb2bf', '--text-muted': '#5c6370',
        '--download': '#61afef', '--upload': '#d19a66', '--brand': '#98c379',
        '--danger': '#e06c75', '--lan': 'rgba(152,195,121,0.15)', '--lan-text': '#98c379',
        '--internet': 'rgba(97,175,239,0.15)', '--internet-text': '#61afef',
    },
    'Matrix': {
        '--app-bg': '#000000', '--surface': '#0a0f0a', '--elevated': '#0f180f',
        '--border': '#1a2e1a', '--text-primary': '#00ff41', '--text-muted': '#00832b',
        '--download': '#00e676', '--upload': '#69ff47', '--brand': '#00ff41',
        '--danger': '#ff1744', '--lan': 'rgba(0,255,65,0.1)', '--lan-text': '#00ff41',
        '--internet': 'rgba(0,230,118,0.1)', '--internet-text': '#00e676',
    },
    'Sunset': {
        '--app-bg': '#1a0e1e', '--surface': '#261228', '--elevated': '#331834',
        '--border': '#45233a', '--text-primary': '#f2c5d4', '--text-muted': '#9a6b7e',
        '--download': '#f48fb1', '--upload': '#ffb74d', '--brand': '#ce93d8',
        '--danger': '#ff5252', '--lan': 'rgba(206,147,216,0.15)', '--lan-text': '#ce93d8',
        '--internet': 'rgba(244,143,177,0.15)', '--internet-text': '#f48fb1',
    },
    'Ocean': {
        '--app-bg': '#0a1628', '--surface': '#0d1f3c', '--elevated': '#112749',
        '--border': '#1a3460', '--text-primary': '#d0e8f5', '--text-muted': '#6a8caa',
        '--download': '#4dd0e1', '--upload': '#4fc3f7', '--brand': '#26c6da',
        '--danger': '#ef5350', '--lan': 'rgba(38,198,218,0.15)', '--lan-text': '#26c6da',
        '--internet': 'rgba(77,208,225,0.15)', '--internet-text': '#4dd0e1',
    },
    'Light': {
        '--app-bg': '#f5f7fa', '--surface': '#ffffff', '--elevated': '#f0f2f5',
        '--border': '#d9dde5', '--text-primary': '#1a202c', '--text-muted': '#718096',
        '--download': '#0284c7', '--upload': '#ea580c', '--brand': '#059669',
        '--danger': '#dc2626', '--lan': 'rgba(5,150,105,0.12)', '--lan-text': '#059669',
        '--internet': 'rgba(2,132,199,0.12)', '--internet-text': '#0284c7',
    },
};

const THEME_NAMES = Object.keys(THEMES);
let activeTheme = localStorage.getItem('curb-theme') || 'CURB Dark';

function applyTheme(name) {
    const t = THEMES[name] || parseCustomTheme();
    if (!t) return;
    const root = document.documentElement;
    for (const [k, v] of Object.entries(t)) root.style.setProperty(k, v);
    activeTheme = name;
    localStorage.setItem('curb-theme', name);
    // Re-render settings to update the selection highlight.
    if (currentView === 'Settings') renderSettings();
}

function parseCustomTheme() {
    try {
        const raw = localStorage.getItem('curb-custom-theme');
        return raw ? JSON.parse(raw) : null;
    } catch { return null; }
}

let daemonStatus = null;
let availableInterfaces = [];

async function loadSettingsData() {
    try {
        const [status, ifaces] = await Promise.all([
            invoke('get_status'),
            invoke('list_interfaces'),
        ]);
        daemonStatus = status;
        availableInterfaces = ifaces;
    } catch (_) {}
    renderSettings();
}

function renderSettings() {
    const view = document.getElementById('settings-view');

    const themeCards = THEME_NAMES.map(name => {
        const t = THEMES[name];
        const active = name === activeTheme;
        return `<div class="theme-card ${active ? 'theme-card-active' : ''}" data-theme="${escHtml(name)}">
            <div class="theme-preview" style="background:${t['--app-bg']}; border-color:${t['--border']};">
                <div class="theme-prev-sidebar" style="background:${t['--surface']}; border-color:${t['--border']};"></div>
                <div class="theme-prev-body" style="background:${t['--app-bg']};">
                    <div class="theme-prev-bar" style="background:${t['--elevated']}; border-color:${t['--border']};"></div>
                    <div class="theme-prev-row" style="background:${t['--surface']}; border-color:${t['--border']};"></div>
                    <div class="theme-prev-accent" style="background:${t['--brand']};"></div>
                    <div class="theme-prev-dl" style="background:${t['--download']};"></div>
                    <div class="theme-prev-ul" style="background:${t['--upload']};"></div>
                </div>
            </div>
            <div class="theme-name">${escHtml(name)}</div>
            ${active ? '<div class="theme-active-badge">Active</div>' : ''}
        </div>`;
    }).join('');

    const customRaw = localStorage.getItem('curb-custom-theme') || '';
    const ifaceCards = availableInterfaces.map(i => {
        const cls = i.is_active ? 'iface-card iface-card-active' : 'iface-card';
        const dot = i.is_up ? 'iface-dot iface-dot-up' : 'iface-dot iface-dot-down';
        return `<div class="${cls}" data-iface="${escHtml(i.name)}">
            <span class="${dot}"></span>
            <span class="iface-name">${escHtml(i.name)}</span>
            ${i.is_active ? '<span class="iface-badge">active</span>' : ''}
        </div>`;
    }).join('');

    view.innerHTML = `
        <div class="settings-scroll">
            <section class="settings-section">
                <div class="settings-section-title">Theme</div>
                <div class="theme-grid" id="theme-grid">${themeCards}</div>
            </section>

            <section class="settings-section">
                <div class="settings-section-title">Custom Theme</div>
                <p class="settings-hint">
                    Paste a JSON object with CSS variable overrides below and click Apply.
                    See <code>CREATING_THEMES.md</code> in the project directory for the full variable list
                    and instructions on asking an AI to generate a theme.
                </p>
                <textarea id="custom-theme-input" class="custom-theme-ta" spellcheck="false" placeholder='{
  "--app-bg": "#0b0e14",
  "--surface": "#131722",
  "--download": "#38bdf8",
  "--upload": "#fb923c"
}'>${escHtml(customRaw)}</textarea>
                <div class="settings-row">
                    <button id="apply-custom-theme-btn" class="modal-btn modal-btn-ok">Apply Custom Theme</button>
                    <button id="clear-custom-theme-btn" class="modal-btn modal-btn-cancel">Clear</button>
                    <span id="custom-theme-error" class="custom-theme-err"></span>
                </div>
            </section>

            <section class="settings-section">
                <div class="settings-section-title">Network Interface</div>
                <p class="settings-hint">Select the interface CURB shapes traffic on. Changes take effect after restarting the daemon (<code>sudo systemctl restart curbd</code>).</p>
                <div class="iface-grid" id="iface-grid">${ifaceCards || '<span style="color:var(--text-muted)">No interfaces found</span>'}</div>
                <div id="iface-restart-notice" class="settings-notice" style="display:none">
                    Interface saved. Run <code>sudo systemctl restart curbd</code> to apply.
                </div>
            </section>

            <section class="settings-section">
                <div class="settings-section-title">About</div>
                <div class="about-grid">
                    <span class="about-label">CURB version</span>
                    <span class="about-value">${daemonStatus ? escHtml(daemonStatus.daemon_version) : '—'}</span>
                    <span class="about-label">Daemon PID</span>
                    <span class="about-value">${daemonStatus ? daemonStatus.pid : '—'}</span>
                    <span class="about-label">Uptime</span>
                    <span class="about-value">${daemonStatus ? formatUptime(daemonStatus.uptime_secs) : '—'}</span>
                </div>
            </section>
        </div>
    `;

    document.getElementById('theme-grid').addEventListener('click', (e) => {
        const card = e.target.closest('[data-theme]');
        if (card) applyTheme(card.dataset.theme);
    });
    document.getElementById('apply-custom-theme-btn').onclick = applyCustomTheme;
    document.getElementById('clear-custom-theme-btn').onclick = clearCustomTheme;

    document.getElementById('iface-grid').addEventListener('click', async (e) => {
        const card = e.target.closest('[data-iface]');
        if (!card) return;
        const name = card.dataset.iface;
        try {
            await invoke('set_interface', { name });
            // Update local state so the card highlights immediately.
            availableInterfaces = availableInterfaces.map(i => ({ ...i, is_active: i.name === name }));
            document.getElementById('iface-restart-notice').style.display = '';
            // Re-render just the iface grid.
            document.getElementById('iface-grid').innerHTML = availableInterfaces.map(i => {
                const cls = i.is_active ? 'iface-card iface-card-active' : 'iface-card';
                const dot = i.is_up ? 'iface-dot iface-dot-up' : 'iface-dot iface-dot-down';
                return `<div class="${cls}" data-iface="${escHtml(i.name)}">
                    <span class="${dot}"></span>
                    <span class="iface-name">${escHtml(i.name)}</span>
                    ${i.is_active ? '<span class="iface-badge">active</span>' : ''}
                </div>`;
            }).join('');
        } catch (err) {
            console.error('set_interface failed:', err);
        }
    });
}

function formatUptime(secs) {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return `${h}h ${m}m`;
}

function applyCustomTheme() {
    const raw = document.getElementById('custom-theme-input').value.trim();
    const errEl = document.getElementById('custom-theme-error');
    try {
        const parsed = JSON.parse(raw);
        if (typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Must be a JSON object');
        const root = document.documentElement;
        for (const [k, v] of Object.entries(parsed)) {
            if (k.startsWith('--')) root.style.setProperty(k, v);
        }
        localStorage.setItem('curb-custom-theme', raw);
        activeTheme = 'Custom';
        localStorage.setItem('curb-theme', 'Custom');
        errEl.textContent = 'Custom theme applied.';
        errEl.style.color = 'var(--brand)';
        // De-highlight preset cards.
        document.querySelectorAll('.theme-card').forEach(c => c.classList.remove('theme-card-active'));
    } catch (e) {
        errEl.textContent = 'Invalid JSON: ' + e.message;
        errEl.style.color = 'var(--danger)';
    }
}

function clearCustomTheme() {
    localStorage.removeItem('curb-custom-theme');
    document.getElementById('custom-theme-input').value = '';
    // Revert to last named theme or default.
    const fallback = THEME_NAMES.includes(activeTheme) ? activeTheme : 'CURB Dark';
    applyTheme(fallback);
}

// Apply saved theme on startup.
if (activeTheme === 'Custom') {
    const custom = parseCustomTheme();
    if (custom) {
        const root = document.documentElement;
        for (const [k, v] of Object.entries(custom)) root.style.setProperty(k, v);
    } else {
        applyTheme('CURB Dark');
    }
} else {
    applyTheme(activeTheme);
}

// ── Start polling ─────────────────────────────────────────────────────────────
// Start polling
poll();
setInterval(poll, 1000);
