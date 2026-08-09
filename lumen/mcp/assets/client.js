// Lumen inspector client. Vanilla JS, zero dependencies, works offline -
// the page is served by the in-app MCP server and polls it over POST /rpc.
//
// Panels:
//   * element tree   - lumen.snapshot_tree, 1 s poll, change flash
//   * detail         - lumen.inspect_entity on the selected node
//   * screenshot     - lumen.screenshot + selected-node rect overlay
//   * signals        - lumen.signals poll + lumen.set_signal writes
//   * events         - lumen.recent_messages tail for a chosen ring
//   * perf strip     - lumen.tick last_tick_micros sparkline

'use strict';

const $ = (id) => document.getElementById(id);

// -- rpc -----------------------------------------------------------------

let nextRpcId = 1;
async function rpc(method, params) {
  const body = { jsonrpc: '2.0', id: nextRpcId++, method };
  if (params !== undefined) body.params = params;
  const res = await fetch('/rpc', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const payload = await res.json();
  if (payload.error) throw new Error(payload.error.message || 'rpc error');
  return payload.result;
}

function setStatus(text, err) {
  const el = $('status');
  el.textContent = text;
  el.className = err ? 'statusbar err' : 'statusbar';
}

// -- state ---------------------------------------------------------------

const state = {
  selected: null,          // selected entity id (number)
  collapsed: new Set(),    // entity ids with folded children
  nodeFingerprints: new Map(), // id -> JSON string of node sans children (change flash)
  flatRows: [],            // visible rows in render order (for keyboard nav)
  filter: '',
  viewport: { w: 0, h: 0 },
  perf: [],                // last N last_tick_micros samples
  rects: new Map(),        // id -> {x,y,w,h} from the latest tree
  sigChangedAt: new Map(), // name -> last_changed_frame previously seen
  frame: 0,
  shotFresh: false,        // a capture has been taken at least once
};
const PERF_SAMPLES = 70;

// -- element tree --------------------------------------------------------

function nodeMatchesFilter(node, needle) {
  if (!needle) return true;
  const hay = [
    node.tag || '',
    node.lumen_id ? '#' + node.lumen_id : '',
    (node.classes || []).map((c) => '.' + c).join(' '),
    node.label || '',
    node.text || '',
    node.role || '',
    String(node.id),
  ].join(' ').toLowerCase();
  return hay.includes(needle);
}

// A node stays visible when it or any descendant matches.
function filterTree(node, needle) {
  const kids = (node.children || [])
    .map((c) => filterTree(c, needle))
    .filter(Boolean);
  if (nodeMatchesFilter(node, needle) || kids.length) {
    return { ...node, children: kids };
  }
  return null;
}

function nodeFingerprint(node) {
  const { children, ...rest } = node;
  return JSON.stringify(rest);
}

function renderTree(roots) {
  const treeEl = $('tree');
  const needle = state.filter.trim().toLowerCase();
  const shown = needle
    ? roots.map((r) => filterTree(r, needle)).filter(Boolean)
    : roots;

  const frag = document.createDocumentFragment();
  state.flatRows = [];
  state.rects.clear();

  const walk = (node, depth) => {
    state.rects.set(node.id, node.rect);
    const row = document.createElement('div');
    const hasKids = (node.children || []).length > 0;
    const folded = state.collapsed.has(node.id);
    row.className = 'node' + (state.selected === node.id ? ' selected' : '');
    row.style.paddingLeft = 6 + depth * 14 + 'px';
    row.dataset.id = String(node.id);

    const fp = nodeFingerprint(node);
    const prev = state.nodeFingerprints.get(node.id);
    if (prev !== undefined && prev !== fp) row.classList.add('changed');
    state.nodeFingerprints.set(node.id, fp);

    const twist = document.createElement('span');
    twist.className = 'twist';
    twist.textContent = hasKids ? (folded ? '\u25b8' : '\u25be') : '\u00b7';
    if (hasKids) {
      twist.onclick = (ev) => {
        ev.stopPropagation();
        toggleFold(node.id);
      };
    }
    row.appendChild(twist);

    const tag = document.createElement('span');
    tag.className = 'tag';
    tag.textContent = node.tag || node.role || 'node';
    row.appendChild(tag);

    if (node.lumen_id) {
      const lid = document.createElement('span');
      lid.className = 'lid';
      lid.textContent = '#' + node.lumen_id;
      row.appendChild(lid);
    }
    if (node.classes && node.classes.length) {
      const cls = document.createElement('span');
      cls.className = 'cls';
      cls.textContent = '.' + node.classes.join('.');
      row.appendChild(cls);
    }
    if (node.label) {
      const lbl = document.createElement('span');
      lbl.className = 'lbl';
      lbl.textContent = JSON.stringify(node.label);
      row.appendChild(lbl);
    }
    if (node.flags && node.flags !== '-') {
      const flags = document.createElement('span');
      flags.className = 'flags';
      flags.textContent = node.flags;
      row.appendChild(flags);
    }

    row.onclick = () => selectNode(node.id);
    frag.appendChild(row);
    state.flatRows.push(node.id);

    if (hasKids && !folded) {
      for (const child of node.children) walk(child, depth + 1);
    }
  };
  for (const root of shown) walk(root, 0);

  treeEl.replaceChildren(frag);
}

function toggleFold(id) {
  if (state.collapsed.has(id)) state.collapsed.delete(id);
  else state.collapsed.add(id);
  refreshTree();
}

function selectNode(id) {
  state.selected = id;
  for (const el of $('tree').children) {
    el.classList.toggle('selected', Number(el.dataset.id) === id);
  }
  refreshDetail();
  positionOverlay();
}

// -- detail panel --------------------------------------------------------

function fmtValue(v) {
  if (v === null || v === undefined) return '';
  if (typeof v === 'string') return v;
  return JSON.stringify(v, null, 1);
}

async function refreshDetail() {
  const id = state.selected;
  const detailEl = $('detail');
  if (id === null) return;
  try {
    const info = await rpc('lumen.inspect_entity', { id });
    $('detail-title').textContent =
      `Inspect - ${info.tag || 'entity'}` +
      (info.lumen_id ? ` #${info.lumen_id}` : ` (${id})`);
    const frag = document.createDocumentFragment();
    let any = false;
    for (const [k, v] of Object.entries(info)) {
      if (v === null || v === undefined) continue;
      if (Array.isArray(v) && v.length === 0) continue;
      any = true;
      const row = document.createElement('div');
      row.className = 'kv';
      const key = document.createElement('span');
      key.className = 'k';
      key.textContent = k;
      const val = document.createElement('span');
      val.className = 'v';
      val.textContent = fmtValue(v);
      row.append(key, val);
      frag.appendChild(row);
    }
    if (!any) {
      const e = document.createElement('div');
      e.className = 'empty';
      e.textContent = 'No recognised components.';
      frag.appendChild(e);
    }
    detailEl.replaceChildren(frag);
  } catch (e) {
    setStatus(`inspect failed: ${e.message}`, true);
  }
}

// -- screenshot + overlay ------------------------------------------------

async function refreshScreenshot() {
  try {
    const shot = await rpc('lumen.screenshot', {});
    if (!shot || !shot.available) {
      $('shot-meta').textContent = shot && shot.reason ? shot.reason : 'unavailable';
      return;
    }
    const img = $('shot-img');
    img.src = 'data:image/png;base64,' + shot.png_base64;
    $('shot-empty').style.display = 'none';
    $('shot-meta').textContent =
      `${shot.width}x${shot.height}px \u00b7 ${shot.source}`;
    state.shotFresh = true;
    img.onload = positionOverlay;
  } catch (e) {
    setStatus(`screenshot failed: ${e.message}`, true);
  }
}

// The overlay maps the selected node's LOGICAL-pixel rect onto the
// displayed image using percentages of the viewport's logical size, so it
// stays correct across dpr scaling and CSS max-width shrinking.
function positionOverlay() {
  const overlay = $('shot-overlay');
  const rect = state.selected !== null ? state.rects.get(state.selected) : null;
  const { w, h } = state.viewport;
  if (!state.shotFresh || !rect || !w || !h || rect.w <= 0 || rect.h <= 0) {
    overlay.style.display = 'none';
    return;
  }
  overlay.style.display = 'block';
  overlay.style.left = (rect.x / w) * 100 + '%';
  overlay.style.top = (rect.y / h) * 100 + '%';
  overlay.style.width = (rect.w / w) * 100 + '%';
  overlay.style.height = (rect.h / h) * 100 + '%';
}

// -- signals panel -------------------------------------------------------

function renderSignals(rows) {
  const body = $('signals');
  const frag = document.createDocumentFragment();
  for (const s of rows) {
    const tr = document.createElement('tr');
    const prev = state.sigChangedAt.get(s.name);
    if (prev !== undefined && prev !== s.last_changed_frame) tr.classList.add('changed');
    state.sigChangedAt.set(s.name, s.last_changed_frame);

    const name = document.createElement('td');
    name.className = 'sig-name';
    name.textContent = s.name;
    const value = document.createElement('td');
    value.textContent = s.value;
    value.title = s.value;
    const kind = document.createElement('td');
    kind.className = 'sig-kind';
    kind.textContent = s.kind;
    const tick = document.createElement('td');
    tick.className = 'sig-tick';
    tick.textContent = s.last_changed_frame ? '@' + s.last_changed_frame : '';
    tr.append(name, value, kind, tick);
    tr.onclick = () => {
      $('sw-name').value = s.name;
      $('sw-value').value = s.value;
      $('sw-value').focus();
      $('sw-value').select();
    };
    frag.appendChild(tr);
  }
  body.replaceChildren(frag);
}

async function writeSignal() {
  const name = $('sw-name').value.trim();
  const value = $('sw-value').value;
  if (!name) return;
  try {
    const out = await rpc('lumen.set_signal', { name, value });
    if (out.error) throw new Error(out.error);
    setStatus(out.summary || `set ${name}`);
    refresh();
  } catch (e) {
    setStatus(`set_signal failed: ${e.message}`, true);
  }
}

// -- events panel --------------------------------------------------------

function describeEvent(type, ev) {
  switch (type) {
    case 'ClickEvent':
      return `entity ${ev.entity} @ (${ev.position.x}, ${ev.position.y}) ${ev.button}`;
    case 'PointerPressed':
    case 'PointerReleased':
      return `(${ev.position.x}, ${ev.position.y}) ${ev.button}`;
    case 'PointerMoved':
      return `(${ev.position.x}, ${ev.position.y})`;
    case 'KeyPressed':
      return `${ev.key}${ev.repeat ? ' (repeat)' : ''}`;
    case 'KeyReleased':
      return ev.key;
    case 'MouseWheel':
      return `\u0394(${ev.delta.x}, ${ev.delta.y}) @ (${ev.position.x}, ${ev.position.y})`;
    case 'FocusedKey':
      return `entity ${ev.entity} <- ${ev.key}`;
    default:
      return JSON.stringify(ev);
  }
}

async function refreshEvents() {
  const type = $('ev-type').value;
  try {
    const events = await rpc('lumen.recent_messages', { type, max: 20 });
    const el = $('events');
    if (!Array.isArray(events) || events.length === 0) {
      el.innerHTML = '<div class="empty">No events on this ring.</div>';
      return;
    }
    const frag = document.createDocumentFragment();
    // newest last on the wire; show newest first.
    for (const ev of [...events].reverse()) {
      const row = document.createElement('div');
      row.className = 'ev-row';
      const b = document.createElement('b');
      b.textContent = type + ' ';
      row.appendChild(b);
      row.appendChild(document.createTextNode(describeEvent(type, ev)));
      frag.appendChild(row);
    }
    el.replaceChildren(frag);
  } catch (e) {
    setStatus(`events failed: ${e.message}`, true);
  }
}

// -- perf sparkline ------------------------------------------------------

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function renderSpark() {
  const canvas = $('perf-spark');
  const ctx = canvas.getContext('2d');
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  const samples = state.perf;
  if (samples.length < 2) return;
  const max = Math.max(...samples, 1);
  ctx.strokeStyle = cssVar('--accent-2') || '#5fd9e0';
  ctx.lineWidth = 1;
  ctx.beginPath();
  const n = samples.length;
  for (let i = 0; i < n; i++) {
    const x = (i / (PERF_SAMPLES - 1)) * (width - 2) + 1;
    const y = height - 2 - (samples[i] / max) * (height - 4);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.stroke();
}

// -- refresh loops -------------------------------------------------------

let latestTree = [];

async function refreshTree() {
  try {
    const out = await rpc('lumen.snapshot_tree', {});
    latestTree = out.tree || [];
    state.frame = out.frame || 0;
    renderTree(latestTree);
    positionOverlay();
  } catch (e) {
    throw e;
  }
}

async function refresh() {
  try {
    const [tick, resources, signals] = await Promise.all([
      rpc('lumen.tick'),
      rpc('lumen.resources'),
      rpc('lumen.signals', {}),
    ]);
    $('m-frame').textContent = String(tick.frame);
    $('m-tick').textContent = String(tick.last_tick_micros);
    state.perf.push(tick.last_tick_micros);
    if (state.perf.length > PERF_SAMPLES) state.perf.shift();
    renderSpark();

    if (resources.viewport && resources.viewport.size) {
      state.viewport = { w: resources.viewport.size.x, h: resources.viewport.size.y };
    }
    renderSignals(signals.signals || []);
    await refreshTree();
    await refreshEvents();
    if (state.selected !== null) await refreshDetail();
    if ($('shot-auto').checked) await refreshScreenshot();
    setStatus(
      `connected \u00b7 frame ${tick.frame} \u00b7 ${state.nodeFingerprints.size} nodes tracked`,
    );
  } catch (e) {
    setStatus(`offline: ${e.message}`, true);
  }
}

// -- keyboard ------------------------------------------------------------

function moveSelection(delta) {
  const rows = state.flatRows;
  if (!rows.length) return;
  const idx = rows.indexOf(state.selected);
  const next = idx === -1 ? 0 : Math.min(rows.length - 1, Math.max(0, idx + delta));
  selectNode(rows[next]);
  const el = [...$('tree').children].find(
    (r) => Number(r.dataset.id) === rows[next],
  );
  if (el) el.scrollIntoView({ block: 'nearest' });
}

document.addEventListener('keydown', (ev) => {
  const inField = ['INPUT', 'SELECT', 'TEXTAREA'].includes(document.activeElement.tagName);
  if (ev.key === 'Escape' && inField) {
    document.activeElement.blur();
    return;
  }
  if (inField) return;
  switch (ev.key) {
    case '/':
      ev.preventDefault();
      $('tree-filter').focus();
      break;
    case 'ArrowDown':
      ev.preventDefault();
      moveSelection(1);
      break;
    case 'ArrowUp':
      ev.preventDefault();
      moveSelection(-1);
      break;
    case 'ArrowLeft':
      if (state.selected !== null && !state.collapsed.has(state.selected)) {
        ev.preventDefault();
        state.collapsed.add(state.selected);
        refreshTree();
      }
      break;
    case 'ArrowRight':
      if (state.selected !== null && state.collapsed.has(state.selected)) {
        ev.preventDefault();
        state.collapsed.delete(state.selected);
        refreshTree();
      }
      break;
    case 'r':
      refreshScreenshot();
      break;
    case 's':
      ev.preventDefault();
      $('sw-name').focus();
      break;
  }
});

// -- wire-up -------------------------------------------------------------

$('tree-filter').addEventListener('input', (ev) => {
  state.filter = ev.target.value;
  renderTree(latestTree);
});
$('shot-refresh').onclick = refreshScreenshot;
$('sw-set').onclick = writeSignal;
$('sw-value').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') writeSignal();
});
$('ev-type').addEventListener('change', refreshEvents);
window.addEventListener('resize', positionOverlay);

refresh();
setInterval(refresh, 1000);
