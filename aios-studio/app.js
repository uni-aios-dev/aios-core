const API = {
  health: '/api/v1/health',
  status: '/api/v1/system/status',
  intent: '/api/v1/intent',
  workflow: '/api/v1/workflow',
  ws: () => {
    const p = window.location;
    const proto = p.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${proto}//${p.host}/ws/telemetry`;
  }
};

const MAX_POINTS = 120;
let ramHistory = [];
let telemetryBuf = [];
let ws = null;
let wsReconnectTimer = null;
let wsRetryDelay = 1000;
let statusTimer = null;

/* ── Tab Switching ── */
function switchTab(name) {
  document.querySelectorAll('.tab-content').forEach(t => t.classList.remove('active'));
  document.querySelectorAll('.nav-item').forEach(n => n.classList.remove('active'));
  document.getElementById(`tab-${name}`).classList.add('active');
  document.querySelector(`.nav-item[data-tab="${name}"]`).classList.add('active');
  if (name === 'dashboard') requestAnimationFrame(drawChart);
}

/* ── Command Palette ── */
function openCommandPalette() {
  const overlay = document.getElementById('command-overlay');
  const input = document.getElementById('command-input');
  const result = document.getElementById('command-result');
  overlay.classList.remove('hidden');
  result.classList.add('hidden');
  result.innerHTML = '';
  input.value = '';
  setTimeout(() => input.focus(), 50);
}

function closeCommandPalette(e) {
  if (e && e.target !== document.getElementById('command-overlay')) return;
  document.getElementById('command-overlay').classList.add('hidden');
}

document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
    e.preventDefault();
    openCommandPalette();
  }
  if (e.key === 'Escape') {
    const overlay = document.getElementById('command-overlay');
    if (!overlay.classList.contains('hidden')) overlay.classList.add('hidden');
  }
});

document.getElementById('command-input').addEventListener('keydown', async e => {
  if (e.key !== 'Enter') return;
  const input = document.getElementById('command-input');
  const prompt = input.value.trim();
  if (!prompt) return;
  e.preventDefault();
  input.disabled = true;
  const resultDiv = document.getElementById('command-result');
  resultDiv.classList.remove('hidden');
  resultDiv.innerHTML = '<em style="color:var(--text-muted)">Executing…</em>';
  sendIntent(prompt).then(res => {
    if (res === null) {
      resultDiv.innerHTML = `<div class="result-error">Network error — is the bridge running?</div>`;
    } else if (res.error) {
      resultDiv.innerHTML = `<div class="result-error">${escapeHtml(res.error)}</div>`;
    } else {
      let caps = '';
      if (res.required_capabilities && res.required_capabilities.length) {
        caps = `<div class="caps-used">Capabilities: ${res.required_capabilities.map(c => `<span>${c}</span>`).join(', ')}</div>`;
      }
      resultDiv.innerHTML = `<div class="result-success"><strong>${escapeHtml(res.description || res.intent_type)}</strong><pre>${escapeHtml(JSON.stringify(res.result, null, 2))}</pre>${caps}</div>`;
    }
    input.disabled = false;
    input.focus();
    input.select();
  });
});

/* ── API ── */
async function sendIntent(prompt) {
  try {
    const r = await fetch(API.intent, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt })
    });
    if (!r.ok) {
      const body = await r.json().catch(() => null);
      return body || { error: `HTTP ${r.status}: ${r.statusText}` };
    }
    return await r.json();
  } catch (e) {
    return null;
  }
}

async function fetchStatus() {
  try {
    const r = await fetch(API.status);
    return r.ok ? await r.json() : null;
  } catch { return null; }
}

function refreshStatus() {
  updateDashboard();
}

/* ── WebSocket with auto-reconnect ── */
function connectWs() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;

  setConnStatus('connecting');

  try {
    ws = new WebSocket(API.ws());
  } catch (e) {
    setConnStatus('error');
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    setConnStatus('connected');
    wsRetryDelay = 1000;
  };

  ws.onmessage = e => {
    try {
      const data = JSON.parse(e.data);
      telemetryBuf.push(data);
      if (telemetryBuf.length > MAX_POINTS) telemetryBuf.shift();

      const pct = data.ram_percent != null ? data.ram_percent : 0;
      ramHistory.push(pct);
      if (ramHistory.length > MAX_POINTS) ramHistory.shift();

      if (document.getElementById('tab-dashboard').classList.contains('active')) {
        updateHealthCards(data);
        updateRamBadge(pct);
        requestAnimationFrame(drawChart);
      }
    } catch { /* ignore malformed */ }
  };

  ws.onclose = () => {
    if (document.getElementById('connection-status').classList.contains('connected')) {
      setConnStatus('connecting');
    }
    ws = null;
    scheduleReconnect();
  };

  ws.onerror = () => {
    ws && ws.close();
  };
}

