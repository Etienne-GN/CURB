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
let hostUpHistory = Array(60).fill(0);
let expandedNodes = new Set(['host', 'lan', 'internet']); // all expanded by default
let currentView = 'Dashboard';

// --- Helpers ---

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
    const width = 1000;
    const height = 100;
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
    const max = Math.max(...data, 1);
    const points = data.map((d, i) => {
        const x = (i / (data.length - 1)) * width;
        const y = height - (d / max) * height;
        return `${x},${y}`;
    });

    const polylinePoints = points.join(" ");
    const areaPoints = `0,${height} ${polylinePoints} ${width},${height}`;

    svg.innerHTML = `
        <polygon points="${areaPoints}" fill="${color}" class="area-fill" />
        <polyline points="${polylinePoints}" stroke="${color}" />
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
        hostDownHistory.push(hostTotals.down_bps);
        hostDownHistory.shift();
        hostUpHistory.push(hostTotals.up_bps);
        hostUpHistory.shift();

        document.getElementById('error-banner').style.display = 'none';
        renderUI();
    } catch (err) {
        console.error("Poll error:", err);
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

    if (!popupOpen) {
        if      (v === 'Dashboard')    renderDashboard();
        else if (v === 'Applications') renderTable();
        else if (v === 'Rules')        renderRules();
        else if (v === 'Quotas')       renderQuotas();
        else if (v === 'Connections')  renderConnections();
    }
    renderDetailStrip();
}

function renderDashboard() {
    const dash = document.getElementById('dashboard-view');
    const topApps = [...apps]
        .sort((a, b) => (b.down_bps + b.up_bps) - (a.down_bps + a.up_bps))
        .slice(0, 6);

    dash.innerHTML = `
        <div class="dash-row">
            <div class="dash-card">
                <div class="dash-label">↓ Download</div>
                <div class="dash-big download-text">${formatRate(hostTotals.down_bps)}</div>
                <svg class="dash-spark" id="dsp-down"></svg>
                <div class="dash-sub">${formatSize(hostTotals.down_total)} total since start</div>
            </div>
            <div class="dash-card">
                <div class="dash-label">↑ Upload</div>
                <div class="dash-big upload-text">${formatRate(hostTotals.up_bps)}</div>
                <svg class="dash-spark" id="dsp-up"></svg>
                <div class="dash-sub">${formatSize(hostTotals.up_total)} total since start</div>
            </div>
            <div class="dash-card">
                <div class="dash-label">Limiter</div>
                <div class="dash-limiter-status" style="color:${limiterState.enabled ? 'var(--brand)' : 'var(--text-muted)'}">
                    ${limiterState.enabled ? 'ON' : 'OFF'}
                </div>
                <div class="dash-iface">${limiterState.interface || '—'}</div>
            </div>
        </div>
        <div class="dash-row">
            <div class="dash-card">
                <div class="dash-label">Local Network</div>
                <div class="dash-zone-rates">
                    <span class="download-text">↓ ${formatRate(hostTotals.lan_down_bps || 0)}</span>
                    <span class="upload-text">↑ ${formatRate(hostTotals.lan_up_bps || 0)}</span>
                </div>
            </div>
            <div class="dash-card">
                <div class="dash-label">Internet</div>
                <div class="dash-zone-rates">
                    <span class="download-text">↓ ${formatRate(hostTotals.inet_down_bps || 0)}</span>
                    <span class="upload-text">↑ ${formatRate(hostTotals.inet_up_bps || 0)}</span>
                </div>
            </div>
            <div class="dash-card dash-card-wide">
                <div class="dash-label">Top Applications</div>
                <div class="dash-app-list">
                    ${topApps.length === 0
                        ? '<div class="dash-sub">No traffic observed yet.</div>'
                        : topApps.map(a => `
                            <div class="dash-app-row">
                                <div class="dash-app-icon">${a.name[0].toUpperCase()}</div>
                                <span class="dash-app-name">${a.name}</span>
                                <div class="dash-app-rates">
                                    <span class="download-text">↓ ${formatRate(a.down_bps)}</span>
                                    <span class="upload-text">↑ ${formatRate(a.up_bps)}</span>
                                </div>
                            </div>`).join('')}
                </div>
            </div>
        </div>
    `;

    createSparkline(document.getElementById('dsp-down'), hostDownHistory, 'var(--download)', 120, 32);
    createSparkline(document.getElementById('dsp-up'),   hostUpHistory,   'var(--upload)',   120, 32);
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
                    <div class="app-icon" style="border-top: 2px solid #34d399">${app.name[0].toUpperCase()}</div>
                    <div class="app-info">
                        <span class="app-name">${app.name}</span>
                        <span class="app-path">${app.exe}</span>
                    </div>
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
            <div class="quota-container">
                <div class="quota-bar-bg">
                    <div class="quota-bar-fill ${quotaExceeded ? 'danger' : ''}" style="width: ${quotaPercent}%"></div>
                </div>
                <span class="quota-label">${quota ? formatSize(quota.used_bytes) + ' / ' + formatSize(quota.budget_bytes) : '—'}</span>
            </div>
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

    tr.querySelector('.quota-container').onclick = (e) => { e.stopPropagation(); promptQuota(app.exe); };

    tbody.appendChild(tr);

    createSparkline(document.getElementById(`spark-down-${exeId}`), app.down_spark, "var(--download)");
    createSparkline(document.getElementById(`spark-up-${exeId}`), app.up_spark, "var(--upload)");
}

function renderDetailStrip() {
    const app = apps.find(a => a.exe === selectedExe);
    const detailInfo = document.getElementById('detail-app-info');
    if (!app) {
        detailInfo.querySelector('.detail-title').textContent = "—";
        detailInfo.querySelector('.app-path').textContent = "—";
        detailInfo.querySelector('div:last-child').innerHTML = "";
        document.getElementById('detail-down-val').textContent = "0 B/s";
        document.getElementById('detail-up-val').textContent = "0 B/s";
        return;
    }

    const limit = appLimits.find(l => l.exe === app.exe);
    const quota = quotas.find(q => q.exe === app.exe);

    detailInfo.querySelector('.detail-title').textContent = app.name;
    detailInfo.querySelector('.app-path').textContent = app.exe;
    
    let status = "Watching";
    let statusClass = "pill-muted";
    if (quota && quota.exceeded) {
        status = "Throttled";
        statusClass = "pill-rose";
    } else if (limit) {
        status = "Limited";
        statusClass = "pill-emerald";
    }

    const pillsContainer = detailInfo.querySelector('div:last-child');
    pillsContainer.innerHTML = `<div class="pill ${statusClass}">${status}</div>`;
    if (limit) {
        pillsContainer.innerHTML += `<div class="pill pill-${limit.scope.toLowerCase()}">${limit.scope}</div>`;
    }

    document.getElementById('detail-down-val').textContent = formatRate(app.down_bps);
    document.getElementById('detail-up-val').textContent = formatRate(app.up_bps);

    createAreaGraph(document.getElementById('svg-detail-down'), app.down_spark, "var(--download)");
    createAreaGraph(document.getElementById('svg-detail-up'), app.up_spark, "var(--upload)");
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

// Wire the sidebar tabs: switch the active item and the current view.
document.querySelectorAll('.nav-item').forEach((item) => {
    item.onclick = (e) => {
        e.preventDefault();
        currentView = item.textContent.trim();
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

// Start polling
poll();
setInterval(poll, 1000);
