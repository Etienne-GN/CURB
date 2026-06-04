const { invoke } = window.__TAURI__.core;

// --- State ---
let apps = [];
let hostTotals = { down_bps: 0, up_bps: 0, down_total: 0, up_total: 0 };
let limiterState = { enabled: false, host: { down_bps: null, up_bps: null }, interface: "" };
let appLimits = [];
let quotas = [];
let selectedExe = null;
let searchQuery = "";
let hostDownHistory = Array(60).fill(0);
let hostUpHistory = Array(60).fill(0);

// --- Helpers ---

function formatRate(bps) {
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
    if (!input) return null;
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

    const hostLimitText = document.getElementById('host-limit-text');
    const downLim = limiterState.host.down_bps ? formatRate(limiterState.host.down_bps) : "∞";
    const upLim = limiterState.host.up_bps ? formatRate(limiterState.host.up_bps) : "∞";
    hostLimitText.textContent = `↓ ${downLim} / ↑ ${upLim}`;

    document.getElementById('interface-name').textContent = limiterState.interface || "";
}

function renderTable() {
    const tbody = document.getElementById('apps-body');
    const filteredApps = apps.filter(app => {
        if (!app.exe) return false; // Skip unresolved for now or handle them later
        if (!searchQuery) return true;
        return app.name.toLowerCase().includes(searchQuery.toLowerCase()) || 
               app.exe.toLowerCase().includes(searchQuery.toLowerCase());
    });

    // If no app selected, pick first
    if (!selectedExe && filteredApps.length > 0) {
        selectedExe = filteredApps[0].exe;
    }

    tbody.innerHTML = "";
    filteredApps.forEach(app => {
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
        if (app.exe === selectedExe) tr.classList.add('selected');
        
        tr.onclick = () => {
            selectedExe = app.exe;
            renderUI();
        };

        const downLimStr = limit && limit.down_bps ? formatRate(limit.down_bps) : "—";
        const upLimStr = limit && limit.up_bps ? formatRate(limit.up_bps) : "—";
        const scope = limit ? limit.scope : "Both";

        const quotaPercent = quota ? Math.min((quota.used_bytes / quota.budget_bytes) * 100, 100) : 0;
        const quotaExceeded = quota ? quota.exceeded : false;

        tr.innerHTML = `
            <td>
                <div class="app-cell">
                    <div class="app-icon" style="border-top: 2px solid #34d399">${app.name[0].toUpperCase()}</div>
                    <div class="app-info">
                        <span class="app-name">${app.name}</span>
                        <span class="app-path">${app.exe}</span>
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
            <td><div class="limit-chip" data-dir="down">${downLimStr}</div></td>
            <td><div class="limit-chip" data-dir="up">${upLimStr}</div></td>
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

        // Click handlers for the specific chips
        tr.querySelector('[data-dir="down"]').onclick = (e) => { e.stopPropagation(); promptAppLimit(app.exe, 'down'); };
        tr.querySelector('[data-dir="up"]').onclick = (e) => { e.stopPropagation(); promptAppLimit(app.exe, 'up'); };
        tr.querySelector('.scope-pill').onclick = (e) => { e.stopPropagation(); cycleScope(app.exe); };
        tr.querySelector('.quota-container').onclick = (e) => { e.stopPropagation(); promptQuota(app.exe); };

        tbody.appendChild(tr);

        createSparkline(document.getElementById(`spark-down-${app.exe.replace(/\//g, '_')}`), app.down_spark, "var(--download)");
        createSparkline(document.getElementById(`spark-up-${app.exe.replace(/\//g, '_')}`), app.up_spark, "var(--upload)");
    });
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

async function promptHostLimit() {
    const down = prompt("Host Download Limit (e.g. 100mbit, 10mb, or leave empty for unlimited):", 
        limiterState.host.down_bps ? formatRate(limiterState.host.down_bps) : "");
    const up = prompt("Host Upload Limit (e.g. 20mbit, 2mb, or leave empty for unlimited):", 
        limiterState.host.up_bps ? formatRate(limiterState.host.up_bps) : "");
    
    try {
        await invoke('set_host_limit', { 
            downBps: down === null ? undefined : parseRate(down), 
            upBps: up === null ? undefined : parseRate(up) 
        });
        poll();
    } catch (err) { alert(err); }
}

async function promptAppLimit(exe, dir) {
    const limit = appLimits.find(l => l.exe === exe);
    const currentVal = limit ? (dir === 'down' ? limit.down_bps : limit.up_bps) : null;
    const input = prompt(`${dir.toUpperCase()} limit for ${exe} (e.g. 5mbit, 1mb, or empty to clear):`, 
        currentVal ? formatRate(currentVal) : "");
    
    if (input === null) return;
    
    const newVal = parseRate(input);
    let downBps = limit ? limit.down_bps : null;
    let upBps = limit ? limit.up_bps : null;
    let scope = limit ? limit.scope.toLowerCase() : 'both';

    if (dir === 'down') downBps = newVal;
    else upBps = newVal;

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

document.getElementById('master-limiter-toggle').onclick = toggleMasterLimiter;
document.getElementById('host-limit-toggle').onclick = promptHostLimit;
document.getElementById('search-apps').oninput = (e) => {
    searchQuery = e.target.value;
    renderTable();
};

// Start polling
poll();
setInterval(poll, 1000);