function scheduleReconnect() {
  if (wsReconnectTimer) return;
  wsReconnectTimer = setTimeout(() => {
    wsReconnectTimer = null;
    connectWs();
  }, wsRetryDelay);
  wsRetryDelay = Math.min(wsRetryDelay * 1.5, 15000);
}

function setConnStatus(status) {
  const el = document.getElementById('connection-status');
  el.className = 'status-badge ' + status;
  const label = el.querySelector('.status-label');
  const labels = { connected: 'Connected', connecting: 'Reconnecting…', disconnected: 'Disconnected', error: 'Connection Error' };
  label.textContent = labels[status] || status;
}

/* ── Dashboard Updates ── */
function updateHealthCards(data) {
  document.getElementById('sys-status').textContent = data.status || '—';
}

function updateRamBadge(pct) {
  document.getElementById('ram-badge').textContent = Math.round(pct) + '%';
}

async function updateDashboard() {
  const status = await fetchStatus();
  if (!status) return;

  document.getElementById('sys-status').textContent = status.status || '—';
  document.getElementById('sys-uptime').textContent = status.watchdog ? formatUptime(status.watchdog.uptime_secs) : '—';
  document.getElementById('wd-status').textContent = status.watchdog ? status.watchdog.state : '—';
  document.getElementById('wd-uptime').textContent = status.watchdog ? formatUptime(status.watchdog.uptime_secs) : '—';

  if (status.processes) {
    document.getElementById('proc-count').textContent = status.processes.total ?? '—';
    document.getElementById('proc-detail').textContent = `${status.processes.running ?? 0} running, ${status.processes.suspended ?? 0} suspended`;
    document.getElementById('proc-badge').textContent = status.processes.total ?? 0;
    renderProcessTable(status.processes.entries || []);
  }

  if (status.resources) {
    const r = status.resources;
    document.getElementById('mem-value').textContent = r.ram_percent != null ? Math.round(r.ram_percent) + '%' : '—';
    document.getElementById('mem-detail').textContent = `${r.ram_used_mb ?? '?'} MB / ${r.ram_total_mb ?? '?'} MB`;
  }

  if (status.blocks) {
    document.getElementById('block-badge').textContent = status.blocks.total ?? 0;
    renderBlocksTable(status.blocks.entries || []);
  }

  /* push status into telemetry buffer for chart consistency */
  if (status.resources && status.resources.ram_percent != null) {
    const pct = status.resources.ram_percent;
    ramHistory.push(pct);
    if (ramHistory.length > MAX_POINTS) ramHistory.shift();
    updateRamBadge(pct);
    requestAnimationFrame(drawChart);
  }
}

/* ── Process Table ── */
function renderProcessTable(entries) {
  const tbody = document.getElementById('process-tbody');
  if (!entries.length) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-state">No data</td></tr>';
    return;
  }
  tbody.innerHTML = entries.map(p => {
    const stateCls = 'state-' + (p.state ? p.state.toLowerCase() : 'unknown');
    return `<tr>
      <td>${p.pid}</td>
      <td>${escapeHtml(p.name)}</td>
      <td>${p.ram_mb ?? '—'} MB</td>
      <td>${p.cpu_ms ?? 0} ms</td>
      <td class="${stateCls}">${escapeHtml(p.state)}</td>
    </tr>`;
  }).join('');
}

/* ── Blocks Table ── */
function renderBlocksTable(entries) {
  const tbody = document.getElementById('blocks-tbody');
  if (!entries.length) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-state">No blocks loaded</td></tr>';
    return;
  }
  tbody.innerHTML = entries.map(b => {
    const stateCls = 'state-' + (b.state ? b.state.toLowerCase() : 'unknown');
    return `<tr>
      <td>${b.id}</td>
      <td>${escapeHtml(b.name)}</td>
      <td>${escapeHtml(b.version)}</td>
      <td class="${stateCls}">${escapeHtml(b.state)}</td>
      <td>
        <button class="action-btn danger" onclick="sendIntent('unload ${escapeHtml(b.name)}')">Stop</button>
      </td>
    </tr>`;
  }).join('');
}

