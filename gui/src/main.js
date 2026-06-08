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
let selectedExe = null;
let searchQuery = "";
let hostDownHistory = Array(60).fill(0);
let hostUpHistory = Array(60).fill(0);
let expandedNodes = new Set(['host']); // host expanded by default
let currentView = 'Applications';

// --- Helpers ---

function formatRate(bps) {
    if (bps === null || bps === undefined) return "—";
    const bits = bps * 8;
    if (bits >= 1000000000) return (bits / 1000000000).toFixed(1) + " Gbit";
    if (bits >= 1000000) return (bits / 1000000).toFixed(1) + " Mbit";
    if (bits >= 1000) return (bits / 1000).toFixed(1) + " Kbit";
    return bits.toFixed(0) + " bit";
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

// --- Data Fetching ---

async function poll() {
    try {
        const [snapshot, limiter, limits, q] = await Promise.all([
            invoke('list_apps'),
            invoke('get_limiter'),
            invoke('list_app_limits'),
            invoke('list_quotas')
        ]);

        apps = snapshot.apps;
        hostTotals = snapshot.host;
        limiterState = limiter;
        appLimits = limits;
        quotas = q;

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
    renderTable();
    renderDetailStrip();
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

function renderTable() {
    const tbody = document.getElementById('apps-body');
    tbody.innerHTML = "";

    // Tabs other than the main monitor show a placeholder for now.
    if (currentView !== 'Applications' && currentView !== 'Dashboard') {
        const tr = document.createElement('tr');
        tr.innerHTML = `<td colspan="8" class="view-placeholder">${currentView} — coming soon.<br>
            <span>Use the Applications tab for live monitoring and limits.</span></td>`;
        tbody.appendChild(tr);
        return;
    }

    // ONE tree rooted at "This computer". Children: the Local Network and
    // Internet host zones, then every application — all nested under the host.
    renderHostRoot(tbody);
    if (expandedNodes.has('host')) {
        renderHostRow(tbody, 'Local Network', limiterState.host.lan.down_bps, limiterState.host.lan.up_bps, 1, 'lan');
        renderHostRow(tbody, 'Internet', limiterState.host.internet.down_bps, limiterState.host.internet.up_bps, 1, 'internet');

        const filteredApps = apps.filter(app => {
            if (!app.exe) return false;
            if (!searchQuery) return true;
            return app.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
                   app.exe.toLowerCase().includes(searchQuery.toLowerCase());
        });
        filteredApps.forEach(app => renderAppRow(tbody, app, 1));
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
        <td></td>
        <td><span class="download-text">${formatRate(hostTotals.down_bps)}</span></td>
        <td><span class="upload-text">${formatRate(hostTotals.up_bps)}</span></td>
        <td class="td-down-lim"></td>
        <td class="td-up-lim"></td>
        <td></td>
        <td></td>
    `;
    const toggle = () => toggleNode('host');
    tr.querySelector('.tree-toggle').onclick = (e) => { e.stopPropagation(); toggle(); };
    tr.querySelector('.tree-indent > span').onclick = (e) => { e.stopPropagation(); toggle(); };
    tr.querySelector('.td-down-lim').appendChild(
        createSpinner(limiterState.host.down_bps, (v) => setHostLimit('host', 'down', v)));
    tr.querySelector('.td-up-lim').appendChild(
        createSpinner(limiterState.host.up_bps, (v) => setHostLimit('host', 'up', v)));
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

function renderHostRow(tbody, label, downLim, upLim, level, type) {
    const tr = document.createElement('tr');
    tr.className = 'tree-row';
    
    // Live rate for the zone rows, split by LAN vs Internet.
    let downRate = "—", upRate = "—";
    if (type === 'lan') {
        downRate = formatRate(hostTotals.lan_down_bps || 0);
        upRate = formatRate(hostTotals.lan_up_bps || 0);
    } else if (type === 'internet') {
        downRate = formatRate(hostTotals.inet_down_bps || 0);
        upRate = formatRate(hostTotals.inet_up_bps || 0);
    } else if (type === 'host') {
        downRate = formatRate(hostTotals.down_bps);
        upRate = formatRate(hostTotals.up_bps);
    }

    tr.innerHTML = `
        <td>
            <div class="tree-indent" style="--level: ${level}">
                <div class="tree-guide" style="--level: ${level}"></div>
                <span style="font-weight: 600;">${label}</span>
            </div>
        </td>
        <td></td>
        <td><span class="download-text">${downRate}</span></td>
        <td><span class="upload-text">${upRate}</span></td>
        <td class="td-down-lim"></td>
        <td class="td-up-lim"></td>
        <td></td>
        <td></td>
    `;

    const downSpinner = createSpinner(downLim, (val) => setHostLimit(type, 'down', val));
    const upSpinner = createSpinner(upLim, (val) => setHostLimit(type, 'up', val));
    
    tr.querySelector('.td-down-lim').appendChild(downSpinner);
    tr.querySelector('.td-up-lim').appendChild(upSpinner);

    tbody.appendChild(tr);
}

function renderAppRow(tbody, app, level) {
    const limit = appLimits.find(l => l.exe === app.exe);
    const quota = quotas.find(q => q.exe === app.exe);

    let status = "Watching";
    let statusClass = "pill-muted";
    if (quota && quota.exceeded) {
        status = "Throttled";
        statusClass = "pill-rose";
    } else if (limit) {
        status = "Limited";
        statusClass = "pill-emerald";
    }

    const tr = document.createElement('tr');
    tr.className = 'tree-row';
    if (app.exe === selectedExe) tr.classList.add('selected');
    
    tr.onclick = () => {
        selectedExe = app.exe;
        renderDetailStrip(); // Just update detail strip, don't re-render whole table to preserve inputs
        document.querySelectorAll('#apps-body tr').forEach(r => r.classList.remove('selected'));
        tr.classList.add('selected');
    };

    const scope = limit ? limit.scope : "Both";
    const quotaPercent = quota ? Math.min((quota.used_bytes / quota.budget_bytes) * 100, 100) : 0;
    const quotaExceeded = quota ? quota.exceeded : false;

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
        <td><span class="pill ${statusClass}">${status}</span></td>
        <td>
            <div class="rate-cell">
                <span class="download-text">${formatRate(app.down_bps)}</span>
                <svg class="inline-spark" id="spark-down-${app.exe.replace(/\//g, '_')}"></svg>
            </div>
        </td>
        <td>
            <div class="rate-cell">
                <span class="upload-text">${formatRate(app.up_bps)}</span>
                <svg class="inline-spark" id="spark-up-${app.exe.replace(/\//g, '_')}"></svg>
            </div>
        </td>
        <td class="td-down-lim"></td>
        <td class="td-up-lim"></td>
        <td><span class="pill pill-${scope.toLowerCase()} scope-pill" style="cursor: pointer;">${scope}</span></td>
        <td>
            <div class="quota-container">
                <div class="quota-bar-bg">
                    <div class="quota-bar-fill ${quotaExceeded ? 'danger' : ''}" style="width: ${quotaPercent}%"></div>
                </div>
                <span class="quota-label">${quota ? formatSize(quota.used_bytes) + ' / ' + formatSize(quota.budget_bytes) : '—'}</span>
            </div>
        </td>
    `;

    const downSpinner = createSpinner(limit ? limit.down_bps : null, (val) => setAppLimit(app.exe, 'down', val));
    const upSpinner = createSpinner(limit ? limit.up_bps : null, (val) => setAppLimit(app.exe, 'up', val));
    
    tr.querySelector('.td-down-lim').appendChild(downSpinner);
    tr.querySelector('.td-up-lim').appendChild(upSpinner);

    tr.querySelector('.scope-pill').onclick = (e) => { e.stopPropagation(); cycleScope(app.exe); };
    tr.querySelector('.quota-container').onclick = (e) => { e.stopPropagation(); promptQuota(app.exe); };

    tbody.appendChild(tr);

    createSparkline(document.getElementById(`spark-down-${app.exe.replace(/\//g, '_')}`), app.down_spark, "var(--download)");
    createSparkline(document.getElementById(`spark-up-${app.exe.replace(/\//g, '_')}`), app.up_spark, "var(--upload)");
}

function renderDetailStrip() {
    const app = apps.find(a => a.exe === selectedExe);
    const detailInfo = document.getElementById('detail-app-info');
    if (!app) {
        detailInfo.querySelector('.detail-title').textContent = "—";
        detailInfo.querySelector('.app-path').textContent = "—";
        detailInfo.querySelector('div:last-child').innerHTML = "";
        document.getElementById('detail-down-val').textContent = "0 bit";
        document.getElementById('detail-up-val').textContent = "0 bit";
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

// Start polling
poll();
setInterval(poll, 1000);