/* ── Chart ── */
function drawChart() {
  const canvas = document.getElementById('ram-chart');
  if (!canvas || canvas.offsetWidth === 0) return;
  const rect = canvas.parentElement.getBoundingClientRect();
  canvas.width = rect.width - 32;
  canvas.height = 200;

  const ctx = canvas.getContext('2d');
  const w = canvas.width;
  const h = canvas.height;
  const p = { top: 16, bottom: 24, left: 40, right: 16 };
  const cw = w - p.left - p.right;
  const ch = h - p.top - p.bottom;

  ctx.clearRect(0, 0, w, h);

  const data = ramHistory.length > 0 ? ramHistory : [0];
  const maxVal = Math.max(100, ...data) * 1.1;

  /* grid lines */
  ctx.strokeStyle = '#1e1e2e';
  ctx.lineWidth = 1;
  ctx.font = '10px sans-serif';
  ctx.fillStyle = '#555577';
  ctx.textAlign = 'right';
  for (let i = 0; i <= 4; i++) {
    const y = p.top + (ch * i) / 4;
    ctx.beginPath();
    ctx.moveTo(p.left, y);
    ctx.lineTo(w - p.right, y);
    ctx.stroke();
    ctx.fillText(Math.round(maxVal - (maxVal * i) / 4) + '%', p.left - 4, y + 3);
  }

  if (data.length < 2) {
    ctx.fillStyle = '#555577';
    ctx.textAlign = 'center';
    ctx.font = '13px sans-serif';
    ctx.fillText('Waiting for data…', w / 2, h / 2);
    return;
  }

  const visible = data.slice(-MAX_POINTS);
  const len = visible.length;

  /* gradient fill */
  const grad = ctx.createLinearGradient(0, p.top, 0, p.top + ch);
  grad.addColorStop(0, 'rgba(124,111,240,0.2)');
  grad.addColorStop(1, 'rgba(124,111,240,0.02)');

  const step = cw / Math.max(len - 1, 1);

  /* fill area */
  ctx.beginPath();
  ctx.moveTo(p.left, p.top + ch);
  for (let i = 0; i < len; i++) {
    const x = p.left + i * step;
    const y = p.top + ch - (visible[i] / maxVal) * ch;
    ctx.lineTo(x, y);
  }
  ctx.lineTo(p.left + (len - 1) * step, p.top + ch);
  ctx.closePath();
  ctx.fillStyle = grad;
  ctx.fill();

  /* line */
  ctx.beginPath();
  ctx.strokeStyle = '#7c6ff0';
  ctx.lineWidth = 2;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  for (let i = 0; i < len; i++) {
    const x = p.left + i * step;
    const y = p.top + ch - (visible[i] / maxVal) * ch;
    i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
  }
  ctx.stroke();

  /* dot on latest */
  if (len > 0) {
    const lx = p.left + (len - 1) * step;
    const ly = p.top + ch - (visible[len - 1] / maxVal) * ch;
    ctx.beginPath();
    ctx.arc(lx, ly, 3, 0, Math.PI * 2);
    ctx.fillStyle = '#7c6ff0';
    ctx.fill();
    ctx.beginPath();
    ctx.arc(lx, ly, 6, 0, Math.PI * 2);
    ctx.fillStyle = 'rgba(124,111,240,0.25)';
    ctx.fill();
  }
}

/* ── Helpers ── */
function formatUptime(secs) {
  if (secs == null) return '—';
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}h ${m}m`;
}

function escapeHtml(str) {
  const d = document.createElement('div');
  d.textContent = str;
  return d.innerHTML;
}

/* ── Workflow Builder ── */
const STEP_LABELS = {
  on_timer: 'On Timer',
  on_event: 'On Event',
  spawn: 'Spawn Process',
  kill: 'Kill Process',
  load_block: 'Load Block',
  unload_block: 'Unload Block',
  compact: 'Compact Memory',
  query: 'System Query'
};

const STEP_PROMPTS = {
  on_timer: 'start timer for 60 seconds',
  on_event: 'listen for system event',
  spawn: 'spawn process',
  kill: 'kill process',
  load_block: 'load block',
  unload_block: 'unload block',
  compact: 'compact memory',
  query: 'query system status'
};

let workflow = [];
let editingStep = null;

function defaultPrompt(action) {
  return STEP_PROMPTS[action] || action;
}

function addStep(action) {
  workflow.push({ action, prompt: defaultPrompt(action) });
  renderWorkflow();
}

function removeStep(index) {
  workflow.splice(index, 1);
  if (editingStep === index) editingStep = null;
  else if (editingStep > index) editingStep--;
  renderWorkflow();
}

function moveStep(index, dir) {
  const target = index + dir;
  if (target < 0 || target >= workflow.length) return;
  [workflow[index], workflow[target]] = [workflow[target], workflow[index]];
  if (editingStep === index) editingStep = target;
  else if (editingStep === target) editingStep = index;
  renderWorkflow();
}

function clearWorkflow() {
  workflow = [];
  editingStep = null;
  renderWorkflow();
  document.getElementById('workflow-result').classList.add('hidden');
}

function beginEditPrompt(index) {
  editingStep = index;
  renderWorkflow();
  setTimeout(() => {
    const inp = document.querySelector('.step-prompt-input');
    if (inp) { inp.focus(); inp.select(); }
  }, 50);
}

function commitPrompt(index) {
  const inp = document.querySelector('.step-prompt-input');
  if (inp && workflow[index]) {
    workflow[index].prompt = inp.value.trim() || defaultPrompt(workflow[index].action);
  }
  editingStep = null;
  renderWorkflow();
}

document.addEventListener('click', e => {
  if (editingStep !== null && !e.target.closest('.step-card')) {
    const inp = document.querySelector('.step-prompt-input');
    if (inp && !inp.contains(e.target)) commitPrompt(editingStep);
  }
});

function renderWorkflow() {
  const list = document.getElementById('step-list');
  const count = document.getElementById('step-count');
  const runBtn = document.getElementById('run-workflow');

  count.textContent = workflow.length + ' step' + (workflow.length !== 1 ? 's' : '');
  runBtn.disabled = workflow.length === 0;

  if (workflow.length === 0) {
    list.innerHTML = '<div class="empty-state">Add steps from the palette to build your workflow</div>';
    return;
  }

  list.innerHTML = workflow.map((s, i) => {
    const label = STEP_LABELS[s.action] || s.action;
    const icon = document.querySelector(`.palette-item[data-action="${s.action}"] .p-icon`);
    const iconHtml = icon ? icon.outerHTML : '<span class="step-icon">▶</span>';
    const isEditing = editingStep === i;
    const promptHtml = isEditing
      ? `<input class="step-prompt-input" value="${escapeHtml(s.prompt)}" onkeydown="if(event.key==='Enter')commitPrompt(${i});if(event.key==='Escape'){event.preventDefault();renderWorkflow();}">`
      : `<span class="step-prompt" onclick="beginEditPrompt(${i})" title="Click to edit prompt">${escapeHtml(s.prompt)}</span>`;
    return `<div class="step-card">
      <span class="step-handle">⠿</span>
      ${iconHtml}
      <div style="flex:1;min-width:0">
        <div class="step-label">${i + 1}. ${label}</div>
        ${promptHtml}
      </div>
      <div class="step-actions">
        <button class="step-btn" onclick="moveStep(${i}, -1)" ${i === 0 ? 'disabled' : ''} title="Move up">↑</button>
        <button class="step-btn" onclick="moveStep(${i}, 1)" ${i === workflow.length - 1 ? 'disabled' : ''} title="Move down">↓</button>
        <button class="step-btn danger" onclick="removeStep(${i})" title="Remove step">✕</button>
      </div>
    </div>`;
  }).join('');
}

/* ── Save / Load Workflows ── */
const STORAGE_KEY = 'aios_workflows';

function saveWorkflow() {
  const name = document.getElementById('workflow-name').value.trim() || 'Untitled';
  const data = JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}');
  data[name] = workflow.map(s => ({ action: s.action, prompt: s.prompt }));
  localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
  showToast(`Workflow "${name}" saved`);
}

function showLoadWorkflow() {
  const data = JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}');
  const dropdown = document.getElementById('load-dropdown');
  const names = Object.keys(data);
  if (names.length === 0) {
    dropdown.innerHTML = '<div class="load-empty">No saved workflows</div>';
  } else {
    dropdown.innerHTML = names.map(n => `
      <div class="load-item">
        <span onclick="loadWorkflow('${escapeHtml(n)}')" style="flex:1;cursor:pointer">${escapeHtml(n)}</span>
        <span class="del-wf" onclick="event.stopPropagation();deleteWorkflow('${escapeHtml(n)}')" title="Delete">✕</span>
      </div>
    `).join('');
  }
  dropdown.classList.toggle('hidden');
}

function loadWorkflow(name) {
  const data = JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}');
  const saved = data[name];
  if (!saved) return;
  workflow = saved.map(s => ({ action: s.action, prompt: s.prompt }));
  editingStep = null;
  document.getElementById('workflow-name').value = name;
  document.getElementById('load-dropdown').classList.add('hidden');
  document.getElementById('workflow-result').classList.add('hidden');
  renderWorkflow();
  showToast(`Workflow "${name}" loaded`);
}

function deleteWorkflow(name) {
  const data = JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}');
  delete data[name];
  localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
  showLoadWorkflow();
  showToast(`Workflow "${name}" deleted`);
}

function showToast(msg) {
  let el = document.getElementById('toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'toast';
    el.style.cssText = 'position:fixed;bottom:24px;right:24px;background:var(--bg-card);border:1px solid var(--border);border-radius:var(--radius);padding:10px 16px;font-size:13px;color:var(--text-primary);z-index:9999;box-shadow:0 8px 24px rgba(0,0,0,.3);transition:opacity .3s;opacity:0';
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.style.opacity = '1';
  clearTimeout(el._hide);
  el._hide = setTimeout(() => { el.style.opacity = '0'; }, 2500);
}

/* close load dropdown on outside click */
document.addEventListener('click', e => {
  const dd = document.getElementById('load-dropdown');
  if (dd && !dd.classList.contains('hidden') && !e.target.closest('.load-group')) {
    dd.classList.add('hidden');
  }
});

async function runWorkflow() {
  if (workflow.length === 0) return;

  const resultDiv = document.getElementById('workflow-result');
  const output = document.getElementById('workflow-output');
  resultDiv.classList.remove('hidden');
  output.innerHTML = '<em style="color:var(--text-muted)">Running workflow…</em>';

  try {
    const r = await fetch(API.workflow, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompts: workflow.map(s => s.prompt) })
    });
    const data = await r.json();

    output.innerHTML = data.results.map(r => {
      const label = r.intent_type || 'Step ' + r.step;
      if (!r.success) {
        return `<div style="margin-top:8px;padding:8px 12px;border-left:3px solid var(--error);background:var(--bg-secondary);border-radius:4px">
          <strong>Step ${r.step}: ${escapeHtml(r.prompt)}</strong><br>
          <span style="color:var(--error)">${escapeHtml(r.error || 'Failed')}</span>
        </div>`;
      }
      return `<div style="margin-top:8px;padding:8px 12px;border-left:3px solid var(--success);background:var(--bg-secondary);border-radius:4px">
        <strong>Step ${r.step}: ${escapeHtml(label)}</strong><br>
        <span style="color:var(--text-secondary);font-size:12px">${escapeHtml(r.description)}</span>
        <pre style="margin-top:4px;font-size:11px;color:var(--text-muted);white-space:pre-wrap">${escapeHtml(JSON.stringify(r.result, null, 2))}</pre>
      </div>`;
    }).join('');

    output.innerHTML += `<div style="margin-top:12px;font-size:12px;color:var(--text-muted)">
      ${data.successful}/${data.total_steps} steps succeeded
    </div>`;
  } catch (e) {
    output.innerHTML = `<div style="margin-top:8px;padding:8px 12px;border-left:3px solid var(--error);background:var(--bg-secondary);border-radius:4px">
      <span style="color:var(--error)">Network error — is the bridge running?</span>
    </div>`;
  }

  resultDiv.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

/* ── Init ── */
function init() {
  connectWs();
  updateDashboard();
  statusTimer = setInterval(updateDashboard, 5000);
  window.addEventListener('resize', () => requestAnimationFrame(drawChart));
}

document.addEventListener('DOMContentLoaded', init);

/* Redraw chart when tab becomes visible */
const observer = new MutationObserver(() => {
  if (document.getElementById('tab-dashboard').classList.contains('active')) {
    requestAnimationFrame(drawChart);
  }
});
observer.observe(document.getElementById('tab-dashboard'), { attributes: true, attributeFilter: ['class'] });
